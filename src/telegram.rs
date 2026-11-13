//! The Telegram side of Voice Button.
//!
//! One long-polling thread receives updates, one worker turns queued media into
//! text. The queue, the owner and the chat mirror live in `telegram.state.json`
//! so a restart resumes instead of forgetting. Only private chats are served;
//! strangers are ignored silently.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::asr::AsrClient;
use crate::config::AppDirs;
use crate::state::{Shared, Status};

const MAX_FILE: u64 = 20 * 1024 * 1024;
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
    unprotect(&bytes)
        .ok()
        .and_then(|plain| serde_json::from_slice(&plain).ok())
        .unwrap_or_default()
}

pub fn write_secure(dirs: &AppDirs, token: &str, proxy: &str) -> Result<()> {
    write_secure_root(&dirs.root, token, proxy)
}

pub fn write_secure_root(root: &Path, token: &str, proxy: &str) -> Result<()> {
    let plain = serde_json::to_vec(&Secure {
        token: token.to_string(),
        proxy: proxy.to_string(),
    })?;
    let protected = protect(&plain)?;
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
    fn send_document(&self, chat: i64, reply_to: i64, name: &str, bytes: &[u8]) -> Result<()>;
    fn react(&self, chat: i64, message: i64, emoji: &str) -> Result<()>;
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
    fn send_document(&self, chat: i64, reply_to: i64, name: &str, bytes: &[u8]) -> Result<()> {
        Client::send_document(self, chat, reply_to, name, bytes)
    }
    fn react(&self, chat: i64, message: i64, emoji: &str) -> Result<()> {
        Client::react(self, chat, message, emoji)
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

    fn send_document(&self, chat: i64, reply_to: i64, name: &str, bytes: &[u8]) -> Result<()> {
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

    fn react(&self, chat: i64, message: i64, emoji: &str) -> Result<()> {
        self.call(
            "setMessageReaction",
            &json!({
                "chat_id": chat,
                "message_id": message,
                "reaction": [{ "type": "emoji", "emoji": emoji }],
            }),
        )?;
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
}

impl ChatEntry {
    pub fn link(&self) -> String {
        format!(
            "tg://openmessage?chat_id={}&message_id={}",
            self.chat_id, self.message_id
        )
    }
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
    pub chat: Vec<ChatEntry>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub bot_name: String,
}

fn state_path(dirs: &AppDirs) -> PathBuf {
    dirs.root.join("telegram.state.json")
}

fn state_path_root(root: &Path) -> PathBuf {
    root.join("telegram.state.json")
}

fn load_state(dirs: &AppDirs) -> TelegramState {
    std::fs::read(state_path(dirs))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_state(dirs: &AppDirs, state: &TelegramState) {
    let path = state_path(dirs);
    let temporary = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(state) {
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(&temporary, &path);
        }
    }
}

/// Read the chat for the desktop window without touching the bot thread.
pub fn chat(dirs: &AppDirs) -> Vec<ChatEntry> {
    load_state(dirs).chat
}

pub fn reset_owner(root: &Path) {
    let path = state_path_root(root);
    let mut state: TelegramState = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    state.owner = None;
    let temporary = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec_pretty(&state) {
        if std::fs::write(&temporary, bytes).is_ok() {
            let _ = std::fs::rename(&temporary, &path);
        }
    }
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
    let state: TelegramState = std::fs::read(state_path_root(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
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
        return "Очередь пуста.".to_string();
    };
    let place = if state.current.is_some() {
        "сейчас".to_string()
    } else {
        format!("в очереди: {}", state.queue.len())
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
            format!("{:.0} с", remaining / speed.max(0.05))
        }
        _ => "—".to_string(),
    };
    format!(
        "Файл: {}\nЭтап: {}\nГотово: {:.1} / {:.1} с ({:.0}%)\nПозиция: {}\nETA: {}\n{}",
        if job.file_name.is_empty() {
            "без имени"
        } else {
            &job.file_name
        },
        if job.stage.is_empty() {
            "ожидает"
        } else {
            &job.stage
        },
        job.done_seconds,
        job.seconds,
        percent,
        place,
        eta,
        job.error
            .as_ref()
            .map(|error| format!("Ошибка: {error}"))
            .unwrap_or_else(|| "Ошибка: —".to_string())
    )
}

// --------------------------------------------------------------------- bot

fn settings(dirs: &AppDirs, shared: &Arc<Shared>) -> (bool, String, String, String) {
    let config = shared.lock().config.clone();
    let secure = read_secure(dirs);
    let proxy = if config.telegram_proxy.trim().is_empty() {
        secure.proxy
    } else {
        config.telegram_proxy
    };
    let base = if config.telegram_api_base.trim().is_empty() {
        API_REAL.to_string()
    } else {
        config.telegram_api_base
    };
    (config.telegram_enabled, secure.token, proxy, base)
}

/// Start the polling and worker threads. They live as long as the daemon and
/// wait quietly while the setting is off or no token is stored.
pub fn spawn(dirs: AppDirs, shared: Arc<Shared>, asr: Arc<Mutex<AsrClient>>) {
    let state = Arc::new(Mutex::new(load_state(&dirs)));

    let poll_dirs = dirs.clone();
    let poll_shared = shared.clone();
    let poll_state = state.clone();
    std::thread::spawn(move || {
        let mut last_problem = String::new();
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
        let outcome = process_job(&client, &dirs, &shared, &asr, &state, &job);
        finish_job(&client, &dirs, &state, &job, outcome);
    });
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
                "Я на связи. Пришлите голосовое, аудио, видео, видеокружок или файл с аудио — верну текст сюда же. /status покажет очередь.",
            )?;
        } else if owner == Some(from) {
            client.send_text(chat_id, message_id, "Бот уже подключён к вам.")?;
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
            "Пришлите голосовое, аудио, видео, видеокружок или файл с аудио.",
        )?;
        return Ok(());
    };
    if media.size > MAX_FILE {
        client.send_text(
            chat_id,
            message_id,
            &format!(
                "Файл больше 20 МБ ({} МБ). Telegram отдаёт ботам не больше 20 МБ; пришлите файл короче или разбейте его.",
                media.size / (1024 * 1024)
            ),
        )?;
        return Ok(());
    }
    client.react(chat_id, message_id, "👀")?;
    let mut job = media;
    job.update_id = update.get("update_id").and_then(Value::as_i64).unwrap_or(0);
    job.chat_id = chat_id;
    job.message_id = message_id;
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
            size,
            seconds: duration,
            done_seconds: 0.0,
            stage: "ожидает".to_string(),
            progress: 0.0,
            text: String::new(),
            error: None,
            received_at: String::new(),
            started_at: String::new(),
        });
    }
    None
}

fn process_job(
    client: &dyn Api,
    dirs: &AppDirs,
    shared: &Arc<Shared>,
    asr: &Arc<Mutex<AsrClient>>,
    state: &Arc<Mutex<TelegramState>>,
    job: &Job,
) -> Result<String> {
    let root = dirs.root.join("telegram");
    std::fs::create_dir_all(&root)?;
    {
        let mut state = state.lock().unwrap();
        if let Some(current) = state.current.as_mut() {
            current.stage = "скачивание".to_string();
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
            current.stage = "расшифровка".to_string();
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
    for chunk in samples.chunks(step.max(1)) {
        wait_for_engine(shared, state, dirs)?;
        {
            let mut state = state.lock().unwrap();
            if let Some(current) = state.current.as_mut() {
                current.stage = "расшифровка".to_string();
            }
            save_state(dirs, &state);
        }
        let reply = asr
            .lock()
            .unwrap()
            .decode("", chunk, crate::telegram_media::TARGET_RATE)
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

fn wait_for_engine(
    shared: &Arc<Shared>,
    state: &Arc<Mutex<TelegramState>>,
    dirs: &AppDirs,
) -> Result<()> {
    loop {
        {
            let data = shared.lock();
            if data.config.telegram_enabled
                && data.status == Status::Idle
                && data.session.is_none()
                && data.stream.is_none()
                && !data.recovering
                && !data.background_active()
            {
                return Ok(());
            }
            if !data.config.telegram_enabled {
                bail!("бот выключен");
            }
        }
        {
            let mut state = state.lock().unwrap();
            if let Some(current) = state.current.as_mut() {
                current.stage = "движок занят".to_string();
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
    let at = chrono::Local::now().to_rfc3339();
    let (text, error) = match outcome {
        Ok(text) if !text.trim().is_empty() => (text, None),
        Ok(_) => ("Речь не распознана.".to_string(), None),
        Err(error) => (
            format!("Не удалось расшифровать: {error:#}"),
            Some(format!("{error:#}")),
        ),
    };
    if error.is_none() {
        let config = crate::config::load_config(&dirs.root);
        let entry = crate::history::HistoryEntry {
            at: at.clone(),
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
    let send = if text.chars().count() <= 4096 {
        client.send_text(job.chat_id, job.message_id, &text)
    } else {
        let name = format!("telegram-{}.txt", job.update_id);
        client.send_document(job.chat_id, job.message_id, &name, text.as_bytes())
    };
    if let Err(problem) = send {
        crate::log::log(&format!("telegram: reply: {problem:#}"));
    }
    let _ = client.react(job.chat_id, job.message_id, "✅");
    let mut state = state.lock().unwrap();
    state.chat.push(ChatEntry {
        chat_id: job.chat_id,
        message_id: job.message_id,
        at,
        file_name: job.file_name.clone(),
        kind: job.kind.clone(),
        seconds: job.seconds,
        text: text.clone(),
        error,
    });
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
        fn send_document(&self, _chat: i64, _reply_to: i64, name: &str, _bytes: &[u8]) -> Result<()> {
            self.documents.lock().unwrap().push(name.to_string());
            Ok(())
        }
        fn react(&self, _chat: i64, message: i64, emoji: &str) -> Result<()> {
            self.reactions.lock().unwrap().push((message, emoji.to_string()));
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
        assert!(api.sent().iter().any(|text| text.contains("20 МБ")));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn only_the_owner_gets_status() {
        let dirs = dirs("status");
        let api = Fake::new(1000);
        let state = Arc::new(Mutex::new(TelegramState::default()));
        handle_update(&api, &dirs, &state, &start(1, 111, "/start")).unwrap();
        handle_update(&api, &dirs, &state, &start(2, 222, "/status")).unwrap();
        assert!(!api.sent().iter().any(|text| text.contains("Очередь")));
        handle_update(&api, &dirs, &state, &start(3, 111, "/status")).unwrap();
        assert!(api.sent().iter().any(|text| text.contains("Очередь")));
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
        assert_eq!(api.reactions.lock().unwrap()[0], (8, "✅".to_string()));
        let _ = std::fs::remove_dir_all(&dirs.root);
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
