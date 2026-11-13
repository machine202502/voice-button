//! The Telegram side of Voice Button.
//!
//! One long-polling thread receives updates, one worker turns queued media into
//! text. The queue, the owner and the chat mirror live in the SQLite database
//! so a restart resumes instead of forgetting. Only private chats are served;
//! strangers are ignored silently.

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::AppDirs;
use crate::i18n::{tr, trf};
use crate::state::{Shared, Status};

const MAX_FILE: u64 = 20 * 1024 * 1024;
/// The marks the bot puts on an incoming message. Telegram accepts only its own
/// fixed set of reactions: a check mark ("✅", "✔", "☑") and "❌" are refused with
/// REACTION_INVALID, a thumb is not. A non-premium bot may hold one reaction per
/// message, so the later mark replaces the earlier one.
const REACTION_WORKING: &str = "👀";
const REACTION_DONE: &str = "👍";
const REACTION_FAILED: &str = "👎";
const SEGMENT_SECONDS: f32 = 15.0;
const API_REAL: &str = "https://api.telegram.org";

// --------------------------------------------------------------- secure file

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        description: *const u16,
        entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt: *const std::ffi::c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut std::ffi::c_void,
        prompt: *const std::ffi::c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Secure {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub proxy: String,
}

fn secure_path(dirs: &AppDirs) -> PathBuf {
    dirs.root.join("telegram.secure")
}

fn secure_path_root(root: &Path) -> PathBuf {
    root.join("telegram.secure")
}

fn protect(bytes: &[u8]) -> Result<Vec<u8>> {
    let input = DataBlob {
        cb_data: bytes.len() as u32,
        pb_data: bytes.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0x1,
            &mut output,
        )
    };
    if ok == 0 {
        bail!("cannot encrypt the Telegram settings for this user");
    }
    let slice = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) };
    let result = slice.to_vec();
    unsafe {
        LocalFree(output.pb_data as *mut std::ffi::c_void);
    }
    Ok(result)
}

fn unprotect(bytes: &[u8]) -> Result<Vec<u8>> {
    let input = DataBlob {
        cb_data: bytes.len() as u32,
        pb_data: bytes.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            0x1,
            &mut output,
        )
    };
    if ok == 0 {
        bail!("cannot decrypt the Telegram settings for this user");
    }
    let slice = unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize) };
    let result = slice.to_vec();
    unsafe {
        LocalFree(output.pb_data as *mut std::ffi::c_void);
    }
    Ok(result)
}

pub fn read_secure(dirs: &AppDirs) -> Secure {
    let path = secure_path(dirs);
    let Ok(bytes) = std::fs::read(&path) else {
        return Secure::default();
    };
    let Ok(plain) = unprotect(&bytes) else {
        return Secure::default();
    };
    let text = String::from_utf8_lossy(&plain);
    let mut lines = text.splitn(2, '\n');
    Secure {
        token: lines.next().unwrap_or_default().to_string(),
        proxy: lines.next().unwrap_or_default().to_string(),
    }
}

pub fn has_token(dirs: &AppDirs) -> bool {
    // Only existence matters here: the tray decides whether the Telegram entry
    // is usable, and decrypting the file on every pass would be wasteful.
    std::fs::metadata(secure_path(dirs))
        .map(|meta| meta.len() > 0)
        .unwrap_or(false)
}

pub fn write_secure(dirs: &AppDirs, token: &str, proxy: &str) -> Result<()> {
    write_secure_root(&dirs.root, token, proxy)
}

pub fn write_secure_root(root: &Path, token: &str, proxy: &str) -> Result<()> {
    // The Windows DPAPI protects the file; inside it the two values are a plain
    // token line and a proxy line, not JSON.
    let plain = format!("{token}\n{proxy}");
    let protected = protect(plain.as_bytes())?;
    std::fs::write(secure_path_root(root), protected).context("writing telegram.secure")?;
    Ok(())
}

// ----------------------------------------------------------------- api client

#[derive(Clone)]
struct Client {
    token: String,
    base: String,
    agent: ureq::Agent,
}

trait Api {
    fn file_path(&self, file_id: &str) -> Result<(String, u64)>;
    fn download(&self, file_path: &str, target: &Path) -> Result<u64>;
    fn send_text(&self, chat: i64, reply_to: i64, text: &str) -> Result<()>;
    fn send_document(
        &self,
        chat: i64,
        reply_to: i64,
        name: &str,
        bytes: &[u8],
        caption: &str,
    ) -> Result<()>;
    fn react(&self, chat: i64, message: i64, emojis: &[&str]) -> Result<()>;
}

impl Api for Client {
    fn file_path(&self, file_id: &str) -> Result<(String, u64)> {
        Client::file_path(self, file_id)
    }
    fn download(&self, file_path: &str, target: &Path) -> Result<u64> {
        Client::download(self, file_path, target)
    }
    fn send_text(&self, chat: i64, reply_to: i64, text: &str) -> Result<()> {
        Client::send_text(self, chat, reply_to, text)
    }
    fn send_document(
        &self,
        chat: i64,
        reply_to: i64,
        name: &str,
        bytes: &[u8],
        caption: &str,
    ) -> Result<()> {
        Client::send_document(self, chat, reply_to, name, bytes, caption)
    }
    fn react(&self, chat: i64, message: i64, emojis: &[&str]) -> Result<()> {
        Client::react(self, chat, message, emojis)
    }
}

impl Client {
    fn new(token: String, proxy: &str, base: &str) -> Result<Self> {
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(20))
            .timeout_read(Duration::from_secs(70))
            .timeout_write(Duration::from_secs(30));
        if !proxy.trim().is_empty() {
            let address = if proxy.contains("://") {
                proxy.to_string()
            } else {
                format!("http://{proxy}")
            };
            builder = builder.proxy(ureq::Proxy::new(&address).context("bad Telegram proxy")?);
        }
        Ok(Self {
            token,
            base: base.trim_end_matches('/').to_string(),
            agent: builder.build(),
        })
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base, self.token, method)
    }

    fn call(&self, method: &str, body: &Value) -> Result<Value> {
        let answer = self
            .agent
            .post(&self.url(method))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())
            .map_err(|error| match error {
                ureq::Error::Status(code, response) => {
                    let text = response.into_string().unwrap_or_default();
                    anyhow!("Telegram {method} answered {code}: {text}")
                }
                other => anyhow!("Telegram {method}: {other}"),
            })?;
        let raw = answer.into_string().context("Telegram answered nothing")?;
        let value: Value = serde_json::from_str(&raw).context("Telegram answered no JSON")?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            bail!(
                "Telegram {method} refused: {}",
                value
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason")
            );
        }
        Ok(value)
    }

    fn get_updates(&self, offset: i64) -> Result<Vec<Value>> {
        let value = self.call(
            "getUpdates",
            &json!({
                "offset": offset,
                "timeout": 25,
                "allowed_updates": ["message"],
            }),
        )?;
        Ok(value
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    fn file_path(&self, file_id: &str) -> Result<(String, u64)> {
        let value = self.call("getFile", &json!({ "file_id": file_id }))?;
        let result = value.get("result").cloned().unwrap_or_default();
        let path = result
            .get("file_path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Telegram did not give a file path"))?
            .to_string();
        let size = result
            .get("file_size")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok((path, size))
    }

    fn download(&self, file_path: &str, target: &Path) -> Result<u64> {
        let url = format!("{}/file/bot{}/{}", self.base, self.token, file_path);
        let response = self
            .agent
            .get(&url)
            .call()
            .map_err(|error| anyhow!("Telegram file download: {error}"))?;
        let mut file = std::fs::File::create(target).context("creating the downloaded file")?;
        let mut reader = response.into_reader();
        let copied = std::io::copy(&mut reader, &mut file).context("copying the downloaded file")?;
        Ok(copied)
    }

    fn send_text(&self, chat: i64, reply_to: i64, text: &str) -> Result<()> {
        self.call(
            "sendMessage",
            &json!({
                "chat_id": chat,
                "text": text,
                "reply_to_message_id": reply_to,
                "disable_web_page_preview": true,
            }),
        )?;
        Ok(())
    }

    fn send_document(
        &self,
        chat: i64,
        reply_to: i64,
        name: &str,
        bytes: &[u8],
        caption: &str,
    ) -> Result<()> {
        let boundary = format!(
            "----voice-button-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or(0)
        );
        let mut body = Vec::new();
        let mut field = |name: &str, value: &str| {
            body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(value.as_bytes());
            body.extend_from_slice(b"\r\n");
        };
        field("chat_id", &chat.to_string());
        field("reply_to_message_id", &reply_to.to_string());
        if !caption.is_empty() {
            field("caption", caption);
        }
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"document\"; filename=\"{name}\"\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(b"Content-Type: text/plain\r\n\r\n");
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        self.agent
            .post(&self.url("sendDocument"))
            .set(
                "Content-Type",
                &format!("multipart/form-data; boundary={boundary}"),
            )
            .send_bytes(&body)
            .map_err(|error| anyhow!("Telegram sendDocument: {error}"))?;
        Ok(())
    }

    fn react(&self, chat: i64, message: i64, emojis: &[&str]) -> Result<()> {
        let reactions: Vec<Value> = emojis
            .iter()
            .map(|emoji| json!({ "type": "emoji", "emoji": emoji }))
            .collect();
        self.call(
            "setMessageReaction",
            &json!({
                "chat_id": chat,
                "message_id": message,
                "reaction": reactions,
            }),
        )?;
        Ok(())
    }

    /// Put exactly two commands in Telegram's menu. Each supported interface
    /// language gets its own descriptions; English is the default for every
    /// other Telegram language.
    fn set_commands(&self) -> Result<()> {
        let commands = |code: &str| {
            json!([
                {
                    "command": "start",
                    "description": crate::i18n::tr_lang(code, "Start the bot"),
                },
                {
                    "command": "status",
                    "description": crate::i18n::tr_lang(code, "Status: file, stage and queue"),
                },
            ])
        };
        self.call("setMyCommands", &json!({ "commands": commands("en") }))?;
        // Every catalog in `locales/` gets its own description; an unknown
        // language keeps the English default.
        for (code, _) in crate::i18n::available() {
            self.call(
                "setMyCommands",
                &json!({
                    "commands": commands(&code),
                    "language_code": code,
                }),
            )?;
        }
        Ok(())
    }
}

// -------------------------------------------------------------------- state

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatEntry {
    pub chat_id: i64,
    pub message_id: i64,
    pub at: String,
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub seconds: f32,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub error: Option<String>,
    /// Who sent the media: name, @username, or the generic "User".
    #[serde(default)]
    pub sender: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub update_id: i64,
    pub chat_id: i64,
    pub message_id: i64,
    pub file_id: String,
    pub file_name: String,
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub seconds: f32,
    #[serde(default)]
    pub done_seconds: f32,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub progress: f32,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub sender: String,
    /// Telegram's original message date, in local RFC 3339. Empty only for old
    /// queued jobs that were saved before this field existed.
    #[serde(default)]
    pub original_at: String,
    #[serde(default)]
    pub received_at: String,
    #[serde(default)]
    pub started_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TelegramState {
    #[serde(default)]
    pub owner: Option<i64>,
    #[serde(default)]
    pub offset: i64,
    #[serde(default)]
    pub queue: VecDeque<Job>,
    #[serde(default)]
    pub current: Option<Job>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub bot_name: String,
}

fn open_store(root: &Path) -> Option<crate::store::Store> {
    match crate::store::Store::open(root) {
        Ok(store) => Some(store),
        Err(error) => {
            crate::log::log(&format!("telegram database: {error:#}"));
            None
        }
    }
}

fn read_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<Job> {
    Ok(Job {
        update_id: row.get(0)?,
        chat_id: row.get(1)?,
        message_id: row.get(2)?,
        file_id: row.get(3)?,
        file_name: row.get(4)?,
        kind: row.get(5)?,
        size: row.get::<_, i64>(6)? as u64,
        seconds: row.get(7)?,
        done_seconds: row.get(8)?,
        stage: row.get(9)?,
        progress: row.get(10)?,
        text: row.get(11)?,
        error: row.get(12)?,
        sender: row.get(13)?,
        original_at: row.get(14)?,
        received_at: row.get(15)?,
        started_at: row.get(16)?,
    })
}

fn write_job(
    transaction: &rusqlite::Transaction<'_>,
    table: &str,
    key_column: &str,
    position: i64,
    job: &Job,
) -> rusqlite::Result<()> {
    let sql = crate::sql::render(
        "telegram/job_write",
        &[("table", table), ("key_column", key_column)],
    );
    transaction.execute(
        &sql,
        rusqlite::params![
            position,
            job.update_id,
            job.chat_id,
            job.message_id,
            job.file_id,
            job.file_name,
            job.kind,
            job.size as i64,
            job.seconds,
            job.done_seconds,
            job.stage,
            job.progress,
            job.text,
            job.error,
            job.sender,
            job.original_at,
            job.received_at,
            job.started_at
        ],
    )?;
    Ok(())
}

fn load_state(dirs: &AppDirs) -> TelegramState {
    let Some(store) = open_store(&dirs.root) else {
        return TelegramState::default();
    };
    let mut state = TelegramState::default();
    state.owner = store
        .kv_get("telegram.owner")
        .ok()
        .flatten()
        .and_then(|value| value.parse().ok());
    state.offset = store
        .kv_get("telegram.offset")
        .ok()
        .flatten()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    state.last_error = store.kv_get("telegram.last_error").ok().flatten();
    state.bot_name = store.kv_get("telegram.bot_name").ok().flatten().unwrap_or_default();
    let conn = store.conn();
    state.current = conn
        .query_row(
            crate::sql::query("telegram/current"),
            [],
            read_job,
        )
        .optional()
        .unwrap_or(None);
    if let Ok(mut statement) = conn.prepare(crate::sql::query("telegram/queue")) {
        if let Ok(rows) = statement.query_map([], read_job) {
            for job in rows.flatten() {
                state.queue.push_back(job);
            }
        }
    }
    state
}

fn save_state(dirs: &AppDirs, state: &TelegramState) {
    let Some(store) = open_store(&dirs.root) else {
        return;
    };
    let mut conn = store.conn();
    let Ok(transaction) = conn.transaction() else {
        return;
    };
    let _ = transaction.execute(
        crate::sql::query("telegram/kv_put"),
        rusqlite::params![
            "telegram.owner",
            state.owner.map(|id| id.to_string()).unwrap_or_default()
        ],
    );
    let _ = transaction.execute(
        crate::sql::query("telegram/kv_put"),
        rusqlite::params!["telegram.offset", state.offset.to_string()],
    );
    let _ = transaction.execute(
        crate::sql::query("telegram/kv_put"),
        rusqlite::params!["telegram.last_error", state.last_error.as_deref().unwrap_or("")],
    );
    let _ = transaction.execute(
        crate::sql::query("telegram/kv_put"),
        rusqlite::params!["telegram.bot_name", state.bot_name.as_str()],
    );
    let _ = transaction.execute(crate::sql::query("telegram/current_clear"), []);
    if let Some(job) = &state.current {
        let _ = write_job(&transaction, "telegram_current", "id", 1, job);
    }
    // The queue is small and changes often; replacing those rows is cheaper
    // than tracking each move. The chat grows forever and is append-only.
    let _ = transaction.execute(crate::sql::query("telegram/queue_clear"), []);
    for (position, job) in state.queue.iter().enumerate() {
        let _ = write_job(&transaction, "telegram_queue", "position", position as i64, job);
    }
    let _ = transaction.execute(crate::sql::query("telegram/bump"), []);
    let _ = transaction.commit();
}

fn append_chat(root: &Path, entry: &ChatEntry) {
    let Some(store) = open_store(root) else {
        return;
    };
    let mut conn = store.conn();
    let Ok(transaction) = conn.transaction() else {
        return;
    };
    let _ = transaction.execute(
        crate::sql::query("telegram/chat_append"),
        rusqlite::params![
            entry.chat_id,
            entry.message_id,
            entry.at,
            entry.sender,
            entry.kind,
            entry.file_name,
            entry.seconds,
            entry.text,
            entry.error,
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
        ],
    );
    if let Err(error) = transaction.execute(crate::sql::query("telegram/bump"), []) {
        crate::log::log(&format!("telegram: cannot bump the chat revision: {error}"));
    }
    let _ = transaction.commit();
}

fn read_chat(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatEntry> {
    Ok(ChatEntry {
        chat_id: row.get(1)?,
        message_id: row.get(2)?,
        at: row.get(3)?,
        sender: row.get(4)?,
        kind: row.get(5)?,
        file_name: row.get(6)?,
        seconds: row.get(7)?,
        text: row.get(8)?,
        error: row.get(9)?,
    })
}

pub fn reset_owner(root: &Path) {
    if let Some(store) = open_store(root) {
        let _ = store.kv_delete("telegram.owner");
    }
}

/// One page of the chat, oldest first, for the window's lazy list.
/// Which slice of the chat the list shows.
///
/// The bounds follow the bot's own clock: a message belongs to the day it was
/// processed, not to the day printed on the file it carried. `at` is still read
/// for older rows that predate the column, so old data keeps working.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ChatRange {
    #[default]
    Today,
    Week,
    All,
}

impl ChatRange {
    /// Lower bound in UTC, or `""` for "no bound". Every stored `added_at` is UTC
    /// in the same shape, so a string comparison is a time comparison.
    pub fn since(self) -> String {
        let local = chrono::Local::now();
        let start = match self {
            Self::All => return String::new(),
            // Midnight at the start of today: the current day runs from 00:00.
            Self::Today => local.date_naive().and_hms_opt(0, 0, 0),
            // Seven calendar days including today.
            Self::Week => (local.date_naive() - chrono::Days::new(6)).and_hms_opt(0, 0, 0),
        };
        let Some(start) = start else {
            return String::new();
        };
        let start = chrono::TimeZone::from_local_datetime(&chrono::Local, &start).earliest();
        start
            .map(|moment| {
                moment
                    .with_timezone(&chrono::Utc)
                    .format("%Y-%m-%dT%H:%M:%SZ")
                    .to_string()
            })
            .unwrap_or_default()
    }

    /// The SQL condition shared by every chat query: an empty bound string
    /// means everything. `param` is the placeholder's 1-based index in the
    /// statement that receives the fragment.
    fn clause(param: usize) -> &'static str {
        match param {
            1 => "(?1 = '' OR COALESCE(NULLIF(added_at, ''), at) >= ?1)",
            2 => "(?2 = '' OR COALESCE(NULLIF(added_at, ''), at) >= ?2)",
            4 => "(?4 = '' OR COALESCE(NULLIF(added_at, ''), at) >= ?4)",
            _ => panic!("chat range uses ?1, ?2 or ?4"),
        }
    }
}

pub fn chat_page(
    dirs: &AppDirs,
    range: ChatRange,
    limit: usize,
    offset: usize,
) -> Vec<ChatEntry> {
    let Some(store) = open_store(&dirs.root) else {
        return Vec::new();
    };
    let conn = store.conn();
    let sql = crate::sql::render("telegram/chat_page", &[("range", ChatRange::clause(1))]);
    let Ok(mut statement) = conn.prepare(&sql) else {
        return Vec::new();
    };
    statement
        .query_map(
            rusqlite::params![range.since(), limit as i64, offset as i64],
            read_chat,
        )
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

pub fn chat_count(dirs: &AppDirs, range: ChatRange) -> usize {
    let sql = crate::sql::render("telegram/chat_count", &[("range", ChatRange::clause(1))]);
    open_store(&dirs.root)
        .and_then(|store| {
            store
                .conn()
                .query_row(
                    &sql,
                    rusqlite::params![range.since()],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
        })
        .unwrap_or(0) as usize
}

pub fn chat_search_page(
    dirs: &AppDirs,
    range: ChatRange,
    needle: &str,
    limit: usize,
    offset: usize,
) -> Vec<ChatEntry> {
    let Some(store) = open_store(&dirs.root) else {
        return Vec::new();
    };
    let pattern = format!("%{}%", needle.trim().replace('%', "\\%").replace('_', "\\_"));
    let conn = store.conn();
    let sql = crate::sql::render(
        "telegram/chat_search_page",
        &[("range", ChatRange::clause(4))],
    );
    let Ok(mut statement) = conn.prepare(&sql) else {
        return Vec::new();
    };
    statement
        .query_map(
            rusqlite::params![pattern, limit as i64, offset as i64, range.since()],
            read_chat,
        )
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

pub fn chat_search_count(dirs: &AppDirs, range: ChatRange, needle: &str) -> usize {
    let sql = crate::sql::render(
        "telegram/chat_search_count",
        &[("range", ChatRange::clause(2))],
    );
    open_store(&dirs.root)
        .and_then(|store| {
            let pattern = format!("%{}%", needle.trim().replace('%', "\\%").replace('_', "\\_"));
            store
                .conn()
                .query_row(
                    &sql,
                    rusqlite::params![pattern, range.since()],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
        })
        .unwrap_or(0) as usize
}

/// What the settings window shows about the bot, without the token.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub owner: Option<i64>,
    pub queued: usize,
    pub current: Option<Job>,
    pub last_error: Option<String>,
}

pub fn snapshot(root: &Path) -> Snapshot {
    let state = load_state(&crate::config::AppDirs {
        root: root.to_path_buf(),
        models: std::path::PathBuf::new(),
    });
    Snapshot {
        owner: state.owner,
        queued: state.queue.len(),
        current: state.current,
        last_error: state.last_error,
    }
}

pub fn status_text(dirs: &AppDirs) -> String {
    let state = load_state(dirs);
    status_of(&state)
}

fn status_of(state: &TelegramState) -> String {
    let Some(job) = state.current.as_ref().or_else(|| state.queue.front()) else {
        return tr("Queue is empty.");
    };
    let place = if state.current.is_some() {
        tr("now")
    } else {
        trf("in queue: {count}", &[("count", &state.queue.len().to_string())])
    };
    let percent = if job.seconds > 0.0 {
        (job.done_seconds / job.seconds * 100.0).clamp(0.0, 100.0)
    } else {
        job.progress.clamp(0.0, 100.0)
    };
    let remaining = (job.seconds - job.done_seconds).max(0.0);
    let started = chrono::DateTime::parse_from_rfc3339(&job.started_at)
        .ok()
        .map(|at| (chrono::Local::now() - at.with_timezone(&chrono::Local)).num_seconds())
        .filter(|seconds| *seconds > 0);
    let eta = match (started, job.done_seconds) {
        (Some(elapsed), done) if done > 0.5 && elapsed > 0 => {
            let speed = done / elapsed as f32;
            format!("{:.0} {}", remaining / speed.max(0.05), tr("s"))
        }
        _ => "—".to_string(),
    };
    let file = if job.file_name.is_empty() {
        tr("unnamed")
    } else {
        job.file_name.clone()
    };
    let stage = if job.stage.is_empty() {
        tr("queued")
    } else {
        tr(&job.stage)
    };
    let error = job
        .error
        .as_ref()
        .map(|error| trf("Error: {error}", &[("error", error)]))
        .unwrap_or_else(|| tr("No error."));
    trf(
        "File: {file}\nStage: {stage}\nDone: {done} / {total} s ({percent}%)\nPosition: {place}\nETA: {eta}\n{error}",
        &[
            ("file", &file),
            ("stage", &stage),
            ("done", &format!("{:.1}", job.done_seconds)),
            ("total", &format!("{:.1}", job.seconds)),
            ("percent", &format!("{:.0}", percent)),
            ("place", &place),
            ("eta", &eta),
            ("error", &error),
        ],
    )
}

// --------------------------------------------------------------------- bot

/// The tray icon turns violet while Telegram works, exactly like the phone.
fn set_background(shared: &Arc<Shared>, active: bool) {
    let mut data = shared.lock();
    if active {
        data.background.insert(crate::state::BackgroundJob::Telegram);
    } else {
        data.background.remove(&crate::state::BackgroundJob::Telegram);
    }
}

fn proxy_setting(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn settings(dirs: &AppDirs, shared: &Arc<Shared>) -> (bool, String, String, String) {
    let config = shared.lock().config.clone();
    let secure = read_secure(dirs);
    // Empty means direct. The old secure-file copy of the proxy is not used:
    // a stored value must be visible in Settings to be in effect.
    let proxy = proxy_setting(&config.telegram_proxy).unwrap_or_default();
    let base = if config.telegram_api_base.trim().is_empty() {
        API_REAL.to_string()
    } else {
        config.telegram_api_base
    };
    (config.telegram_enabled, secure.token, proxy, base)
}

/// Start the polling and worker threads. They live as long as the daemon and
/// wait quietly while the setting is off or no token is stored.
pub fn spawn(dirs: AppDirs, shared: Arc<Shared>, asr: Arc<crate::asr_engine::Engine>) {
    let state = Arc::new(Mutex::new(load_state(&dirs)));
    // A downloaded take is a temporary file: it is deleted as soon as its job
    // ends. Files found here belong to a run that crashed mid-job, so they are
    // removed before the worker starts.
    if let Ok(entries) = std::fs::read_dir(dirs.root.join("telegram")) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let poll_dirs = dirs.clone();
    let poll_shared = shared.clone();
    let poll_state = state.clone();
    std::thread::spawn(move || {
        let mut last_problem = String::new();
        let mut commands_for = String::new();
        loop {
            // The settings window cannot write the token itself: it leaves a
            // request, and the daemon stores it encrypted for this user.
            if let Some(request) = crate::ipc::take_telegram_token_request(&poll_dirs.root) {
                if let Err(error) = write_secure_root(
                    &poll_dirs.root,
                    &request.token,
                    &request.proxy,
                ) {
                    crate::log::log(&format!("telegram: cannot store the token: {error:#}"));
                }
            }
            if crate::ipc::take_telegram_reset(&poll_dirs.root) {
                reset_owner(&poll_dirs.root);
            }
            let (enabled, token, proxy, base) = settings(&poll_dirs, &poll_shared);
            if !enabled || token.trim().is_empty() {
                std::thread::sleep(Duration::from_millis(800));
                continue;
            }
            let commands_key = format!("{token}|{}", crate::i18n::active_code());
            let client = match Client::new(token, &proxy, &base) {
                Ok(client) => client,
                Err(error) => {
                    let text = format!("{error:#}");
                    if text != last_problem {
                        crate::log::log(&format!("telegram: {text}"));
                        last_problem = text.clone();
                    }
                    poll_state.lock().unwrap().last_error = Some(text);
                    save_state(&poll_dirs, &poll_state.lock().unwrap());
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
            };
            if commands_for != commands_key {
                match client.set_commands() {
                    Ok(()) => commands_for = commands_key.clone(),
                    Err(error) => {
                        crate::log::log(&format!("telegram: cannot set commands: {error:#}"));
                    }
                }
            }
            let offset = poll_state.lock().unwrap().offset;
            match client.get_updates(offset) {
                Ok(updates) => {
                    for update in updates {
                        let update_id = update.get("update_id").and_then(Value::as_i64).unwrap_or(0);
                        {
                            let mut state = poll_state.lock().unwrap();
                            state.offset = state.offset.max(update_id + 1);
                        }
                        match handle_update(&client, &poll_dirs, &poll_state, &update) {
                            Ok(()) => {
                                poll_state.lock().unwrap().last_error = None;
                                save_state(&poll_dirs, &poll_state.lock().unwrap());
                            }
                            Err(error) => {
                                let text = format!("{error:#}");
                                crate::log::log(&format!("telegram: {text}"));
                                poll_state.lock().unwrap().last_error = Some(text);
                                save_state(&poll_dirs, &poll_state.lock().unwrap());
                            }
                        }
                    }
                    save_state(&poll_dirs, &poll_state.lock().unwrap());
                }
                Err(error) => {
                    let text = format!("{error:#}");
                    if text != last_problem {
                        crate::log::log(&format!("telegram: {text}"));
                        last_problem = text;
                        poll_state.lock().unwrap().last_error = Some(format!("{error:#}"));
                        save_state(&poll_dirs, &poll_state.lock().unwrap());
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            }
        }
    });

    std::thread::spawn(move || loop {
        let (enabled, token, proxy, base) = settings(&dirs, &shared);
        if !enabled || token.trim().is_empty() {
            std::thread::sleep(Duration::from_millis(800));
            continue;
        }
        let job = {
            let mut state = state.lock().unwrap();
            if state.current.is_none() {
                state.current = state.queue.pop_front();
                if state.current.is_some() {
                    save_state(&dirs, &state);
                }
            }
            state.current.clone()
        };
        let Some(job) = job else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let client = match Client::new(token, &proxy, &base) {
            Ok(client) => client,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        set_background(&shared, true);
        let outcome = process_job(&client, &dirs, &shared, &asr, &state, &job);
        finish_job(&client, &dirs, &state, &job, outcome);
        set_background(&shared, false);
    });
}

/// The bot names the computer's time zone explicitly; the app deliberately
/// does not, so changing the computer's zone re-renders the same stored instant.
fn telegram_date_label_with_zone(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("[%d.%m.%y %H:%M UTC%:z]")
                .to_string()
        })
        .unwrap_or_default()
}

fn message_date(message: &Value) -> String {
    let seconds = message
        .get("forward_origin")
        .and_then(|origin| origin.get("date"))
        .and_then(Value::as_i64)
        .or_else(|| message.get("forward_date").and_then(Value::as_i64))
        .or_else(|| message.get("date").and_then(Value::as_i64));
    seconds
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0)).map(|time| time.with_timezone(&chrono::Local))
        .map(|time| time.to_rfc3339())
        .unwrap_or_default()
}

fn user_label(value: &Value) -> Option<String> {
    let first = value.get("first_name").and_then(Value::as_str).unwrap_or("").trim();
    let last = value.get("last_name").and_then(Value::as_str).unwrap_or("").trim();
    let name = format!("{first} {last}").trim().to_string();
    let username = value.get("username").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty());
    if !name.is_empty() {
        return Some(match username {
            Some(username) => format!("{name} (@{username})"),
            None => name,
        });
    }
    username.map(|username| format!("@{username}"))
}

fn chat_label(value: &Value) -> Option<String> {
    value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| user_label(value))
}

/// The forwarded person wins over the sender: the question is whose words were
/// transcribed. Telegram has two shapes for this (old `forward_from` and new
/// `forward_origin`), so both are read.
fn sender_label(message: &Value) -> String {
    if let Some(origin) = message.get("forward_origin") {
        let kind = origin.get("type").and_then(Value::as_str).unwrap_or("");
        let label = match kind {
            "user" => origin.get("sender_user").and_then(user_label),
            "hidden_user" => origin
                .get("sender_user_name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
            "chat" | "channel" => origin.get("sender_chat").and_then(chat_label),
            _ => None,
        };
        return label.unwrap_or_else(|| tr("User"));
    }
    if let Some(person) = message.get("forward_from").and_then(user_label) {
        return person;
    }
    if let Some(name) = message
        .get("forward_sender_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        return name.to_string();
    }
    if let Some(chat) = message.get("forward_from_chat").and_then(chat_label) {
        return chat;
    }
    message
        .get("from")
        .and_then(user_label)
        .unwrap_or_else(|| tr("User"))
}

fn handle_update(
    client: &dyn Api,
    dirs: &AppDirs,
    state: &Arc<Mutex<TelegramState>>,
    update: &Value,
) -> Result<()> {
    let Some(message) = update.get("message") else {
        return Ok(());
    };
    let chat = message.get("chat").cloned().unwrap_or_default();
    if chat.get("type").and_then(Value::as_str) != Some("private") {
        return Ok(());
    }
    let chat_id = chat.get("id").and_then(Value::as_i64).unwrap_or(0);
    let message_id = message.get("message_id").and_then(Value::as_i64).unwrap_or(0);
    let from = message
        .get("from")
        .and_then(|from| from.get("id"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let text = message.get("text").and_then(Value::as_str).unwrap_or("");
    let owner = state.lock().unwrap().owner;

    if text.trim() == "/start" {
        if owner.is_none() {
            {
                let mut state = state.lock().unwrap();
                state.owner = Some(from);
            }
            save_state(dirs, &state.lock().unwrap());
            client.send_text(
                chat_id,
                message_id,
                &tr("I am here. Send a voice note, audio, video, video note or a file with audio — I will return the text here. /status shows the queue."),
            )?;
        } else if owner == Some(from) {
            client.send_text(chat_id, message_id, &tr("The bot is already connected to you."))?;
        }
        return Ok(());
    }

    if owner != Some(from) {
        return Ok(());
    }

    if text.trim() == "/status" {
        let answer = status_of(&state.lock().unwrap());
        client.send_text(chat_id, message_id, &answer)?;
        return Ok(());
    }

    let Some(media) = extract_media(message) else {
        client.send_text(
            chat_id,
            message_id,
            &tr("Send a voice note, audio, video, video note or a file with audio."),
        )?;
        return Ok(());
    };
    if media.size > MAX_FILE {
        client.send_text(
            chat_id,
            message_id,
            &trf(
                "File is larger than 20 MB ({size} MB). Telegram gives bots at most 20 MB; send a shorter file or split it.",
                &[("size", &(media.size / (1024 * 1024)).to_string())],
            ),
        )?;
        return Ok(());
    }
    client.react(chat_id, message_id, &[REACTION_WORKING])?;
    let mut job = media;
    job.update_id = update.get("update_id").and_then(Value::as_i64).unwrap_or(0);
    job.chat_id = chat_id;
    job.message_id = message_id;
    job.sender = sender_label(message);
    job.original_at = message_date(message);
    job.received_at = chrono::Local::now().to_rfc3339();
    {
        let mut state = state.lock().unwrap();
        state.queue.push_back(job);
        save_state(dirs, &state);
    }
    Ok(())
}

fn extract_media(message: &Value) -> Option<Job> {
    for (kind, key) in [
        ("voice", "voice"),
        ("video_note", "video_note"),
        ("audio", "audio"),
        ("video", "video"),
        ("document", "document"),
    ] {
        let Some(item) = message.get(key) else {
            continue;
        };
        let Some(file_id) = item.get("file_id").and_then(Value::as_str) else {
            continue;
        };
        let size = item
            .get("file_size")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let duration = item
            .get("duration")
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32;
        let name = item
            .get("file_name")
            .and_then(Value::as_str)
            .unwrap_or_else(|| match kind {
                "voice" | "video_note" => "voice.ogg",
                "video" => "video.mp4",
                "audio" => "audio.mp3",
                _ => "file.bin",
            })
            .to_string();
        return Some(Job {
            update_id: 0,
            chat_id: 0,
            message_id: 0,
            file_id: file_id.to_string(),
            file_name: name,
            kind: kind.to_string(),
            sender: String::new(),
            original_at: String::new(),
            size,
            seconds: duration,
            done_seconds: 0.0,
            stage: "queued".to_string(),
            progress: 0.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        });
    }
    None
}

/// Removes a downloaded take when its job leaves the scope, whatever the
/// outcome. The original file stays in Telegram; only the local copy goes.
struct TemporaryAudio(PathBuf);

impl Drop for TemporaryAudio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn process_job(
    client: &dyn Api,
    dirs: &AppDirs,
    shared: &Arc<Shared>,
    asr: &Arc<crate::asr_engine::Engine>,
    state: &Arc<Mutex<TelegramState>>,
    job: &Job,
) -> Result<String> {
    let root = dirs.root.join("telegram");
    std::fs::create_dir_all(&root)?;
    {
        let mut state = state.lock().unwrap();
        if let Some(current) = state.current.as_mut() {
            current.stage = "downloading".to_string();
        }
        save_state(dirs, &state);
    }
    let (file_path, size) = client.file_path(&job.file_id)?;
    if size > MAX_FILE || (job.size > MAX_FILE) {
        bail!("файл больше 20 МБ");
    }
    let extension = Path::new(&file_path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("bin");
    let target = root.join(format!("{}.{}", job.update_id, extension));
    // The file exists only for this job: success, error and an early return all
    // remove it, so the data directory does not grow with old audio. The guard
    // is created before the download so a half-written file is removed too.
    let _temporary = TemporaryAudio(target.clone());
    let downloaded = client.download(&file_path, &target)?;
    if downloaded > MAX_FILE {
        bail!("файл больше 20 МБ");
    }

    let samples = crate::telegram_media::decode_to_16k(&target)?;
    let seconds = samples.len() as f32 / crate::telegram_media::TARGET_RATE as f32;
    {
        let mut state = state.lock().unwrap();
        if let Some(current) = state.current.as_mut() {
            current.seconds = seconds;
            current.stage = "transcribing".to_string();
        }
        save_state(dirs, &state);
    }
    // Empty model: the recognizer worker already has the user's selected model
    // loaded; its API id (Engine/model) is not what the settings store returns.
    {
        let mut state = state.lock().unwrap();
        if let Some(current) = state.current.as_mut() {
            current.started_at = chrono::Local::now().to_rfc3339();
        }
        save_state(dirs, &state);
    }
    let step = (SEGMENT_SECONDS * crate::telegram_media::TARGET_RATE as f32) as usize;
    let mut text = String::new();
    let mut done = 0usize;
    let model = {
        let data = shared.lock();
        crate::transcribe::source_model(
            data.config.telegram_model.trim(),
            &data.config.telegram_model_folder,
        )
    };
    for chunk in samples.chunks(step.max(1)) {
        wait_for_engine(shared, state, dirs)?;
        {
            let mut state = state.lock().unwrap();
            if let Some(current) = state.current.as_mut() {
                current.stage = "transcribing".to_string();
            }
            save_state(dirs, &state);
        }
        let reply = asr
            .decode(
                crate::asr_engine::Priority::Telegram,
                &model,
                chunk,
                crate::telegram_media::TARGET_RATE,
            )
            .context("the recognizer did not answer")?;
        if reply.ok && !reply.text.trim().is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(reply.text.trim());
        }
        done += chunk.len();
        {
            let mut state = state.lock().unwrap();
            if let Some(current) = state.current.as_mut() {
                current.done_seconds = done as f32 / crate::telegram_media::TARGET_RATE as f32;
                current.progress = if seconds > 0.0 {
                    (current.done_seconds / seconds * 100.0).min(100.0)
                } else {
                    0.0
                };
                current.text = text.clone();
            }
            save_state(dirs, &state);
        }
    }
    Ok(text)
}

/// The Telegram worker may take the recognizer only when nobody above it is
/// using it. Its own violet background mark is not a reason to wait.
fn telegram_may_decode(data: &crate::state::Data) -> bool {
    data.config.telegram_enabled
        && data.status == Status::Idle
        && data.session.is_none()
        && data.stream.is_none()
        && !data.recovering
        && !data.background_has(crate::state::BackgroundJob::Phone)
        && data
            .file
            .as_ref()
            .map(|file| !file.running && file.total_seconds > 0.0)
            .unwrap_or(true)
}

fn wait_for_engine(
    shared: &Arc<Shared>,
    state: &Arc<Mutex<TelegramState>>,
    dirs: &AppDirs,
) -> Result<()> {
    loop {
        {
            let data = shared.lock();
            if telegram_may_decode(&data) {
                return Ok(());
            }
            if !data.config.telegram_enabled {
                bail!("бот выключен");
            }
        }
        {
            let mut state = state.lock().unwrap();
            if let Some(current) = state.current.as_mut() {
                current.stage = "engine busy".to_string();
            }
            save_state(dirs, &state);
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn finish_job(
    client: &dyn Api,
    dirs: &AppDirs,
    state: &Arc<Mutex<TelegramState>>,
    job: &Job,
    outcome: Result<String>,
) {
    let finished_at = chrono::Local::now().to_rfc3339();
    let at = if job.original_at.trim().is_empty() {
        finished_at
    } else {
        job.original_at.clone()
    };
    let transcribed = matches!(&outcome, Ok(text) if !text.trim().is_empty());
    let (text, error) = match outcome {
        Ok(text) if !text.trim().is_empty() => (text, None),
        Ok(_) => (tr("Bot: speech was not recognised."), None),
        Err(error) => (
            trf("Bot: could not transcribe: {error}", &[("error", &format!("{error:#}"))]),
            Some(format!("{error:#}")),
        ),
    };
    if error.is_none() {
        let config = crate::config::load_config(&dirs.root);
        let history_at = chrono::DateTime::parse_from_rfc3339(&at)
            .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|_| at.clone());
        let entry = crate::history::HistoryEntry {
            at: history_at,
            text: text.clone(),
            model: crate::transcribe::selected_id(&config),
            parts: Vec::new(),
            source: crate::history::Source::Telegram,
            state: crate::history::EntryState::Complete,
            audio: None,
            group: None,
            message: Some(job.message_id as u64),
        };
        if let Err(problem) = crate::history::append(&dirs.root, &entry) {
            crate::log::log(&format!("telegram: history: {problem:#}"));
        }
    }
    let sender = if job.sender.trim().is_empty() {
        tr("User")
    } else {
        job.sender.clone()
    };
    let stamp = telegram_date_label_with_zone(&at);
    let meta = if stamp.is_empty() {
        sender.clone()
    } else {
        format!("{stamp} {sender}")
    };
    let answer = if transcribed {
        format!("{meta}: {text}")
    } else {
        text.clone()
    };
    let send = if answer.chars().count() <= 4096 {
        client.send_text(job.chat_id, job.message_id, &answer)
    } else {
        let name = format!("telegram-{}.txt", job.update_id);
        // The attachment keeps the same metadata in its caption (visible in
        // Telegram) and as its first line (visible after opening the file).
        client.send_document(
            job.chat_id,
            job.message_id,
            &name,
            answer.as_bytes(),
            &meta,
        )
    };
    if let Err(problem) = send {
        crate::log::log(&format!("telegram: reply: {problem:#}"));
    }
    // The terminal mark replaces the eyes: one reaction is all a non-premium bot
    // may hold, and the last state is the one worth reading.
    let terminal = if error.is_none() {
        REACTION_DONE
    } else {
        REACTION_FAILED
    };
    if let Err(problem) = client.react(job.chat_id, job.message_id, &[terminal]) {
        crate::log::log(&format!("telegram: cannot mark the message: {problem:#}"));
    }
    let mut state = state.lock().unwrap();
    let chat_entry = ChatEntry {
        chat_id: job.chat_id,
        message_id: job.message_id,
        at,
        file_name: job.file_name.clone(),
        kind: job.kind.clone(),
        seconds: job.seconds,
        text: text.clone(),
        error,
        sender: job.sender.clone(),
    };
    append_chat(&dirs.root, &chat_entry);
    state.current = None;
    save_state(dirs, &state);
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        sent: Mutex<Vec<(i64, i64, String)>>,
        documents: Mutex<Vec<String>>,
        reactions: Mutex<Vec<(i64, String)>>,
        size: u64,
    }

    impl Fake {
        fn new(size: u64) -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                documents: Mutex::new(Vec::new()),
                reactions: Mutex::new(Vec::new()),
                size,
            }
        }
        fn sent(&self) -> Vec<String> {
            self.sent.lock().unwrap().iter().map(|item| item.2.clone()).collect()
        }
    }

    impl Api for Fake {
        fn file_path(&self, _file_id: &str) -> Result<(String, u64)> {
            Ok(("files/voice.ogg".to_string(), self.size))
        }
        fn download(&self, _file_path: &str, _target: &Path) -> Result<u64> {
            Ok(self.size)
        }
        fn send_text(&self, chat: i64, reply_to: i64, text: &str) -> Result<()> {
            self.sent.lock().unwrap().push((chat, reply_to, text.to_string()));
            Ok(())
        }
        fn send_document(
            &self,
            _chat: i64,
            _reply_to: i64,
            name: &str,
            _bytes: &[u8],
            _caption: &str,
        ) -> Result<()> {
            self.documents.lock().unwrap().push(name.to_string());
            Ok(())
        }
        fn react(&self, _chat: i64, message: i64, emojis: &[&str]) -> Result<()> {
            self.reactions
                .lock()
                .unwrap()
                .push((message, emojis.join("")));
            Ok(())
        }
    }

    fn dirs(name: &str) -> AppDirs {
        let root = std::env::temp_dir().join(format!(
            "vb-telegram-{}-{}-{}",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("models")).unwrap();
        AppDirs {
            root: root.clone(),
            models: root.join("models"),
        }
    }

    fn start(update_id: i64, user: i64, text: &str) -> Value {
        json!({
            "update_id": update_id,
            "message": {
                "message_id": update_id * 10,
                "chat": { "id": user, "type": "private" },
                "from": { "id": user },
                "text": text,
            }
        })
    }

    fn voice(update_id: i64, user: i64, size: u64) -> Value {
        json!({
            "update_id": update_id,
            "message": {
                "message_id": update_id * 10,
                "chat": { "id": user, "type": "private" },
                "from": { "id": user },
                "voice": { "file_id": "f1", "file_size": size, "duration": 5 },
            }
        })
    }

    #[test]
    fn the_first_start_owns_the_bot_and_others_are_silent() {
        let dirs = dirs("owner");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        handle_update(&api, &dirs, &state, &start(1, 111, "/start")).unwrap();
        assert_eq!(state.lock().unwrap().owner, Some(111));
        assert_eq!(api.sent().len(), 1);
        handle_update(&api, &dirs, &state, &start(2, 222, "/start")).unwrap();
        assert_eq!(state.lock().unwrap().owner, Some(111), "a stranger does not take over");
        assert_eq!(api.sent().len(), 1, "and gets no answer");
        handle_update(&api, &dirs, &state, &voice(3, 222, 1000)).unwrap();
        assert!(state.lock().unwrap().queue.is_empty(), "a stranger cannot queue files");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn a_voice_note_is_queued_and_acknowledged_with_eyes() {
        let dirs = dirs("voice");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        handle_update(&api, &dirs, &state, &start(1, 111, "/start")).unwrap();
        handle_update(&api, &dirs, &state, &voice(2, 111, 1000)).unwrap();
        let queued = state.lock().unwrap().queue.front().cloned().unwrap();
        assert_eq!(queued.file_id, "f1");
        assert_eq!(queued.kind, "voice");
        assert_eq!(queued.file_name, "voice.ogg");
        assert_eq!(queued.seconds, 5.0);
        assert_eq!(api.reactions.lock().unwrap()[0], (20, "👀".to_string()));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn a_file_over_twenty_megabytes_is_refused_before_download() {
        let dirs = dirs("large");
        let api = Fake::new(MAX_FILE + 1);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        handle_update(&api, &dirs, &state, &start(1, 111, "/start")).unwrap();
        handle_update(&api, &dirs, &state, &voice(2, 111, MAX_FILE + 1)).unwrap();
        assert!(state.lock().unwrap().queue.is_empty());
        assert!(api.sent().iter().any(|text| text.contains("20 MB") || text.contains("20 МБ")));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn only_the_owner_gets_status() {
        let dirs = dirs("status");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        handle_update(&api, &dirs, &state, &start(1, 111, "/start")).unwrap();
        handle_update(&api, &dirs, &state, &start(2, 222, "/status")).unwrap();
        assert!(!api.sent().iter().any(|text| text.contains(&tr("Queue is empty."))));
        handle_update(&api, &dirs, &state, &start(3, 111, "/status")).unwrap();
        assert!(api.sent().iter().any(|text| text.contains(&tr("Queue is empty."))));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn status_shows_file_stage_percent_and_queue_position() {
        let mut state = TelegramState::default();
        state.current = Some(Job {
            update_id: 1,
            chat_id: 1,
            message_id: 2,
            file_id: "f".into(),
            file_name: "long.ogg".into(),
            kind: "voice".into(),
            sender: String::new(),
            original_at: String::new(),
            size: 1000,
            seconds: 100.0,
            done_seconds: 25.0,
            stage: "расшифровка".into(),
            progress: 25.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        });
        let text = status_of(&state);
        assert!(text.contains("long.ogg"));
        assert!(text.contains("расшифровка"));
        assert!(text.contains("25.0 / 100.0"));
        assert!(text.contains("25%"));
    }

    #[test]
    fn a_very_long_transcript_is_sent_as_a_txt_document() {
        let dirs = dirs("long");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        let job = Job {
            update_id: 3,
            chat_id: 7,
            message_id: 8,
            file_id: "f".into(),
            file_name: "long.ogg".into(),
            kind: "voice".into(),
            sender: String::new(),
            original_at: String::new(),
            size: 1000,
            seconds: 60.0,
            done_seconds: 60.0,
            stage: "расшифровка".into(),
            progress: 100.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        };
        state.lock().unwrap().current = Some(job.clone());
        finish_job(&api, &dirs, &state, &job, Ok("я".repeat(5000)));
        assert_eq!(api.documents.lock().unwrap().len(), 1, "a document, not a message");
        assert!(api.sent().is_empty(), "nothing over 4096 is sent as text");
        assert_eq!(api.reactions.lock().unwrap().last().unwrap().1, "👍");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    #[ignore]
    fn real_bot_api_registers_the_two_commands() {
        let dirs = crate::config::AppDirs {
            root: std::env::var_os("APPDATA")
                .map(std::path::PathBuf::from)
                .unwrap()
                .join("voice-button"),
            models: std::path::PathBuf::new(),
        };
        let secure = read_secure(&dirs);
        crate::i18n::init(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            "ru",
        );
        assert_ne!(
            crate::i18n::tr_lang("ru", "Start the bot"),
            "Start the bot",
            "the Russian command description must come from the catalog"
        );
        let client = Client::new(
            secure.token,
            &secure.proxy,
            "https://api.telegram.org",
        )
        .unwrap();
        client.set_commands().expect("register start/status");
        println!("commands registered in ru, en and zh");
    }

    /// Live diagnosis of the reaction on a message this computer already
    /// handled: prints what Telegram answers instead of guessing.
    #[test]
    #[ignore]
    fn real_bot_api_reaction_on_a_stored_message() {
        let dirs = crate::config::AppDirs {
            root: std::env::var_os("APPDATA")
                .map(std::path::PathBuf::from)
                .unwrap()
                .join("voice-button"),
            models: std::path::PathBuf::new(),
        };
        let secure = read_secure(&dirs);
        println!(
            "token: {} chars, unchanged by trim: {}, whitespace inside: {}",
            secure.token.chars().count(),
            secure.token == secure.token.trim(),
            secure.token.chars().any(char::is_whitespace),
        );
        println!("proxy: {:?}", secure.proxy);
        let client = match Client::new(
            secure.token.clone(),
            &secure.proxy,
            "https://api.telegram.org",
        ) {
            Ok(client) => client,
            Err(error) => {
                println!("client: {error:#}");
                return;
            }
        };
        match client.call("getMe", &json!({})) {
            Ok(me) => println!("getMe: ok, bot @{}", me["result"]["username"]),
            Err(error) => println!("getMe failed: {error:#}"),
        }
        let entries = chat_page(&dirs, ChatRange::All, 5, 0);
        println!("stored messages: {}", entries.len());
        for entry in &entries {
            println!(
                "  chat {} message {} at {} kind {}",
                entry.chat_id, entry.message_id, entry.at, entry.kind
            );
        }
        let Some(entry) = entries.first() else {
            println!("nothing in the chat table, nothing to react to");
            return;
        };
        // Which of the candidate marks Telegram actually accepts.
        for emoji in [
            "👀", "✅", "✔", "☑", "👍", "👌", "🎉", "💯", "🏆", "🙏", "🤝", "🔥", "❤", "👎",
            "❌", "⚠", "🚫", "💔", "🤬", "😢", "🤨", "🫡", "📝", "✍",
        ] {
            let answer = client.react(entry.chat_id, entry.message_id, &[emoji]);
            println!(
                "react {emoji}: {}",
                match answer {
                    Ok(()) => "allowed".to_string(),
                    Err(error) => format!("{error:#}"),
                }
            );
        }
        // Leave the message with the mark the bot itself would end on.
        let _ = client.react(entry.chat_id, entry.message_id, &[REACTION_DONE]);
    }

    #[test]
    #[ignore]
    fn real_bot_api_answers_get_me() {
        let dirs = crate::config::AppDirs {
            root: std::env::var_os("APPDATA")
                .map(std::path::PathBuf::from)
                .unwrap()
                .join("voice-button"),
            models: std::path::PathBuf::new(),
        };
        let secure = read_secure(&dirs);
        assert!(!secure.token.is_empty(), "run the PowerShell step that stores the token");
        let client = Client::new(
            secure.token,
            &secure.proxy,
            "https://api.telegram.org",
        )
        .unwrap();
        let me = client.call("getMe", &json!({})).unwrap();
        let username = me["result"]["username"].as_str().unwrap_or("?");
        println!("bot username: {username}");
    }

    #[test]
    fn the_queue_survives_a_reload_and_the_owner_can_be_reset() {
        let dirs = dirs("reload");
        let mut state = TelegramState::default();
        state.owner = Some(99);
        state.queue.push_back(Job {
            update_id: 5,
            chat_id: 1,
            message_id: 2,
            file_id: "f".into(),
            file_name: "pending.ogg".into(),
            kind: "voice".into(),
            sender: String::new(),
            original_at: String::new(),
            size: 10,
            seconds: 0.0,
            done_seconds: 0.0,
            stage: "ожидает".into(),
            progress: 0.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        });
        save_state(&dirs, &state);
        let loaded = load_state(&dirs);
        assert_eq!(loaded.owner, Some(99));
        assert_eq!(loaded.queue.len(), 1);
        assert_eq!(loaded.queue[0].file_name, "pending.ogg");
        reset_owner(&dirs.root);
        assert_eq!(load_state(&dirs).owner, None, "the reset clears the owner");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn the_original_message_date_is_preferred_over_the_forward_date() {
        let plain = json!({"date": 1_700_000_000});
        assert!(!message_date(&plain).is_empty());
        assert!(telegram_date_label_with_zone(&message_date(&plain)).starts_with('['));

        let forwarded = json!({
            "date": 1_700_000_100,
            "forward_origin": {"type": "user", "date": 1_600_000_000,
                               "sender_user": {"first_name": "Борис"}}
        });
        let parsed = chrono::DateTime::parse_from_rfc3339(&message_date(&forwarded)).unwrap();
        assert_eq!(parsed.timestamp(), 1_600_000_000);
    }

    #[test]
    fn the_sender_is_the_forwarded_person_or_the_sender_name() {
        assert_eq!(
            sender_label(&json!({"from":{"first_name":"Анна","last_name":"Петрова","username":"anna"}})),
            "Анна Петрова (@anna)"
        );
        assert_eq!(
            sender_label(&json!({"from":{"username":"just_user"}})),
            "@just_user"
        );
        assert_eq!(
            sender_label(&json!({"from":{"id":1},"forward_origin":{"type":"user","sender_user":{"first_name":"Борис","username":"b"}}})),
            "Борис (@b)"
        );
        assert_eq!(
            sender_label(&json!({"from":{"first_name":"Анна"},"forward_origin":{"type":"hidden_user","sender_user_name":"Скрытый"}})),
            "Скрытый"
        );
        assert_eq!(
            sender_label(&json!({"from":{"id":1}})),
            tr("User")
        );
    }

    #[test]
    fn every_media_kind_is_recognised_with_its_name() {
        let kinds = [
            ("voice", "voice", "voice.ogg"),
            ("audio", "audio", "audio.mp3"),
            ("video", "video", "video.mp4"),
            ("video_note", "video_note", "voice.ogg"),
            ("document", "document", "report.wav"),
        ];
        for (field, kind, fallback) in kinds {
            let mut item = json!({
                "file_id": "id",
                "file_size": 1234,
                "duration": 7,
            });
            if field == "document" {
                item["file_name"] = json!(fallback);
            }
            let message = json!({ field: item });
            let job = extract_media(&message).expect("media is recognised");
            assert_eq!(job.kind, kind);
            assert_eq!(job.file_id, "id");
            assert_eq!(job.size, 1234);
            assert_eq!(job.seconds, 7.0);
            assert_eq!(job.file_name, fallback);
        }
        let text = json!({ "message": { "text": "привет" } });
        assert!(extract_media(&text).is_none(), "plain text is not media");
    }

    #[test]
    fn the_reaction_marks_are_ones_telegram_accepts() {
        // Telegram accepts only a fixed set of emoji. This was read off the live
        // API: a check mark, a cross and a warning sign are refused with
        // REACTION_INVALID, which is why the bot appeared to do nothing.
        const ALLOWED: [&str; 25] = [
            "👍", "👎", "❤", "🔥", "🥰", "👏", "😁", "🤔", "🤯", "😱", "🤬", "🎉", "🤩", "🙏",
            "👌", "🕊", "🤡", "🥱", "😍", "🐳", "🌭", "💯", "🤣", "⚡", "👀",
        ];
        for mark in [REACTION_WORKING, REACTION_DONE, REACTION_FAILED] {
            assert!(ALLOWED.contains(&mark), "{mark} is refused by Telegram");
        }
        for refused in ["✅", "✔", "☑", "❌", "⚠", "🚫"] {
            assert!(!ALLOWED.contains(&refused), "{refused} must not come back");
        }
    }

    #[test]
    fn a_failed_file_is_answered_as_an_error_and_marked_in_the_chat() {
        let dirs = dirs("error");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        let job = Job {
            update_id: 9,
            chat_id: 1,
            message_id: 9,
            file_id: "f".into(),
            file_name: "broken.ogg".into(),
            kind: "voice".into(),
            sender: String::new(),
            original_at: String::new(),
            size: 1,
            seconds: 0.0,
            done_seconds: 0.0,
            stage: "расшифровка".into(),
            progress: 0.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        };
        state.lock().unwrap().current = Some(job.clone());
        finish_job(&api, &dirs, &state, &job, Err(anyhow!("broken container")));
        assert!(api.sent()[0].contains(tr("Bot: could not transcribe: {error}").split("{error}").next().unwrap()));
        assert_eq!(api.reactions.lock().unwrap().last().unwrap().1, "👎");
        let entry = chat_page(&dirs, ChatRange::All, 1, 0)[0].clone();
        assert!(entry.error.is_some());
        assert!(entry.text.contains("broken container"));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn the_date_filter_counts_by_processing_time() {
        let dirs = dirs("range");
        let today = chrono::Utc::now();
        let three_days = today - chrono::Duration::days(3);
        let long_ago = today - chrono::Duration::days(40);
        let stamp = |moment: chrono::DateTime<chrono::Utc>| moment.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        {
            let store = open_store(&dirs.root).expect("store");
            let conn = store.conn();
            for (index, added) in [today, three_days, long_ago].iter().enumerate() {
                conn.execute(
                    crate::sql::query("telegram/test_chat_insert"),
                    rusqlite::params![
                        1i64,
                        index as i64 + 1,
                        "2001-01-01T00:00:00Z",
                        "User",
                        "voice",
                        "a.ogg",
                        1.0f64,
                        format!("message {index}"),
                        Option::<String>::None,
                        stamp(*added)
                    ],
                )
                .expect("insert");
            }
        }

        assert_eq!(chat_count(&dirs, ChatRange::Today), 1, "only today's message");
        assert_eq!(chat_count(&dirs, ChatRange::Week), 2, "today and three days ago");
        assert_eq!(chat_count(&dirs, ChatRange::All), 3, "everything");
        assert_eq!(chat_page(&dirs, ChatRange::Today, 10, 0).len(), 1);
        assert_eq!(chat_page(&dirs, ChatRange::All, 10, 0).len(), 3);
        // The date on the file is 2001 and must not decide anything.
        assert!(chat_page(&dirs, ChatRange::All, 10, 0)
            .iter()
            .all(|entry| entry.at.starts_with("2001-")));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn old_messages_without_a_processing_stamp_fall_back_to_their_date() {
        // Rows written before `added_at` existed have an empty stamp; the filter
        // must still find them, or the log looks empty to anyone with history.
        let dirs = dirs("legacy-range");
        let nine_days_ago = (chrono::Utc::now() - chrono::Duration::days(9))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        {
            let store = open_store(&dirs.root).expect("store");
            let conn = store.conn();
            for index in 0..3 {
                conn.execute(
                    crate::sql::query("telegram/test_chat_insert"),
                    rusqlite::params![
                        1i64,
                        index as i64 + 1,
                        nine_days_ago.as_str(),
                        "User",
                        "voice",
                        "old.ogg",
                        1.0f64,
                        format!("old {index}"),
                        Option::<String>::None,
                        ""
                    ],
                )
                .expect("insert");
            }
        }
        assert_eq!(chat_count(&dirs, ChatRange::Today), 0, "nothing was handled today");
        assert_eq!(chat_count(&dirs, ChatRange::Week), 0);
        assert_eq!(chat_count(&dirs, ChatRange::All), 3, "everything is still there");
        assert_eq!(chat_page(&dirs, ChatRange::All, 10, 0).len(), 3);
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn the_chat_keeps_the_order_the_bot_worked_through_it() {
        // The date on a message is the date of the original file and can be
        // anything: a forwarded note is old while it is processed today. The log
        // is the bot's own order, so the rows must follow the order they were
        // stored in, never the timestamp.
        let dirs = dirs("order");
        let api = Fake::new(1000);
        let today = ChatEntry {
            chat_id: 1,
            message_id: 7,
            at: "2026-11-13T12:00:00Z".to_string(),
            sender: "User".to_string(),
            kind: "voice".to_string(),
            file_name: "today.ogg".to_string(),
            seconds: 3.0,
            text: "processed first, dated today".to_string(),
            error: None,
        };
        let ancient = ChatEntry {
            chat_id: 1,
            message_id: 8,
            at: "2019-01-02T09:00:00Z".to_string(),
            sender: "User".to_string(),
            kind: "voice".to_string(),
            file_name: "ancient.ogg".to_string(),
            seconds: 3.0,
            text: "processed second, dated 2019".to_string(),
            error: None,
        };
        append_chat(&dirs.root, &today);
        append_chat(&dirs.root, &ancient);
        let _ = api;

        let entries = chat_page(&dirs, ChatRange::All, 10, 0);
        let texts: Vec<&str> = entries.iter().map(|entry| entry.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["processed first, dated today", "processed second, dated 2019"],
            "the order is the order of processing, not of the message dates"
        );
        // And it holds across pages: the second page continues where the first
        // stopped, whatever the dates say.
        let second = chat_page(&dirs, ChatRange::All, 1, 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].text, "processed second, dated 2019");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn silence_is_answered_not_left_unanswered() {
        let dirs = dirs("silence");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        let job = Job {
            update_id: 10,
            chat_id: 1,
            message_id: 10,
            file_id: "f".into(),
            file_name: "silence.ogg".into(),
            kind: "voice".into(),
            sender: String::new(),
            original_at: String::new(),
            size: 1,
            seconds: 1.0,
            done_seconds: 1.0,
            stage: "расшифровка".into(),
            progress: 100.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        };
        finish_job(&api, &dirs, &state, &job, Ok(String::new()));
        assert_eq!(api.sent()[0], tr("Bot: speech was not recognised."));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn the_telegram_background_is_violet_and_does_not_hide_the_phone() {
        let dirs = dirs("background");
        let mut config = crate::config::Config::default();
        config.telegram_enabled = true;
        let shared = Arc::new(Shared::new(config, Vec::new()));
        set_background(&shared, true);
        assert!(shared.lock().background_active(), "the tray must be violet");
        assert!(
            shared
                .lock()
                .background_has(crate::state::BackgroundJob::Telegram)
        );
        shared
            .lock()
            .background
            .insert(crate::state::BackgroundJob::Phone);
        set_background(&shared, false);
        assert!(
            shared.lock().background_active(),
            "clearing Telegram must not clear the phone"
        );
        shared
            .lock()
            .background
            .remove(&crate::state::BackgroundJob::Phone);
        assert!(!shared.lock().background_active());
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn wait_for_engine_sleeps_through_a_recording_and_resumes() {
        let dirs = dirs("wait");
        let mut config = crate::config::Config::default();
        config.telegram_enabled = true;
        let shared = Arc::new(Shared::new(config, Vec::new()));
        shared.lock().status = Status::Recording;
        let state = Arc::new(Mutex::new(TelegramState::default()));
        let (done, finished) = std::sync::mpsc::channel();
        let thread_shared = shared.clone();
        let thread_state = state.clone();
        let thread_dirs = dirs.clone();
        std::thread::spawn(move || {
            let _ = wait_for_engine(&thread_shared, &thread_state, &thread_dirs);
            let _ = done.send(());
        });
        assert!(
            finished.recv_timeout(Duration::from_millis(300)).is_err(),
            "Telegram must wait while the microphone holds the engine"
        );
        shared.lock().status = Status::Idle;
        finished
            .recv_timeout(Duration::from_secs(3))
            .expect("Telegram resumes when the engine is free");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn a_proxy_is_optional() {
        assert!(proxy_setting("").is_none());
        assert!(proxy_setting("   ").is_none());
        assert_eq!(
            proxy_setting("127.0.0.1:10408").as_deref(),
            Some("127.0.0.1:10408")
        );
    }

    #[test]
    fn telegram_pauses_when_the_phone_or_the_microphone_needs_the_engine() {
        let dirs = dirs("pause");
        let mut config = crate::config::Config::default();
        config.telegram_enabled = true;
        let shared = Arc::new(Shared::new(config, Vec::new()));
        assert!(telegram_may_decode(&shared.lock()), "idle means Telegram may run");
        for busy in [
            Status::Recording,
            Status::Transcribing,
            Status::File,
            Status::Recovering,
            Status::NoModel,
        ] {
            shared.lock().status = busy;
            assert!(
                !telegram_may_decode(&shared.lock()),
                "status {busy:?} pauses Telegram"
            );
        }
        shared.lock().status = Status::Idle;
        shared
            .lock()
            .background
            .insert(crate::state::BackgroundJob::Phone);
        assert!(!telegram_may_decode(&shared.lock()), "the phone pauses Telegram");
        shared
            .lock()
            .background
            .remove(&crate::state::BackgroundJob::Phone);
        shared
            .lock()
            .background
            .insert(crate::state::BackgroundJob::Telegram);
        assert!(
            telegram_may_decode(&shared.lock()),
            "Telegram's own violet mark must not pause itself"
        );
        shared.lock().recovering = true;
        assert!(!telegram_may_decode(&shared.lock()), "recovery pauses Telegram");
        shared.lock().recovering = false;
        shared.lock().file = Some(crate::ipc::FileSnapshot {
            running: true,
            total_seconds: 120.0,
            ..Default::default()
        });
        assert!(!telegram_may_decode(&shared.lock()), "an imported file pauses Telegram");
        shared.lock().file = None;
        shared.lock().stream = Some(crate::state::StreamSession {
            id: 1,
            model: None,
            sample_rate: 16_000,
            buffer: Arc::new(Mutex::new(Vec::new())),
            received_samples: 0,
            live_text: String::new(),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        assert!(!telegram_may_decode(&shared.lock()), "an API stream pauses Telegram");
        shared.lock().stream = None;
        shared.lock().config.telegram_enabled = false;
        assert!(!telegram_may_decode(&shared.lock()), "a disabled bot does nothing");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn the_token_is_encrypted_on_disk() {
        let dirs = dirs("secure");
        write_secure(&dirs, "123:secret-token", "http://127.0.0.1:10408").unwrap();
        let raw = std::fs::read(dirs.root.join("telegram.secure")).unwrap();
        assert!(!raw.windows(6).any(|window| window == b"secret"));
        let secure = read_secure(&dirs);
        assert_eq!(secure.token, "123:secret-token");
        assert_eq!(secure.proxy, "http://127.0.0.1:10408");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }
}
