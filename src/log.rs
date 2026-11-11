use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Rotate before the active log grows past this size…
pub const MAX_LOG_BYTES: u64 = 1_048_576;
/// …keeping this many rotated copies (log.1.txt … log.N.txt).
pub const KEEP_ROTATED: u32 = 5;

pub fn log_path(root: &Path) -> PathBuf {
    root.join("log.txt")
}

/// Path of the crash marker: created when the daemon starts, removed on a
/// clean exit. Its presence at the next start means the previous run died.
pub fn crash_flag_path(root: &Path) -> PathBuf {
    root.join("crash.flag")
}

/// Human-readable record of crashes (guard-written exit codes, panics).
pub fn crash_report_path(root: &Path) -> PathBuf {
    root.join("crash.txt")
}

pub fn mark_running(root: &Path) {
    let _ = std::fs::write(crash_flag_path(root), b"");
}

pub fn clear_running(root: &Path) {
    let _ = std::fs::remove_file(crash_flag_path(root));
}

/// True when the previous run did not exit cleanly.
pub fn previous_run_crashed(root: &Path) -> bool {
    crash_flag_path(root).exists()
}

/// Append one line to crash.txt (bounded to avoid an unbounded file).
pub fn append_crash_report(root: &Path, line: &str) {
    let path = crash_report_path(root);
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > 256 * 1024 {
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{at}] {line}");
    }
}

/// Whether crash.txt was written within `seconds`: the guard reports a crash
/// right before restarting the daemon, so a fresh report means a real crash and
/// not an ordinary shutdown.
pub fn crash_report_recent(root: &Path, seconds: u64) -> bool {
    let Ok(meta) = std::fs::metadata(crash_report_path(root)) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|age| age.as_secs() <= seconds)
        .unwrap_or(false)
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
    rotate_files(path);
    true
}

fn rotate_files(path: &Path) {
    let Some(root) = path.parent() else { return };
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("log.txt");
    for i in (1..KEEP_ROTATED).rev() {
        let from = root.join(format!("{name}.{i}"));
        let to = root.join(format!("{name}.{}", i + 1));
        let _ = std::fs::rename(&from, &to);
    }
    let _ = std::fs::rename(path, root.join(format!("{name}.1")));
}

/// Start logging for the daemon. The previous log is *kept* as `log.txt.1`:
/// after a crash it holds the last lines (including a panic message), and
/// truncating it erased exactly the evidence needed to explain the crash.
pub fn init(root: &std::path::Path) {
    init_with(root, true);
}

/// For the UI process: appends to the daemon's log, never rotates it.
pub fn init_child(root: &std::path::Path) {
    init_with(root, false);
}

fn init_with(root: &std::path::Path, own_file: bool) {
    let _ = LOG_PATH.set(log_path(root));
    if own_file {
        rotate_files(LOG_PATH.get().unwrap());
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(LOG_PATH.get().unwrap())
    {
        let _ = writeln!(
            file,
            "[{}] --- app started (pid {}, v{}) ---",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
            std::process::id(),
            env!("CARGO_PKG_VERSION")
        );
    }
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("PANIC: {info}"));
        default_hook(info);
    }));
}

fn who() -> String {
    match std::thread::current().name() {
        Some(name) => name.to_string(),
        None => format!("{:?}", std::thread::current().id()),
    }
}

pub fn log(msg: &str) {
    let Some(path) = LOG_PATH.get() else { return };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{at}] [{}] {msg}", who());
    }
}
