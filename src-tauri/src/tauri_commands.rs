use crate::settings::{self, Config};
use crate::AppState;
use tauri::{State, Emitter, Manager};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::channel};

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Config { state.config.lock().expect("config lock poisoned").clone() }
#[tauri::command]
pub fn save_config(app: tauri::AppHandle, state: State<'_, AppState>, config: Config) -> Result<(), String> {
    let mut guard = state.config.lock().map_err(|e| e.to_string())?;
    let old = guard.clone();
    // `total_processed` is owned by the queue worker and `window` by the window
    // itself; neither is a user-editable setting, and the window may still hold
    // stale values that would roll the counter back or snap the window away.
    let mut config = config;
    config.total_processed = guard.total_processed;
    config.window = guard.window;
    settings::save(&config).map_err(|e| e.to_string())?;
    *guard = config.clone();
    let mut changed = Vec::new();
    macro_rules! diff { ($field:ident) => { if old.$field != config.$field { changed.push(format!("{}: {} → {}", stringify!($field), old.$field, config.$field)); } }; }
    diff!(download_dir); diff!(output_dir); diff!(theme); diff!(enable_notification); diff!(enable_sound); diff!(enable_tray_flash); diff!(minimal_metadata); diff!(embed_lrc); diff!(shred_mode); diff!(always_on_top); diff!(close_to_tray); diff!(silent_start);
    // `total_processed` is intentionally not part of the diff: it is not a
    // user-editable setting.
    let _ = &old;
    crate::logger::write(Some(&app), "INFO", if changed.is_empty() { "配置保存（无变化）".to_string() } else { format!("配置变更: {}", changed.join(", ")) });
    Ok(())
}
#[tauri::command]
pub fn set_always_on_top(window: tauri::Window, enabled: bool) -> Result<(), String> { window.set_always_on_top(enabled).map_err(|e| e.to_string()) }
/// Hide the window into the tray instead of closing the application.
/// Hide the window into the tray, remembering its geometry first.
#[tauri::command]
pub fn hide_window(window: tauri::Window, app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    // Hiding is not quitting, so persist the position here too: the app may run
    // for days in the tray and only be closed by the system.
    save_window_state(&app, &state);
    window.hide().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn clear_notification_registry(app: tauri::AppHandle) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // 清理当前及旧版本曾使用过的通知注册标识。
    let script = r#"$ids=@('NADT','com.nadt.app','com.nadt.ncmtool'); $paths=@('HKCU:\Software\Classes\AppUserModelId','HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Notifications\Settings'); foreach($p in $paths){ foreach($k in (Get-ChildItem -Path $p -ErrorAction SilentlyContinue | Where-Object { $ids -contains $_.PSChildName })){ Write-Output ($p + '\' + $k.PSChildName); Remove-Item -Path $k.PSPath -Recurse -Force -ErrorAction SilentlyContinue } }"#;
    crate::logger::write(Some(&app), "INFO", "开始清理通知注册表");
    let output = std::process::Command::new("powershell").args(["-NoProfile","-NonInteractive","-Command",script]).creation_flags(0x08000000).output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        crate::logger::write(Some(&app), "ERROR", "清理失败");
        return Err("清理通知注册表失败".into());
    }
    // Each removed key is reported so the log shows exactly what was touched.
    let removed: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if removed.is_empty() {
        crate::logger::write(Some(&app), "INFO", "没有找到需要清理的注册项");
        return Err("没有找到需要清理的注册项".into());
    }
    for item in &removed {
        crate::logger::write(Some(&app), "INFO", format!("已删除 {}", item));
    }
    crate::logger::write(Some(&app), "INFO", format!("清理完成，共 {} 项", removed.len()));
    Ok(())
}

/// Log lines produced before the window attached its listener.
///
/// A silent start enables monitoring while the window is still hidden, so the
/// UI must fetch that history instead of relying on live events only.
#[tauri::command]
pub fn get_logs() -> Vec<String> { crate::logger::history() }

/// Whether the monitor is currently active.
///
/// The UI asks for this on mount, because a silent start enables monitoring
/// automatically while the window is still hidden.
#[tauri::command]
pub fn is_monitoring(state: State<'_, AppState>) -> bool {
    state.monitor_stop.lock().ok().and_then(|g| g.as_ref().map(|f| !f.load(Ordering::Relaxed))).unwrap_or(false)
}

/// Report which folder the configured download path resolves to, if any.
///
/// Used by the settings UI to confirm a folder choice immediately, instead of
/// letting monitoring fail later.
#[tauri::command]
pub fn resolve_download_dir(path: String) -> Option<String> {
    resolve_download_dir_inner(&path).map(|p| p.to_string_lossy().into_owned())
}

/// Resolve the folder that actually holds downloaded NCM files.
///
/// The user may pick either the NetEase download root or the `VipSongsDownload`
/// folder inside it, so both are accepted: a path that already *is* the folder
/// is used as-is, and a path that merely contains it resolves to the child.
/// Returns `None` when neither exists, so callers can report it uniformly.
pub fn resolve_download_dir_inner(configured: &str) -> Option<std::path::PathBuf> {
    const INNER: &str = "VipSongsDownload";
    let path = std::path::PathBuf::from(configured);
    // Already pointing at the folder: use it directly instead of appending a
    // second copy, which would produce `VipSongsDownload\VipSongsDownload`.
    if path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(INNER)) && path.is_dir() {
        return Some(path);
    }
    let nested = path.join(INNER);
    // Only the selected VipSongsDownload folder or its parent root is valid.
    // Do not accept arbitrary existing directories, otherwise monitoring can
    // appear to start while watching the wrong location.
    if nested.is_dir() { return Some(nested); }
    None
}

#[tauri::command]
pub fn start_monitor(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    crate::logger::write(Some(&app), "INFO", "请求启动监控");
    if state.monitor_stop.lock().map_err(|e| e.to_string())?.as_ref().is_some_and(|flag| !flag.load(Ordering::Relaxed)) {
        crate::logger::write(Some(&app), "ERROR", "启动监控失败: 监控已经在运行");
        return Err("监控已经在运行".into());
    }
    let cfg = state.config.lock().map_err(|e| e.to_string())?.clone();
    let fail = |app: &tauri::AppHandle, msg: String| { crate::logger::write(Some(app), "ERROR", format!("启动监控失败: {}", msg)); msg };
    if cfg.download_dir.is_empty() || cfg.output_dir.is_empty() { return Err(fail(&app, "下载目录和输出目录不能为空".into())); }
    if !std::path::Path::new(&cfg.output_dir).is_dir() { return Err(fail(&app, format!("输出目录不存在: {}", cfg.output_dir))); }
    let watch_dir = match resolve_download_dir_inner(&cfg.download_dir) {
        Some(d) => d,
        None => return Err(fail(&app, format!("下载目录不存在或其中没有 VipSongsDownload 文件夹: {}", cfg.download_dir))),
    };
    let output = std::path::PathBuf::from(&cfg.output_dir);
    let stop = Arc::new(AtomicBool::new(false));
    *state.monitor_stop.lock().map_err(|e| e.to_string())? = Some(stop.clone());
    let notify_enabled = cfg.enable_notification;
    // Sound is a property of the notification itself: without notifications
    // there is nothing that could play a sound.
    let sound_enabled = cfg.enable_notification && cfg.enable_sound;
    // Remember what this session is watching so the stop toast reports the
    // directory that was actually monitored, not a later edit to the config.
    *state.session.lock().map_err(|e| e.to_string())? = Some(crate::Session {
        stop: stop.clone(),
        watch_dir: watch_dir.to_string_lossy().into_owned(),
        notify: notify_enabled,
        sound: sound_enabled,
    });
    let embed_lrc = cfg.embed_lrc;
    let minimal_metadata = cfg.minimal_metadata;
    if notify_enabled {
        crate::desktop_notify::send_notification("监控已启动", watch_dir.to_string_lossy().as_ref(), sound_enabled);
    }
    let shred_mode = cfg.shred_mode;
    // Both the watcher and manual drops feed the one shared queue, so only a
    // single file is ever processed at a time and the counters stay coherent.
    let queue_tx = state.queue_tx.lock().map_err(|e| e.to_string())?.clone()
        .ok_or_else(|| "队列未就绪".to_string())?;
    let pending = state.pending.clone();
    let processed = state.processed.clone();
    let sender_app = app.clone();
    crate::file_watcher::watch_download_dir(watch_dir.clone(), stop, move |input| {
        let depth = pending.fetch_add(1, Ordering::Relaxed) + 1;
        crate::logger::write(Some(&sender_app), "INFO", format!("新文件加入队列: {} (排队 {})",
            std::path::Path::new(&input).file_name().and_then(|x| x.to_str()).unwrap_or(&input), depth));
        let _ = sender_app.emit("queue-state", serde_json::json!({
            "state": "running", "queued": depth, "processed": processed.load(Ordering::Relaxed)
        }));
        let _ = queue_tx.send(input);
    }).map_err(|e| {
        crate::logger::write(None, "ERROR", format!("启动目录监控失败: {}", e));
        e.to_string()
    })?;
    crate::logger::write(Some(&app), "INFO", format!("监控已启动 | 监控目录: {} | 输出目录: {}", watch_dir.display(), output.display()));
    let mut enabled = Vec::new();
    if minimal_metadata { enabled.push("极简模式"); }
    if embed_lrc { enabled.push("写入 LRC 歌词文件"); }
    if shred_mode { enabled.push("删除原始文件"); }
    if !enabled.is_empty() { crate::logger::write(Some(&app), "INFO", format!("输出设置: {}", enabled.join("、"))); }
    Ok(())
}

/// Start the single work queue. Called once at startup.
///
/// Every job reads the current settings when it starts, so a config change
/// applies to the next file rather than to the one already running.
pub fn start_queue(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let mut slot = match state.queue_tx.lock() { Ok(g) => g, Err(_) => return };
    if slot.is_some() { return; }
    let (tx, rx) = channel::<String>();
    *slot = Some(tx);
    drop(slot);

    let app = app.clone();
    let pending = state.pending.clone();
    let processed = state.processed.clone();
    let busy = state.busy.clone();
    std::thread::spawn(move || {
        while let Ok(input) = rx.recv() {
            // The task has left the queue: decrement before doing any work so
            // the pending count always reflects what is still waiting.
            let depth = pending.load(Ordering::Relaxed).saturating_sub(1);
            pending.store(depth, Ordering::Relaxed);

            let state = app.state::<AppState>();
            let cfg = state.config.lock().map(|c| c.clone()).unwrap_or_default();
            let output = std::path::PathBuf::from(&cfg.output_dir);
            let filename = std::path::Path::new(&input).file_name().and_then(|x| x.to_str()).unwrap_or(&input).to_string();
            let done_so_far = processed.load(Ordering::Relaxed);

            if !output.is_dir() {
                crate::logger::write(Some(&app), "ERROR", format!("输出目录不存在，跳过 {}: {}", filename, output.display()));
                let _ = app.emit("queue-progress", serde_json::json!({
                    "state": "failed", "file": filename, "status": "输出目录不存在",
                    "progress": 0, "queued": depth, "processed": done_so_far
                }));
                continue;
            }

            busy.store(true, Ordering::Relaxed);
            let job = crate::pipeline::Job { input, filename: filename.clone(), pending: depth, processed: done_so_far };
            let result = crate::pipeline::process_job(&app, &cfg, &output, &job);
            busy.store(false, Ordering::Relaxed);

            if cfg.enable_notification {
                let event = if result.is_ok() { "处理完成" } else { "处理失败" };
                let detail = result.as_ref().map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string()).unwrap_or_else(|_| filename.clone());
                crate::desktop_notify::send_notification(event, &detail, cfg.enable_sound);
            }
            if let Err(ref e) = result {
                crate::logger::write(Some(&app), "ERROR", format!("处理失败 {}: {}", filename, e));
            }

            // Lifetime total, persisted so it survives restarts.
            let mut total = 0u64;
            if result.is_ok() {
                let state = app.state::<AppState>();
                let snapshot = state.config.lock().ok().map(|mut guard| {
                    guard.total_processed = guard.total_processed.saturating_add(1);
                    guard.clone()
                });
                if let Some(cfg) = snapshot {
                    total = cfg.total_processed;
                    let _ = crate::settings::save(&cfg);
                }
            }
            let done = if result.is_ok() { processed.fetch_add(1, Ordering::Relaxed) + 1 } else { processed.load(Ordering::Relaxed) };
            let _ = app.emit("queue-progress", serde_json::json!({
                "state": if result.is_ok() { "completed" } else { "failed" },
                "file": filename,
                "status": match &result { Ok(_) => "已完成".to_string(), Err(e) => format!("失败: {}", e) },
                "progress": if result.is_ok() { 100 } else { 0 },
                "queued": pending.load(Ordering::Relaxed),
                "processed": done,
                "total": total
            }));
        }
    });
}

/// Add NCM files to the work queue from a drag-and-drop.
///
/// Uses the same queue as the folder watcher, so a drop during monitoring is
/// processed in turn rather than in parallel. LRC files are accepted and
/// ignored here because each job picks up its own same-named `.lrc` from disk.
#[tauri::command]
pub fn enqueue_files(app: tauri::AppHandle, state: State<'_, AppState>, paths: Vec<String>) -> Result<usize, String> {
    let mut accepted = Vec::new();
    for p in paths {
        let path = std::path::Path::new(&p);
        let is_ncm = path.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("ncm"));
        if is_ncm && path.is_file() { accepted.push(p); }
    }
    if accepted.is_empty() {
        return Err("没有可处理的 .ncm 文件".into());
    }
    let tx = state.queue_tx.lock().map_err(|e| e.to_string())?.clone()
        .ok_or_else(|| "队列未就绪".to_string())?;
    let count = accepted.len();
    for path in &accepted {
        let depth = state.pending.fetch_add(1, Ordering::Relaxed) + 1;
        crate::logger::write(Some(&app), "INFO", format!("手动添加: {} (排队 {})",
            std::path::Path::new(path).file_name().and_then(|x| x.to_str()).unwrap_or(path), depth));
        tx.send(path.clone()).map_err(|e| e.to_string())?;
    }
    let _ = app.emit("queue-state", serde_json::json!({
        "state": "running",
        "queued": state.pending.load(Ordering::Relaxed),
        "processed": state.processed.load(Ordering::Relaxed),
    }));
    Ok(count)
}

/// Capture the window geometry so the next launch can restore it.
///
/// Called on every exit path, including hide-to-tray, so the size the user
/// last chose is what they get back.
pub fn save_window_state(app: &tauri::AppHandle, state: &State<'_, AppState>) {
    let Some(window) = app.get_webview_window("main") else { return };
    let scale = window.scale_factor().unwrap_or(1.0);
    if scale <= 0.0 { return; }
    let maximized = window.is_maximized().unwrap_or(false);
    let fullscreen = window.is_fullscreen().unwrap_or(false);

    // A maximised or fullscreen window reports the *screen* rect, not the
    // rectangle the user chose. Overwriting the saved size with it would make
    // the next launch open at that size, losing the real one, so only the
    // maximised flag is updated and the geometry is carried over untouched.
    if maximized || fullscreen {
        if let Ok(mut guard) = state.config.lock() {
            let mut geom = guard.window.unwrap_or(crate::settings::WindowState {
                x: 0, y: 0, width: 0.0, height: 0.0, maximized: true,
            });
            geom.maximized = maximized;
            guard.window = Some(geom);
            let snapshot = guard.clone();
            drop(guard);
            let _ = crate::settings::save(&snapshot);
        }
        return;
    }

    // Both values are read from the inner rect on purpose. `outer_position`
    // measures from the frame origin, which sits a shadow-width above and to
    // the left of the client area on Windows 11; pairing it with `inner_size`
    // mixes two rectangles and shifts the window a few pixels every restart.
    let Ok(pos) = window.inner_position() else { return };
    let Ok(size) = window.inner_size() else { return };
    let geom = crate::settings::WindowState {
        x: (pos.x as f64 / scale).round() as i32,
        y: (pos.y as f64 / scale).round() as i32,
        width: (size.width as f64 / scale).round(),
        height: (size.height as f64 / scale).round(),
        maximized: false,
    };
    if let Ok(mut guard) = state.config.lock() {
        guard.window = Some(geom);
        let snapshot = guard.clone();
        drop(guard);
        let _ = crate::settings::save(&snapshot);
    }
}

/// Shut the monitor down and terminate the process.
///
/// Tauri keeps running after its last window is closed (the tray lives on), so
/// closing the window would otherwise leave the monitor threads alive with no
/// visible way back. Every quit path funnels through here instead.
#[tauri::command]
pub fn quit_app(app: tauri::AppHandle, state: State<'_, AppState>) {
    save_window_state(&app, &state);
    stop_monitor_inner(&app, &state);
    // The worker checks the stop flag between files, so anything still queued is
    // dropped immediately; only the song being decoded right now can be in
    // flight. Wait briefly for it so the output is never truncated.
    if state.busy.load(Ordering::Relaxed) {
        crate::logger::write(Some(&app), "INFO", "正在等待当前文件处理完成...");
        for _ in 0..50 {
            if !state.busy.load(Ordering::Relaxed) { break; }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if state.busy.load(Ordering::Relaxed) {
            crate::logger::write(Some(&app), "ERROR", "等待超时 (5 秒)，强制退出");
        }
    }
    crate::logger::write(Some(&app), "INFO", "程序退出");
    app.exit(0);
}

fn stop_monitor_inner(_app: &tauri::AppHandle, state: &State<'_, AppState>) {
    if let Ok(mut guard) = state.monitor_stop.lock() {
        if let Some(flag) = guard.take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
    if let Ok(mut s) = state.session.lock() { *s = None; }
}

#[tauri::command]
pub fn stop_monitor(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    crate::logger::write(Some(&app), "INFO", "请求停止监控");
    // The snapshot is the session that is actually running, so the stop toast
    // matches the start toast even if the config changed meanwhile.
    let session = state.session.lock().map_err(|e| e.to_string())?.take();
    let running = state.monitor_stop.lock().map_err(|e| e.to_string())?.take();
    match running {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            crate::logger::write(Some(&app), "INFO", "监控已停止 (当前正在处理的文件会继续完成，等待中的队列已丢弃)");
            if let Some(s) = session {
                if s.notify {
                    crate::desktop_notify::send_notification("监控已停止", &s.watch_dir, s.sound);
                }
            }
        }
        None => {},
    }
    Ok(())
}
