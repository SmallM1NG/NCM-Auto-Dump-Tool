use std::{collections::VecDeque, fs::{self, OpenOptions}, io::Write, path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Emitter};

static FILE_LOCK: Mutex<()> = Mutex::new(());
/// Recent lines kept in memory so a window that attaches its listener late
/// (notably after a silent start) can still show what already happened.
static HISTORY: Mutex<Option<VecDeque<String>>> = Mutex::new(None);
const HISTORY_LIMIT: usize = 200;

/// Resolved once at startup so every write targets the same file, and a failure
/// is reported rather than silently falling back to a relative path.
static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init() -> Result<(), String> {
    let path = crate::settings::data_dir().map_err(|e| format!("无法定位数据目录: {e}"))?.join("nadt.log");
    if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|e| format!("无法创建数据目录 {}: {e}", parent.display()))?; }
    OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("无法写入日志文件 {}: {e}", path.display()))?;
    if let Ok(mut h) = HISTORY.lock() { *h = Some(VecDeque::with_capacity(HISTORY_LIMIT)); }
    if let Ok(mut p) = LOG_PATH.lock() { *p = Some(path); }
    Ok(())
}

/// Path of the active log file, if `init` succeeded.
pub fn path() -> Option<PathBuf> { LOG_PATH.lock().ok().and_then(|p| p.clone()) }

pub fn write(app: Option<&AppHandle>, level: &str, message: impl AsRef<str>) {
    let line = format!("[{}] [{}] {}", timestamp(), level, message.as_ref());
    // Without a resolved path the app is running without a log; the in-memory
    // history still works so the window can show what happened.
    if let Some(path) = path() {
        if let Ok(_guard) = FILE_LOCK.lock() {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{}", line);
            }
        }
    }
    if let Ok(mut h) = HISTORY.lock() {
        if let Some(q) = h.as_mut() {
            if q.len() == HISTORY_LIMIT { q.pop_front(); }
            q.push_back(line.clone());
        }
    }
    if let Some(app) = app { let _ = app.emit("log-message", line); }
}

/// Lines emitted so far, oldest first. Used to backfill the log view on mount.
pub fn history() -> Vec<String> {
    HISTORY.lock().ok().and_then(|h| h.as_ref().map(|q| q.iter().cloned().collect())).unwrap_or_default()
}

fn timestamp() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

