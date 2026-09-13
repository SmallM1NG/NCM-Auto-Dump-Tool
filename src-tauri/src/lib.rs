pub mod ncm_decrypt;
pub mod settings;
pub mod file_watcher;
pub mod audio_tags;
pub mod logger;
pub mod desktop_notify;
pub mod pipeline;
pub mod tauri_commands;

use settings::Config;
use std::sync::{Mutex, Arc, atomic::AtomicBool};
use tauri::menu::{MenuBuilder, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::Manager;

/// Settings captured when monitoring starts.
///
/// The stop notification reports the directory that was actually being watched,
/// which may differ from the config if the user edited it mid-session.
pub struct Session {
    pub stop: Arc<AtomicBool>,
    pub watch_dir: String,
    pub notify: bool,
    pub sound: bool,
}

pub struct AppState {
    pub config: Mutex<Config>,
    pub monitor_stop: Mutex<Option<Arc<AtomicBool>>>,
    /// Snapshot of the running monitor session, if any.
    pub session: Mutex<Option<Session>>,
    /// Set while the worker is decoding a file, so quitting can wait for it
    /// instead of leaving a truncated output behind.
    pub busy: Arc<AtomicBool>,
    /// The single work queue. Both the watcher and manual drops push here, and
    /// one worker drains it, so only one file is ever processed at a time.
    pub queue_tx: Mutex<Option<std::sync::mpsc::Sender<String>>>,
    /// Files waiting in the queue.
    pub pending: Arc<std::sync::atomic::AtomicUsize>,
    /// Files finished successfully in this session.
    pub processed: Arc<std::sync::atomic::AtomicUsize>,
}

/// Minimum window size in logical pixels (enforced on drag resize).
const MIN_W: f64 = 420.0;
const MIN_H: f64 = 700.0;

/// Window size used the first time the app runs, centred on the primary monitor.
const DEFAULT_W: f64 = 420.0;
const DEFAULT_H: f64 = 700.0;

/// Whether a saved window rectangle still overlaps a connected monitor.
///
/// Guards against restoring a position from a display that is no longer
/// attached, which would otherwise open the window off-screen where the user
/// cannot reach it.
fn start_tray_flash(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut hidden = false;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let state = app.state::<AppState>();
            let enabled = state.config.lock().map(|c| c.enable_tray_flash).unwrap_or(false);
            let busy = state.busy.load(std::sync::atomic::Ordering::Relaxed);
            let waiting = state.pending.load(std::sync::atomic::Ordering::Relaxed);
            let active = busy || waiting > 0;
            let Some(tray) = app.tray_by_id("main") else { continue };
            let tooltip = format!("等待处理：{} 个", waiting);
            let _ = tray.set_tooltip(Some(tooltip));
            if !enabled || !active {
                if hidden {
                    if let Some(icon) = crate::desktop_notify::load_icon() {
                        let _ = tray.set_icon(Some(icon));
                    }
                    hidden = false;
                }
                continue;
            }
            if hidden {
                if let Some(icon) = crate::desktop_notify::load_icon() {
                    let _ = tray.set_icon(Some(icon));
                }
            } else {
                if let Some(icon) = crate::desktop_notify::load_dimmed_icon() {
                    let _ = tray.set_icon(Some(icon));
                }
            }
            hidden = !hidden;
        }
    });
}

fn position_is_visible(app: &tauri::AppHandle, x: i32, y: i32, w: f64, h: f64) -> bool {
    let Ok(monitors) = app.available_monitors() else { return false };
    monitors.iter().any(|m| {
        let pos = m.position();
        let size = m.size();
        let scale = m.scale_factor();
        // Convert the monitor rect to logical pixels to match the saved values.
        let left = pos.x as f64 / scale;
        let top = pos.y as f64 / scale;
        let right_edge = left + size.width as f64 / scale;
        let bottom_edge = top + size.height as f64 / scale;
        // Require some of the window to land on this monitor.
        let win_right = x as f64 + w;
        let win_bottom = y as f64 + h;
        win_right > left && (x as f64) < right_edge && win_bottom > top && (y as f64) < bottom_edge
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial = settings::load();
    let silent_start = initial.silent_start;
    let always_on_top = initial.always_on_top;
    let saved_window = initial.window;
    tauri::Builder::default()
        .manage(AppState {
            config: Mutex::new(initial),
            monitor_stop: Mutex::new(None),
            session: Mutex::new(None),
            busy: Arc::new(AtomicBool::new(false)),
            queue_tx: Mutex::new(None),
            pending: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            processed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            if let Err(e) = logger::init() { eprintln!("NADT: {e}"); }
            logger::write(Some(app.handle()), "INFO", format!("NADT 启动 | 数据目录: {}",
                settings::data_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| "未知".into())));
            // One queue serves both the watcher and manual drops.
            tauri_commands::start_queue(app.handle());
            // Attribute toasts to NADT instead of the host process.
            desktop_notify::register_app_id();
            // Build the window ourselves: a config-declared window is created
            // through the runtime's message loop, which is not up yet inside
            // `setup`, so every call on it fails with `FailedToReceiveMessage`
            // (including `show()`, which still returns `Ok`).
            //
            // Created *invisible* on purpose. Geometry is applied while the
            // window is off-screen, so the first frame the user ever sees is
            // already at the saved position and size. Creating it visible and
            // moving it afterwards is what produced a blank window at the
            // default position that then jumped.
            let window = tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
                .data_directory(settings::data_dir().ok().and_then(|data| data.parent().map(|root| root.join("webview"))).unwrap_or_else(|| std::path::PathBuf::from("webview")))
                .title("NADT")
                .inner_size(saved_window.map(|w| w.width).unwrap_or(DEFAULT_W), saved_window.map(|w| w.height).unwrap_or(DEFAULT_H))
                .min_inner_size(MIN_W, MIN_H)
                .resizable(true)
                .decorations(false)
                .visible(false)
                .always_on_top(always_on_top)
                .build()?;
            // 无边框窗口的最小尺寸存在一个容易忽略的差异：Tao 的
            // `min_inner_size` 在 Windows 上最终会参与外框尺寸限制，而
            // 自建标题栏属于客户区的一部分。直接限制 420x700 时，实际
            // 可见窗口可能缩到约 406x692。把外框与客户区的差值补回去，
            // 确保用户看到的窗口不会小于 420x700。
            {
                let scale = window.scale_factor().unwrap_or(1.0);
                if scale > 0.0 {
                    if let (Ok(outer), Ok(inner)) = (window.outer_size(), window.inner_size()) {
                        let frame_w = (outer.width as f64 - inner.width as f64) / scale;
                        let frame_h = (outer.height as f64 - inner.height as f64) / scale;
                        let min_w = (MIN_W + frame_w).max(MIN_W);
                        let min_h = (MIN_H + frame_h).max(MIN_H);
                        let _ = window.set_min_size(Some(tauri::LogicalSize::new(min_w, min_h)));
                    }
                }
            }
            // Restore the last position, or centre on first run. A stored
            // position that no longer lands on any monitor (the display was
            // unplugged or rearranged) falls back to centring, so the window
            // can never open off-screen.
            match saved_window {
                Some(w) if position_is_visible(app.handle(), w.x, w.y, w.width, w.height) => {
                    // Size first, then position: a resize can re-centre the
                    // window, so the position is applied last and wins.
                    let _ = window.set_size(tauri::LogicalSize::new(w.width, w.height));
                    let _ = window.set_position(tauri::LogicalPosition::new(w.x, w.y));
                    if w.maximized { let _ = window.maximize(); }
                }
                Some(w) => {
                    let _ = window.center();
                    logger::write(Some(app.handle()), "INFO", format!(
                        "窗口位置 ({}, {}) 不在任何显示器内，改为居中显示", w.x, w.y));
                }
                None => {
                    let _ = window.center();
                    logger::write(Some(app.handle()), "INFO", "首次启动，窗口居中显示");
                }
            }
            // The single moment the window becomes visible, and only after its
            // geometry is final. A silent start skips it entirely and the app
            // lives in the tray from the outset.
            if !silent_start { let _ = window.show(); }
            let open_item = MenuItemBuilder::with_id("open", "打开主界面").build(app)?;
            let download_item = MenuItemBuilder::with_id("download", "打开下载目录").build(app)?;
            let output_item = MenuItemBuilder::with_id("output", "打开输出目录").build(app)?;
            let quit_item = MenuItemBuilder::with_id("quit", "退出程序").build(app)?;
            let menu = MenuBuilder::new(app)
                .items(&[&open_item, &download_item, &output_item, &PredefinedMenuItem::separator(app)?, &quit_item])
                .build()?;
            let mut tray = TrayIconBuilder::with_id("main")
                .menu(&menu).on_menu_event(|app, event| {
                match event.id.as_ref() {
                    "quit" => tauri_commands::quit_app(app.clone(), app.state::<AppState>()),
                    "download" => {
                        // Open the folder that actually holds the downloads, so a
                        // configured root still lands on VipSongsDownload.
                        let configured = app.state::<AppState>().config.lock().ok().map(|c| c.download_dir.clone()).unwrap_or_default();
                        let target = tauri_commands::resolve_download_dir_inner(&configured).unwrap_or_else(|| std::path::PathBuf::from(configured));
                        if !target.as_os_str().is_empty() { let _ = std::process::Command::new("explorer").arg(target).spawn(); }
                    }
                    "output" => { let path = app.state::<AppState>().config.lock().ok().map(|c| c.output_dir.clone()).unwrap_or_default(); if !path.is_empty() { let _ = std::process::Command::new("explorer").arg(path).spawn(); } }
                    "open" => { if let Some(window) = app.get_webview_window("main") { let _ = window.show(); let _ = window.unminimize(); let _ = window.set_focus(); } }
                    _ => {}
                }
            });
            if let Some(icon) = desktop_notify::load_icon() {
                tray = tray.icon(icon);
            }
            tray.build(app)?;
            start_tray_flash(app.handle());
            // Silent start also enables monitoring right away. start_monitor
            // logs the outcome itself, so only failures need a note here.
            if silent_start {
                let state = app.state::<AppState>();
                if let Err(e) = tauri_commands::start_monitor(app.handle().clone(), state) {
                    logger::write(Some(app.handle()), "ERROR", format!("静默启动: 自动启用监控失败: {}", e));
                }
            }
            Ok(())
        })
        // Any window close must terminate the process, otherwise the tray and
        // monitor threads would keep running with no window to return to.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                tauri_commands::quit_app(window.app_handle().clone(), window.state::<AppState>());
            }
        })
        .invoke_handler(tauri::generate_handler![tauri_commands::get_config, tauri_commands::save_config, tauri_commands::set_always_on_top, tauri_commands::hide_window, tauri_commands::clear_notification_registry, tauri_commands::is_monitoring, tauri_commands::get_logs, tauri_commands::enqueue_files, tauri_commands::resolve_download_dir, tauri_commands::start_monitor, tauri_commands::stop_monitor, tauri_commands::quit_app])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
