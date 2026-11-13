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
// Voice Button API server status (daemon → UI), so the API tab can show
// running/stopped and the address that is actually bound.

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct ApiSnapshot {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub error: Option<String>,
}

impl From<ApiSnapshot> for crate::state::ApiStatus {
    fn from(s: ApiSnapshot) -> Self {
        Self {
            running: s.running,
            host: s.host,
            port: s.port,
            error: s.error,
        }
    }
}

pub fn api_state_path(root: &Path) -> PathBuf {
    root.join("api.state.json")
}

pub fn write_api_state(root: &Path, snap: &ApiSnapshot) {
    if let Ok(text) = serde_json::to_string(snap) {
        atomic_write(api_state_path(root), text.as_bytes());
    }
}

pub fn read_api_state(root: &Path) -> Option<ApiSnapshot> {
    let text = std::fs::read_to_string(api_state_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

// ---------------------------------------------------------------------------
// The phone server (daemon → UI): whether it listens, on which port, where the
// QR points, and whether the one paired phone is connected.

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct SiteSnapshot {
    pub running: bool,
    pub port: u16,
    /// Address the QR code points at.
    pub url: String,
    pub paired: bool,
}

pub fn site_state_path(root: &Path) -> PathBuf {
    root.join("site.state.json")
}

pub fn write_site_state(root: &Path, snap: &SiteSnapshot) {
    if let Ok(text) = serde_json::to_string(snap) {
        atomic_write(site_state_path(root), text.as_bytes());
    }
}

pub fn read_site_state(root: &Path) -> Option<SiteSnapshot> {
    let text = std::fs::read_to_string(site_state_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Ask the daemon for a fresh pairing token (the "Connect phone" button).
pub fn request_site_pair(root: &Path) {
    atomic_write(root.join("site.pair.request"), b"1");
}

pub fn take_site_pair_request(root: &Path) -> bool {
    let path = root.join("site.pair.request");
    if path.exists() {
        let _ = std::fs::remove_file(path);
        true
    } else {
        false
    }
}

/// Ask the daemon to disconnect the phone (the "Disconnect" button).
pub fn request_site_revoke(root: &Path) {
    atomic_write(root.join("site.revoke.request"), b"1");
}

pub fn take_site_revoke_request(root: &Path) -> bool {
    let path = root.join("site.revoke.request");
    if path.exists() {
        let _ = std::fs::remove_file(path);
        true
    } else {
        false
    }
}

/// Ask the daemon to archive or unarchive one monolog. The window never writes
/// `site/db.json`: it leaves the request and the daemon applies it.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteArchiveRequest {
    pub monolog: u64,
    pub archived: bool,
}

pub fn request_site_archive(root: &Path, monolog: u64, archived: bool) {
    let request = SiteArchiveRequest { monolog, archived };
    if let Ok(text) = serde_json::to_string(&request) {
        atomic_write(root.join("site.archive.request"), text.as_bytes());
    }
}

pub fn take_site_archive_request(root: &Path) -> Option<SiteArchiveRequest> {
    let path = root.join("site.archive.request");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&text).ok()
}

/// Ask the daemon to open or close the internet access for the phone. The
/// script needs an elevated PowerShell, so the request is a file and the daemon
/// runs it on its own thread.
pub fn request_site_network(root: &Path, enable: bool) {
    let name = if enable {
        "site.net.enable"
    } else {
        "site.net.disable"
    };
    atomic_write(root.join(name), b"1");
}

pub fn take_site_network(root: &Path) -> Option<bool> {
    let enable = root.join("site.net.enable");
    if enable.exists() {
        let _ = std::fs::remove_file(enable);
        let _ = std::fs::remove_file(root.join("site.net.disable"));
        return Some(true);
    }
    let disable = root.join("site.net.disable");
    if disable.exists() {
        let _ = std::fs::remove_file(disable);
        return Some(false);
    }
    None
}

/// The internet-access script is running right now (UAC prompt or UPnP).
pub fn set_site_network_busy(root: &Path, busy: bool) {
    set_flag_file(root.join("site.network.busy"), busy);
}

pub fn site_network_busy(root: &Path) -> bool {
    root.join("site.network.busy").exists()
}

// ---------------------------------------------------------------------------
// The recording session (daemon → UI), so the window can show that a recording
// was started by the API server. Removed when the session ends.

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub id: u64,
    pub owner: crate::state::InteractorKind,
    pub model: Option<String>,
}

impl From<&crate::state::Session> for SessionSnapshot {
    fn from(session: &crate::state::Session) -> Self {
        Self {
            id: session.id,
            owner: session.owner.kind,
            model: session.model_label(),
        }
    }
}

impl From<SessionSnapshot> for crate::state::SessionView {
    fn from(s: SessionSnapshot) -> Self {
        Self {
            id: s.id,
            owner: s.owner,
            model: s.model,
        }
    }
}

pub fn session_path(root: &Path) -> PathBuf {
    root.join("session.json")
}

/// Publish the open session, or remove the file when it is over.
pub fn write_session(root: &Path, session: Option<&crate::state::Session>) {
    match session {
        Some(session) => {
            if let Ok(text) = serde_json::to_string(&SessionSnapshot::from(session)) {
                atomic_write(session_path(root), text.as_bytes());
            }
        }
        None => {
            let _ = std::fs::remove_file(session_path(root));
        }
    }
}

pub fn read_session(root: &Path) -> Option<SessionSnapshot> {
    let text = std::fs::read_to_string(session_path(root)).ok()?;
    serde_json::from_str(&text).ok()
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

/// Same as the above but for the append-to-last-message shortcut.
pub fn append_hotkey_capture_path(root: &Path) -> PathBuf {
    root.join("hotkey2.capture")
}

/// Same as the above but for the record-and-paste shortcut.
pub fn paste_hotkey_capture_path(root: &Path) -> PathBuf {
    root.join("hotkey3.capture")
}

pub fn set_hotkey_capture(root: &Path, active: bool) {
    set_flag_file(hotkey_capture_path(root), active);
}

pub fn hotkey_capture_active(root: &Path) -> bool {
    hotkey_capture_path(root).exists()
}

pub fn set_append_hotkey_capture(root: &Path, active: bool) {
    set_flag_file(append_hotkey_capture_path(root), active);
}

pub fn append_hotkey_capture_active(root: &Path) -> bool {
    append_hotkey_capture_path(root).exists()
}

pub fn set_paste_hotkey_capture(root: &Path, active: bool) {
    set_flag_file(paste_hotkey_capture_path(root), active);
}

pub fn paste_hotkey_capture_active(root: &Path) -> bool {
    paste_hotkey_capture_path(root).exists()
}

fn set_flag_file(path: PathBuf, active: bool) {
    if active {
        let _ = std::fs::File::create(path);
    } else {
        let _ = std::fs::remove_file(path);
    }
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
        crate::state::Status::Recovering => "recovering",
        crate::state::Status::File => "file",
        crate::state::Status::NoModel => "nomodel",
    };
    atomic_write(status_path(root), text.as_bytes());
}

pub fn read_status(root: &Path) -> Option<crate::state::Status> {
    let text = std::fs::read_to_string(status_path(root)).ok()?;
    Some(match text.trim() {
        "recording" => crate::state::Status::Recording,
        "transcribing" => crate::state::Status::Transcribing,
        "recovering" => crate::state::Status::Recovering,
        "file" => crate::state::Status::File,
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
// Imported file transcription (UI → daemon → UI).

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct FileSnapshot {
    pub path: String,
    pub name: String,
    pub done_seconds: f64,
    pub total_seconds: f64,
    pub text: String,
    pub error: Option<String>,
    pub running: bool,
    pub cancelled: bool,
}

pub fn file_request_path(root: &Path) -> PathBuf {
    root.join("file.request")
}

pub fn write_file_request(root: &Path, path: &str) {
    atomic_write(file_request_path(root), path.as_bytes());
}

/// Returns the requested file path and consumes the request.
pub fn take_file_request(root: &Path) -> Option<String> {
    let path = file_request_path(root);
    let content = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let content = content.trim().to_string();
    (!content.is_empty()).then_some(content)
}

pub fn file_cancel_path(root: &Path) -> PathBuf {
    root.join("file.cancel")
}

pub fn request_file_cancel(root: &Path) {
    let _ = std::fs::File::create(file_cancel_path(root));
}

pub fn file_cancel_requested(root: &Path) -> bool {
    file_cancel_path(root).exists()
}

pub fn clear_file_cancel(root: &Path) {
    let _ = std::fs::remove_file(file_cancel_path(root));
}

pub fn file_state_path(root: &Path) -> PathBuf {
    root.join("file.state.json")
}

pub fn write_file_state(root: &Path, snapshot: &FileSnapshot) {
    if let Ok(text) = serde_json::to_string(snapshot) {
        atomic_write(file_state_path(root), text.as_bytes());
    }
}

pub fn read_file_state(root: &Path) -> Option<FileSnapshot> {
    let text = std::fs::read_to_string(file_state_path(root)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn remove_file_state(root: &Path) {
    let _ = std::fs::remove_file(file_state_path(root));
}

// ---------------------------------------------------------------------------
// Recording toggle requests from the UI or from the API server.

pub fn record_request_path(root: &Path) -> PathBuf {
    root.join("record.request")
}

/// Written by the UI (human interactor) or by the API server (API interactor).
/// The content is either an interactor JSON object or legacy plain text.
pub fn write_record_request(root: &Path, interactor_json: &str) {
    atomic_write(record_request_path(root), interactor_json.as_bytes());
}

/// Returns the pending interactor (or legacy content) and consumes the request.
pub fn take_record_request(root: &Path) -> Option<String> {
    let path = record_request_path(root);
    let content = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let content = content.trim().to_string();
    (!content.is_empty()).then_some(content)
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
