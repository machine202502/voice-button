use crate::config::{Backend, Config};
pub use crate::history::{History, HistoryEntry};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Recording,
    Transcribing,
    /// No usable model selected: recording is unavailable and the tray icon
    /// is shown grayed out.
    NoModel,
}

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub model: String,
    pub file: usize,
    pub files: usize,
    /// Bytes downloaded of the current file.
    pub downloaded: u64,
    /// Expected size of the current file, if the server reported it.
    pub total: Option<u64>,
    /// Bytes of fully-downloaded files (before the current one).
    pub done_bytes: u64,
    /// Sum of expected sizes of fully-downloaded files.
    pub done_total: u64,
    pub done: bool,
    pub error: Option<String>,
    /// Set to ask the downloader thread to stop and clean up.
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

pub struct Data {
    pub status: Status,
    pub config: Config,
    /// Last successfully applied (persisted) model selection. A pending
    /// selection that is not usable yet (e.g. model still downloading) lives
    /// only in `config` until it becomes applicable.
    pub applied_backend: Backend,
    pub applied_whisper_model: String,
    pub applied_whisper_file: Option<String>,
    pub applied_onnx_model: String,
    pub applied_onnx_folder: Option<String>,
    pub applied_language: String,
    pub last_error: Option<String>,
    pub download: Option<DownloadProgress>,
    /// Streaming transcription preview, published by the daemon while a
    /// recording is in progress (UI-side only; the daemon leaves it empty).
    pub live_text: String,
    pub live_started_ms: i64,
    /// The settings window is waiting for the user to press a shortcut.
    pub capturing_hotkey: bool,
    pub history: History,
    pub quit: bool,
}

pub struct Shared {
    data: Mutex<Data>,
}

impl Shared {
    pub fn new(config: Config, history: impl Into<History>) -> Self {
        let history = history.into();
        Self {
            data: Mutex::new(Data {
                status: Status::Idle,
                applied_backend: config.backend,
                applied_whisper_model: config.whisper_model.clone(),
                applied_whisper_file: config.whisper_file.clone(),
                applied_onnx_model: config.onnx_model.clone(),
                applied_onnx_folder: config.onnx_folder.clone(),
                applied_language: config.language.clone(),
                config,
                last_error: None,
                download: None,
                live_text: String::new(),
                live_started_ms: 0,
                capturing_hotkey: false,
                history,
                quit: false,
            }),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap()
    }

    pub fn set_error(&self, msg: String) {
        self.lock().last_error = Some(msg);
    }

    pub fn quit_requested(&self) -> bool {
        self.lock().quit
    }

    pub fn set_download_file(&self, index: usize) {
        if let Some(dl) = self.lock().download.as_mut() {
            dl.file = index;
            dl.downloaded = 0;
            dl.total = None;
        }
    }

    pub fn set_download_total(&self, total: Option<u64>) {
        if let Some(dl) = self.lock().download.as_mut() {
            dl.total = total;
        }
    }

    pub fn set_download_progress(&self, downloaded: u64) {
        if let Some(dl) = self.lock().download.as_mut() {
            dl.downloaded = downloaded;
        }
    }

    /// Move the current file's counters into the overall done counters.
    pub fn complete_download_file(&self) {
        if let Some(dl) = self.lock().download.as_mut() {
            dl.done_bytes += dl.downloaded;
            dl.done_total += dl.total.unwrap_or(dl.downloaded);
            dl.downloaded = 0;
            dl.total = None;
        }
    }

    pub fn finish_download(&self) {
        if let Some(dl) = self.lock().download.as_mut() {
            dl.done = true;
            dl.downloaded = dl.total.unwrap_or(dl.downloaded);
        }
    }

    pub fn fail_download(&self, msg: String) {
        if let Some(dl) = self.lock().download.as_mut() {
            dl.error = Some(msg);
        }
    }

    pub fn clear_download(&self) {
        self.lock().download = None;
    }
}

pub fn load_history(root: &std::path::Path) -> History {
    History::load(root)
}
