//! File-based IPC between the daemon and the UI process. Everything lives in
//! the app data directory so both processes already know the paths, and all
//! writes are atomic (temp file + rename) so readers never see torn content.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Serializable mirror of `crate::state::DownloadProgress` (the in-process
/// version carries a cancellation flag that cannot cross process borders).
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct DownloadSnapshot {
    pub model: String,
    pub file: usize,
    pub files: usize,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub done_bytes: u64,
    pub done_total: u64,
    pub done: bool,
    pub error: Option<String>,
}

pub fn download_request_path(root: &Path) -> PathBuf {
    root.join("download.request")
}

pub fn download_cancel_path(root: &Path) -> PathBuf {
    root.join("download.cancel")
}

pub fn download_state_path(root: &Path) -> PathBuf {
    root.join("download.state.json")
}

pub fn write_download_request(root: &Path, model_id: &str) {
    atomic_write(download_request_path(root), model_id.as_bytes());
}

/// Returns the requested model id and consumes the request file.
pub fn take_download_request(root: &Path) -> Option<String> {
    let path = download_request_path(root);
    let content = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let id = content.trim().to_string();
    (!id.is_empty()).then_some(id)
}

pub fn request_cancel(root: &Path) {
    let _ = std::fs::File::create(download_cancel_path(root));
}

pub fn cancel_requested(root: &Path) -> bool {
    download_cancel_path(root).exists()
}

pub fn clear_cancel(root: &Path) {
    let _ = std::fs::remove_file(download_cancel_path(root));
}

pub fn write_download_state(root: &Path, snap: &DownloadSnapshot) {
    if let Ok(text) = serde_json::to_string(snap) {
        atomic_write(download_state_path(root), text.as_bytes());
    }
}

pub fn read_download_state(root: &Path) -> Option<DownloadSnapshot> {
    let text = std::fs::read_to_string(download_state_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn remove_download_state(root: &Path) {
    let _ = std::fs::remove_file(download_state_path(root));
}

// ---------------------------------------------------------------------------
// Recording status (daemon → UI), so the UI can lock model/backend switching
// while recording or transcribing.

pub fn status_path(root: &Path) -> PathBuf {
    root.join("status.txt")
}

/// While the Settings window captures a shortcut, the daemon releases its own
/// global hotkey so the very combination being typed is not swallowed by it.
pub fn hotkey_capture_path(root: &Path) -> PathBuf {
    root.join("hotkey.capture")
}

pub fn set_hotkey_capture(root: &Path, active: bool) {
    let path = hotkey_capture_path(root);
    if active {
        let _ = std::fs::File::create(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
}

pub fn hotkey_capture_active(root: &Path) -> bool {
    hotkey_capture_path(root).exists()
}
/// The daemon watches the physical keyboard while the Settings window captures a
/// shortcut and writes the result here; the window may not hold keyboard focus.
pub fn hotkey_result_path(root: &Path) -> PathBuf {
    root.join("hotkey.result")
}

pub fn write_hotkey_result(root: &Path, text: &str) {
    atomic_write(hotkey_result_path(root), text.as_bytes());
}

pub fn take_hotkey_result(root: &Path) -> Option<String> {
    let path = hotkey_result_path(root);
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}
pub fn write_status(root: &Path, status: crate::state::Status) {
    let text = match status {
        crate::state::Status::Idle => "idle",
        crate::state::Status::Recording => "recording",
        crate::state::Status::Transcribing => "transcribing",
        crate::state::Status::NoModel => "nomodel",
    };
    atomic_write(status_path(root), text.as_bytes());
}

pub fn read_status(root: &Path) -> Option<crate::state::Status> {
    let text = std::fs::read_to_string(status_path(root)).ok()?;
    Some(match text.trim() {
        "recording" => crate::state::Status::Recording,
        "transcribing" => crate::state::Status::Transcribing,
        "nomodel" => crate::state::Status::NoModel,
        _ => crate::state::Status::Idle,
    })
}

// ---------------------------------------------------------------------------
// Live (streaming) transcription, published by the daemon while recording so
// the UI can show words as they are spoken.

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct LiveView {
    pub text: String,
    /// Recording start, milliseconds since UNIX epoch (for elapsed time).
    pub started: i64,
}

pub fn live_path(root: &Path) -> PathBuf {
    root.join("live.json")
}

pub fn write_live(root: &Path, view: &LiveView) {
    if let Ok(text) = serde_json::to_string(view) {
        atomic_write(live_path(root), text.as_bytes());
    }
}

pub fn read_live(root: &Path) -> Option<LiveView> {
    let text = std::fs::read_to_string(live_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn clear_live(root: &Path) {
    let _ = std::fs::remove_file(live_path(root));
}

// ---------------------------------------------------------------------------
// Recording toggle requests from the UI.

pub fn record_request_path(root: &Path) -> PathBuf {
    root.join("record.request")
}

pub fn write_record_request(root: &Path) {
    atomic_write(record_request_path(root), b"toggle");
}

/// Returns true once per UI request and consumes it.
pub fn take_record_request(root: &Path) -> bool {
    let path = record_request_path(root);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        true
    } else {
        false
    }
}

/// Write via a temp file + rename so concurrent readers never see a partial
/// file.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8]) {
    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
