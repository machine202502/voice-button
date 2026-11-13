//! Cross-process runtime state, kept in SQLite next to the data rather than in
//! JSON files.
//!
//! The daemon publishes what it is doing (status, download progress, the open
//! session, live text), the window publishes requests, and both sides read the
//! same `kv` table. Values are a short line-based encoding, not JSON; the
//! settings file is the only JSON the app keeps.

use std::path::Path;

use crate::store::Store;

/// One value line: `key<TAB>value`, with control characters escaped so a text
/// field may contain line breaks.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn encode(fields: &[(&str, String)]) -> String {
    let mut out = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(key);
        out.push('\t');
        out.push_str(&escape(value));
    }
    out
}

fn decode(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('\t') {
            map.insert(key.to_string(), unescape(value));
        }
    }
    map
}

fn field(map: &std::collections::BTreeMap<String, String>, key: &str) -> String {
    map.get(key).cloned().unwrap_or_default()
}

fn number<T: std::str::FromStr>(map: &std::collections::BTreeMap<String, String>, key: &str) -> T
where
    T: Default,
{
    field(map, key).parse().unwrap_or_default()
}

fn open(root: &Path) -> Option<Store> {
    match Store::open(root) {
        Ok(store) => Some(store),
        Err(error) => {
            crate::log::log(&format!("runtime state database: {error:#}"));
            None
        }
    }
}

fn write_value(root: &Path, key: &str, value: &str) {
    if let Some(store) = open(root) {
        let _ = store.kv_put(key, value);
    }
}

fn read_value(root: &Path, key: &str) -> Option<String> {
    open(root)?.kv_get(key).ok().flatten().filter(|value| !value.is_empty())
}

/// Write via a temp file + rename so concurrent readers never see a partial
/// file. Only the settings file and the network script's plain-text result use
/// this now.
pub fn atomic_write(path: impl AsRef<Path>, bytes: &[u8]) {
    let path = path.as_ref();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// ---------------------------------------------------------------------------
// Model download progress (daemon → UI).

#[derive(Default, Clone, Debug, PartialEq)]
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

pub fn write_download_request(root: &Path, model_id: &str) {
    write_value(root, "request.download", model_id);
}

/// Returns the requested model id and consumes the request.
pub fn take_download_request(root: &Path) -> Option<String> {
    let store = open(root)?;
    let id = store.kv_take("request.download")?.trim().to_string();
    (!id.is_empty()).then_some(id)
}

pub fn request_cancel(root: &Path) {
    write_value(root, "request.download_cancel", "1");
}

pub fn cancel_requested(root: &Path) -> bool {
    read_value(root, "request.download_cancel").is_some()
}

pub fn clear_cancel(root: &Path) {
    if let Some(store) = open(root) {
        let _ = store.kv_delete("request.download_cancel");
    }
}

pub fn write_download_state(root: &Path, snap: &DownloadSnapshot) {
    let text = encode(&[
        ("model", snap.model.clone()),
        ("file", snap.file.to_string()),
        ("files", snap.files.to_string()),
        ("downloaded", snap.downloaded.to_string()),
        ("total", snap.total.map(|value| value.to_string()).unwrap_or_default()),
        ("done_bytes", snap.done_bytes.to_string()),
        ("done_total", snap.done_total.to_string()),
        ("done", (snap.done as u8).to_string()),
        ("error", snap.error.clone().unwrap_or_default()),
    ]);
    write_value(root, "runtime.download", &text);
}

pub fn read_download_state(root: &Path) -> Option<DownloadSnapshot> {
    let map = decode(&read_value(root, "runtime.download")?);
    Some(DownloadSnapshot {
        model: field(&map, "model"),
        file: number(&map, "file"),
        files: number(&map, "files"),
        downloaded: number(&map, "downloaded"),
        total: field(&map, "total").parse().ok(),
        done_bytes: number(&map, "done_bytes"),
        done_total: number(&map, "done_total"),
        done: field(&map, "done") == "1",
        error: Some(field(&map, "error")).filter(|value| !value.is_empty()),
    })
}

pub fn remove_download_state(root: &Path) {
    if let Some(store) = open(root) {
        let _ = store.kv_delete("runtime.download");
    }
}

// ---------------------------------------------------------------------------
// Voice Button API server status (daemon → UI).

#[derive(Default, Clone, Debug, PartialEq, Eq)]
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

pub fn write_api_state(root: &Path, snap: &ApiSnapshot) {
    let text = encode(&[
        ("running", (snap.running as u8).to_string()),
        ("host", snap.host.clone()),
        ("port", snap.port.to_string()),
        ("error", snap.error.clone().unwrap_or_default()),
    ]);
    write_value(root, "runtime.api", &text);
}

pub fn read_api_state(root: &Path) -> Option<ApiSnapshot> {
    let map = decode(&read_value(root, "runtime.api")?);
    Some(ApiSnapshot {
        running: field(&map, "running") == "1",
        host: field(&map, "host"),
        port: number(&map, "port"),
        error: Some(field(&map, "error")).filter(|value| !value.is_empty()),
    })
}

// ---------------------------------------------------------------------------
// The phone server (daemon → UI).

#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct SiteSnapshot {
    pub running: bool,
    pub port: u16,
    /// Address the QR code points at.
    pub url: String,
    pub paired: bool,
}

pub fn write_site_state(root: &Path, snap: &SiteSnapshot) {
    let text = encode(&[
        ("running", (snap.running as u8).to_string()),
        ("port", snap.port.to_string()),
        ("url", snap.url.clone()),
        ("paired", (snap.paired as u8).to_string()),
    ]);
    write_value(root, "runtime.site", &text);
}

pub fn read_site_state(root: &Path) -> Option<SiteSnapshot> {
    let map = decode(&read_value(root, "runtime.site")?);
    Some(SiteSnapshot {
        running: field(&map, "running") == "1",
        port: number(&map, "port"),
        url: field(&map, "url"),
        paired: field(&map, "paired") == "1",
    })
}

/// Ask the daemon for a fresh pairing token (the "Connect phone" button).
pub fn request_site_pair(root: &Path) {
    write_value(root, "request.site_pair", "1");
}

pub fn take_site_pair_request(root: &Path) -> bool {
    open(root)
        .and_then(|store| store.kv_take("request.site_pair"))
        .is_some()
}

/// Ask the daemon to disconnect the phone (the "Disconnect" button).
pub fn request_site_revoke(root: &Path) {
    write_value(root, "request.site_revoke", "1");
}

pub fn take_site_revoke_request(root: &Path) -> bool {
    open(root)
        .and_then(|store| store.kv_take("request.site_revoke"))
        .is_some()
}

/// Store or replace the Telegram bot token without writing it to config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramTokenRequest {
    pub token: String,
    pub proxy: String,
}

pub fn request_telegram_token(root: &Path, token: &str, proxy: &str) {
    let text = encode(&[
        ("token", token.to_string()),
        ("proxy", proxy.to_string()),
    ]);
    write_value(root, "request.telegram_token", &text);
}

pub fn take_telegram_token_request(root: &Path) -> Option<TelegramTokenRequest> {
    let store = open(root)?;
    let map = decode(&store.kv_take("request.telegram_token")?);
    Some(TelegramTokenRequest {
        token: field(&map, "token"),
        proxy: field(&map, "proxy"),
    })
}

pub fn request_telegram_reset(root: &Path) {
    write_value(root, "request.telegram_reset", "1");
}

pub fn take_telegram_reset(root: &Path) -> bool {
    open(root)
        .and_then(|store| store.kv_take("request.telegram_reset"))
        .is_some()
}

/// Ask the daemon to archive or unarchive one monolog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SiteArchiveRequest {
    pub monolog: u64,
    pub archived: bool,
}

pub fn request_site_archive(root: &Path, monolog: u64, archived: bool) {
    let text = encode(&[
        ("monolog", monolog.to_string()),
        ("archived", (archived as u8).to_string()),
    ]);
    write_value(root, "request.site_archive", &text);
}

pub fn take_site_archive_request(root: &Path) -> Option<SiteArchiveRequest> {
    let store = open(root)?;
    let map = decode(&store.kv_take("request.site_archive")?);
    Some(SiteArchiveRequest {
        monolog: number(&map, "monolog"),
        archived: field(&map, "archived") == "1",
    })
}

/// Ask the daemon to open or close the internet access for the phone.
pub fn request_site_network(root: &Path, enable: bool) {
    write_value(
        root,
        "request.site_network",
        if enable { "enable" } else { "disable" },
    );
}

pub fn take_site_network(root: &Path) -> Option<bool> {
    let value = open(root)?.kv_take("request.site_network")?;
    Some(value == "enable")
}

/// The internet-access script is running right now (UAC prompt or UPnP).
pub fn set_site_network_busy(root: &Path, busy: bool) {
    if busy {
        write_value(root, "runtime.site_network_busy", "1");
    } else if let Some(store) = open(root) {
        let _ = store.kv_delete("runtime.site_network_busy");
    }
}

pub fn site_network_busy(root: &Path) -> bool {
    read_value(root, "runtime.site_network_busy").is_some()
}

// ---------------------------------------------------------------------------
// The recording session (daemon → UI).

#[derive(Default, Clone, Debug, PartialEq, Eq)]
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

/// Publish the open session, or clear it when it is over.
pub fn write_session(root: &Path, session: Option<&crate::state::Session>) {
    match session {
        Some(session) => {
            let snapshot = SessionSnapshot::from(session);
            let text = encode(&[
                ("id", snapshot.id.to_string()),
                ("owner", format!("{:?}", snapshot.owner)),
                ("model", snapshot.model.clone().unwrap_or_default()),
            ]);
            write_value(root, "runtime.session", &text);
        }
        None => {
            if let Some(store) = open(root) {
                let _ = store.kv_delete("runtime.session");
            }
        }
    }
}

pub fn read_session(root: &Path) -> Option<SessionSnapshot> {
    let map = decode(&read_value(root, "runtime.session")?);
    let owner = match field(&map, "owner").as_str() {
        "Server" => crate::state::InteractorKind::Server,
        _ => crate::state::InteractorKind::User,
    };
    Some(SessionSnapshot {
        id: number(&map, "id"),
        owner,
        model: Some(field(&map, "model")).filter(|value| !value.is_empty()),
    })
}

// ---------------------------------------------------------------------------
// Recording status (daemon → UI).

pub fn write_status(root: &Path, status: crate::state::Status) {
    let text = match status {
        crate::state::Status::Idle => "idle",
        crate::state::Status::Recording => "recording",
        crate::state::Status::Transcribing => "transcribing",
        crate::state::Status::Recovering => "recovering",
        crate::state::Status::File => "file",
        crate::state::Status::NoModel => "nomodel",
    };
    write_value(root, "runtime.status", text);
}

pub fn read_status(root: &Path) -> Option<crate::state::Status> {
    Some(match read_value(root, "runtime.status")?.trim() {
        "recording" => crate::state::Status::Recording,
        "transcribing" => crate::state::Status::Transcribing,
        "recovering" => crate::state::Status::Recovering,
        "file" => crate::state::Status::File,
        "nomodel" => crate::state::Status::NoModel,
        _ => crate::state::Status::Idle,
    })
}

// ---------------------------------------------------------------------------
// Shortcut capture flags (UI → daemon).

fn set_flag(root: &Path, key: &str, active: bool) {
    if active {
        write_value(root, key, "1");
    } else if let Some(store) = open(root) {
        let _ = store.kv_delete(key);
    }
}

fn flag(root: &Path, key: &str) -> bool {
    read_value(root, key).is_some()
}

pub fn set_hotkey_capture(root: &Path, active: bool) {
    set_flag(root, "runtime.capture.record", active);
}

pub fn hotkey_capture_active(root: &Path) -> bool {
    flag(root, "runtime.capture.record")
}

pub fn set_append_hotkey_capture(root: &Path, active: bool) {
    set_flag(root, "runtime.capture.append", active);
}

pub fn append_hotkey_capture_active(root: &Path) -> bool {
    flag(root, "runtime.capture.append")
}

pub fn set_paste_hotkey_capture(root: &Path, active: bool) {
    set_flag(root, "runtime.capture.paste", active);
}

pub fn paste_hotkey_capture_active(root: &Path) -> bool {
    flag(root, "runtime.capture.paste")
}

/// The daemon watches the physical keyboard while the Settings window captures a
/// shortcut and writes the result here; the window may not hold keyboard focus.
pub fn write_hotkey_result(root: &Path, text: &str) {
    write_value(root, "runtime.hotkey_result", text);
}

pub fn take_hotkey_result(root: &Path) -> Option<String> {
    let text = open(root)?.kv_take("runtime.hotkey_result")?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

pub fn clear_hotkey_result(root: &Path) {
    if let Some(store) = open(root) {
        let _ = store.kv_delete("runtime.hotkey_result");
    }
}

// ---------------------------------------------------------------------------
// Live (streaming) transcription, published while recording.

#[derive(Default, Clone, Debug)]
pub struct LiveView {
    pub text: String,
    /// Recording start, milliseconds since UNIX epoch (for elapsed time).
    pub started: i64,
}

pub fn write_live(root: &Path, view: &LiveView) {
    let text = encode(&[
        ("text", view.text.clone()),
        ("started", view.started.to_string()),
    ]);
    write_value(root, "runtime.live", &text);
}

pub fn read_live(root: &Path) -> Option<LiveView> {
    let map = decode(&read_value(root, "runtime.live")?);
    Some(LiveView {
        text: field(&map, "text"),
        started: number(&map, "started"),
    })
}

pub fn clear_live(root: &Path) {
    if let Some(store) = open(root) {
        let _ = store.kv_delete("runtime.live");
    }
}

// ---------------------------------------------------------------------------
// Imported file transcription (UI → daemon → UI).

#[derive(Clone, Debug, Default, PartialEq)]
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

pub fn write_file_request(root: &Path, path: &str) {
    write_value(root, "request.file", path);
}

/// Returns the requested file path and consumes the request.
pub fn take_file_request(root: &Path) -> Option<String> {
    let store = open(root)?;
    let path = store.kv_take("request.file")?.trim().to_string();
    (!path.is_empty()).then_some(path)
}

pub fn request_file_cancel(root: &Path) {
    write_value(root, "request.file_cancel", "1");
}

pub fn file_cancel_requested(root: &Path) -> bool {
    read_value(root, "request.file_cancel").is_some()
}

pub fn clear_file_cancel(root: &Path) {
    if let Some(store) = open(root) {
        let _ = store.kv_delete("request.file_cancel");
    }
}

pub fn write_file_state(root: &Path, snapshot: &FileSnapshot) {
    let text = encode(&[
        ("path", snapshot.path.clone()),
        ("name", snapshot.name.clone()),
        ("done_seconds", snapshot.done_seconds.to_string()),
        ("total_seconds", snapshot.total_seconds.to_string()),
        ("text", snapshot.text.clone()),
        ("error", snapshot.error.clone().unwrap_or_default()),
        ("running", (snapshot.running as u8).to_string()),
        ("cancelled", (snapshot.cancelled as u8).to_string()),
    ]);
    write_value(root, "runtime.file", &text);
}

pub fn read_file_state(root: &Path) -> Option<FileSnapshot> {
    let map = decode(&read_value(root, "runtime.file")?);
    Some(FileSnapshot {
        path: field(&map, "path"),
        name: field(&map, "name"),
        done_seconds: number(&map, "done_seconds"),
        total_seconds: number(&map, "total_seconds"),
        text: field(&map, "text"),
        error: Some(field(&map, "error")).filter(|value| !value.is_empty()),
        running: field(&map, "running") == "1",
        cancelled: field(&map, "cancelled") == "1",
    })
}

pub fn remove_file_state(root: &Path) {
    if let Some(store) = open(root) {
        let _ = store.kv_delete("runtime.file");
    }
}

// ---------------------------------------------------------------------------
// Recording toggle requests.

/// Written by the UI (human interactor) or by the API server (API interactor).
/// The value is the interactor kind, `user` or `server`.
pub fn write_record_request(root: &Path, interactor: &str) {
    write_value(root, "request.record", interactor);
}

/// Returns the pending interactor and consumes the request.
pub fn take_record_request(root: &Path) -> Option<String> {
    let store = open(root)?;
    let value = store.kv_take("request.record")?.trim().to_string();
    (!value.is_empty()).then_some(value)
}
