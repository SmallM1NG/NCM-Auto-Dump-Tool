use notify::{Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{collections::HashSet, path::Path, sync::{mpsc::channel, Arc, atomic::{AtomicBool, Ordering}}, thread, time::Duration};

/// Wait until the file stops growing and can be opened exclusively.
///
/// NetEase Cloud Music creates the `.ncm` and then keeps writing it, so acting
/// on the create event alone would try to decode a partial file.
fn wait_until_stable(path: &Path, stop: &Arc<AtomicBool>) -> bool {
    let mut last: u64 = 0;
    for _ in 0..20 {
        if stop.load(Ordering::Relaxed) { return false; }
        let Ok(meta) = std::fs::metadata(path) else { return false };
        let len = meta.len();
        if len > 0 && len == last {
            // Confirm the writer is done by taking an exclusive handle.
            if std::fs::OpenOptions::new().read(true).write(true).open(path).is_ok() {
                return true;
            }
        }
        last = len;
        thread::sleep(Duration::from_millis(250));
    }
    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}

pub fn watch_download_dir<P, F>(path: P, stop: Arc<AtomicBool>, on_ncm: F) -> notify::Result<()>
where P: AsRef<Path>, F: Fn(String) + Send + 'static {
    let (tx, rx) = channel();
    let mut watcher = RecommendedWatcher::new(tx, NotifyConfig::default().with_poll_interval(Duration::from_millis(500)))?;
    watcher.watch(path.as_ref(), RecursiveMode::NonRecursive)?;
    thread::spawn(move || {
        let _watcher = watcher;
        let mut known = HashSet::new();
        while !stop.load(Ordering::Relaxed) {
            let result = match rx.recv_timeout(Duration::from_millis(500)) { Ok(v) => v, Err(_) => continue };
            if let Ok(Event { kind: EventKind::Create(_), paths, .. }) = result {
                for path in paths {
                    if !path.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("ncm")) { continue; }
                    let key = path.to_string_lossy().to_lowercase();
                    if !known.insert(key) { continue; }
                    if !path.is_file() { continue; }
                    // Only enqueue once the download has finished writing.
                    if !wait_until_stable(&path, &stop) { continue; }
                    on_ncm(path.to_string_lossy().into_owned());
                }
            }
        }
    });
    Ok(())
}

