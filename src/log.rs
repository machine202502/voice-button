use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Rotate before the active log grows past this size…
pub const MAX_LOG_BYTES: u64 = 1_048_576;
/// …keeping this many rotated copies (log.1.txt … log.N.txt).
pub const KEEP_ROTATED: u32 = 3;

pub fn log_path(root: &Path) -> PathBuf {
    root.join("log.txt")
}

/// Rotate the log files when the active log exceeds `MAX_LOG_BYTES`:
/// log.txt → log.1.txt, log.1.txt → log.2.txt and so on, dropping the oldest.
/// Returns true when a rotation happened. Called by the daemon (throttled);
/// writers open the file per line, so they transparently follow the rename.
pub fn rotate_if_needed() -> bool {
    let Some(path) = LOG_PATH.get() else { return false };
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_LOG_BYTES {
        return false;
    }
    let Some(root) = path.parent() else { return false };
    for i in (1..KEEP_ROTATED).rev() {
        let from = root.join(format!("log.{i}.txt"));
        let to = root.join(format!("log.{}.txt", i + 1));
        let _ = std::fs::rename(&from, &to);
    }
    let _ = std::fs::rename(path, root.join("log.1.txt"));
    true
}

pub fn init(root: &std::path::Path) {
    init_with(root, true);
}

/// For the UI process: keeps the daemon's log instead of truncating it.
pub fn init_child(root: &std::path::Path) {
    init_with(root, false);
}

fn init_with(root: &std::path::Path, truncate: bool) {
    let _ = LOG_PATH.set(log_path(root));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(truncate)
        .append(!truncate)
        .open(LOG_PATH.get().unwrap())
    {
        let _ = writeln!(file, "app started (truncate={truncate})");
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("PANIC: {info}"));
        default_hook(info);
    }));
}

pub fn log(msg: &str) {
    let Some(path) = LOG_PATH.get() else { return };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{at}] {msg}");
    }
}
