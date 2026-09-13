use serde::{Deserialize, Serialize};
use std::{fs, io, path::PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub download_dir: String,
    pub output_dir: String,
    pub enable_notification: bool,
    pub enable_sound: bool,
    pub enable_tray_flash: bool,
    /// Write only Title, Artist and cover art; drop every other tag.
    #[serde(default)]
    pub minimal_metadata: bool,
    pub embed_lrc: bool,
    pub shred_mode: bool,
    pub first_run: bool,
    /// Keep the main window above other windows.
    #[serde(default)]
    pub always_on_top: bool,
    /// Close button hides the window into the tray instead of exiting.
    #[serde(default)]
    pub close_to_tray: bool,
    /// Start hidden in the tray and begin monitoring immediately.
    #[serde(default)]
    pub silent_start: bool,
    /// 主题模式：dark、light 或 system。
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Lifetime count of NCM files decoded successfully.
    #[serde(default)]
    pub total_processed: u64,
    /// Remembered window geometry, captured on close and restored on start.
    /// `None` until the window has been moved or resized at least once, which
    /// is what makes the very first launch open centred.
    #[serde(default)]
    pub window: Option<WindowState>,
}

/// Last known window geometry, in logical pixels.
///
/// `x`/`y` are the **client area** origin, matching the basis `width`/`height`
/// are measured on: both come from the inner rect, never the outer one. Saving
/// an outer position against an inner size mixes two different rectangles and
/// makes the window creep by the frame/shadow thickness on every restart.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: f64,
    pub height: f64,
    /// Restored before the window is first shown, so a maximised window does
    /// not appear at its normal size and then visibly jump to full screen.
    #[serde(default)]
    pub maximized: bool,
}

fn default_theme() -> String { "system".to_string() }

impl Default for Config {
    fn default()->Self {
        let output_dir = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Downloads")
            .join("NADT Export");
        Self { download_dir:String::new(), output_dir:output_dir.to_string_lossy().into_owned(), enable_notification:true, enable_sound:true, enable_tray_flash:true, minimal_metadata:false, embed_lrc:false, shred_mode:false, first_run:true, always_on_top:false, close_to_tray:false, silent_start:false, theme:default_theme(), total_processed:0, window:None }
    }
}

/// Directory that holds runtime data (`settings.json`, `NADT.png`, `NADT.ico`,
/// `nadt.log`).
///
/// Portable by design: the folder sits next to the executable, so the whole app
/// can be moved or run from a USB stick and keeps its configuration, log and
/// icons together. The working directory is deliberately *not* used — launching
/// from a shortcut, a terminal or Explorer would each give a different one.
///
/// During development the binary lives in `src-tauri/target/<profile>/`, so the
/// search walks up until it finds the project root (the directory holding
/// `data/`, or a sibling `src-tauri`), falling back to the executable's folder.
pub fn data_dir() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe_dir = exe.parent().unwrap_or_else(|| std::path::Path::new("."));
    Ok(resolve_root(exe_dir).join("data"))
}

/// Walk up from the executable until the project root is recognisable.
///
/// A shipped build has no `src-tauri` parent, so this returns the executable's
/// own directory and `data/` sits right beside the binary.
fn resolve_root(exe_dir: &std::path::Path) -> PathBuf {
    let mut dir = exe_dir.to_path_buf();
    for _ in 0..6 {
        // Shipped layout: data/ already next to the exe.
        if dir.join("data").is_dir() { return dir; }
        // Development layout: the exe is under src-tauri/target/<profile>/.
        if dir.join("src-tauri").is_dir() {
            if let Some(parent) = dir.parent() { return parent.to_path_buf(); }
        }
        match dir.parent() { Some(p) => dir = p.to_path_buf(), None => break }
    }
    exe_dir.to_path_buf()
}

pub fn config_path() -> io::Result<PathBuf> { Ok(data_dir()?.join("settings.json")) }

/// Load the persisted settings, creating and saving a default file when it is
/// missing or unreadable so the app always starts from a consistent state.
pub fn load() -> Config {
    let Ok(path) = config_path() else { return Config::default() };
    if let Ok(raw) = fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<Config>(&raw) {
            ensure_output_dir(&cfg);
            return cfg;
        }
    }
    let cfg = Config::default();
    ensure_output_dir(&cfg);
    let _ = save(&cfg);
    cfg
}

/// Ensure the configured output directory exists before the UI presents it as
/// the default destination. Creating the settings file alone only creates the
/// `data` directory, so the old first-run flow displayed a path that did not
/// actually exist yet.
fn ensure_output_dir(cfg: &Config) {
    if !cfg.output_dir.trim().is_empty() {
        let _ = fs::create_dir_all(&cfg.output_dir);
    }
}

pub fn save(cfg:&Config)->io::Result<()> { let p=config_path()?; if let Some(parent)=p.parent(){fs::create_dir_all(parent)?;} fs::write(p, serde_json::to_vec_pretty(cfg)?) }
