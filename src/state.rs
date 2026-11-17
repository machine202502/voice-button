use crate::config::{Backend, Config};
pub use crate::history::{History, HistoryChunk, HistoryEntry, Source};
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Recording,
    Transcribing,
    /// A recording was interrupted by a crash and is being transcribed now:
    /// the microphone is blocked and the tray icon is blue.
    Recovering,
    /// A file chosen by the user is being transcribed: the tray icon is green
    /// and the microphone is unavailable.
    File,
    /// No usable model selected: recording is unavailable and the tray icon
    /// is shown grayed out.
    NoModel,
}

/// Event sent from a session or the recovery thread to the daemon loop.
#[derive(Debug)]
pub enum AppEvent {
    Transcribed {
        text: String,
        model: String,
        append: bool,
        paste_target: Option<isize>,
        /// False for API sessions: the caller's audio never touches the
        /// clipboard of the user.
        clipboard: bool,
        state: crate::history::EntryState,
        /// Recording directory to clean up once the text is saved.
        dir: Option<std::path::PathBuf>,
        /// Who produced the audio: the user, the API, or an imported file.
        source: crate::history::Source,
        /// Audio that belongs to the entry (a kept recording or the user's file).
        audio: Option<String>,
        /// Timestamp for the history entry. The phone owns the time of its own
        /// messages, so it travels with the event instead of being "now".
        at: Option<String>,
        /// The session that produced this text. A live session and a lazy take
        /// can be open at once, so the result names its owner explicitly.
        session: u64,
        /// A lazy API take: background work, so no end cue and no status change.
        lazy: bool,
    },
    /// A file transcription was cancelled by the user.
    Cancelled,
    Failed {
        message: String,
        /// A lazy take: the failure must not clear the live session's slots.
        lazy: bool,
    },
    /// A crash-interrupted recording was transcribed into the history.
    Recovered {
        chars: usize,
        failed_segments: usize,
        at: String,
    },
}

/// Who is asking the app to record. Every input path (button, hotkey, tray
/// click, API request) speaks through this abstraction, so sounds, the tray
/// icon and history attribution work the same for all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum InteractorKind {
    /// A human being, via the window, a global shortcut or the tray icon.
    #[default]
    User,
    /// The local API server, on behalf of a page or a script.
    Server,
}

impl InteractorKind {
    /// Name used in JSON snapshots and the API.
    pub fn label(self) -> &'static str {
        match self {
            InteractorKind::User => "user",
            InteractorKind::Server => "server",
        }
    }

    pub fn from_label(value: &str) -> Self {
        match value {
            "server" => InteractorKind::Server,
            _ => InteractorKind::User,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interactor {
    pub kind: InteractorKind,
}

impl Interactor {
    pub fn user() -> Self {
        Self {
            kind: InteractorKind::User,
        }
    }

    pub fn server() -> Self {
        Self {
            kind: InteractorKind::Server,
        }
    }

    pub fn is_user(&self) -> bool {
        self.kind == InteractorKind::User
    }

    /// A human user may stop any session; the API server only its own. This is
    /// the whole privilege model: the user's dictation is never interrupted by
    /// the server.
    pub fn may_stop(&self, session: &Session) -> bool {
        self.is_user() || session.owner.kind == InteractorKind::Server
    }
}

/// A model the API server may use for the sessions it starts, without touching
/// the model the user picked in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub backend: Backend,
    pub id: String,
}

impl ModelChoice {
    /// Stable identifier used by the API: `Onnx/gigaam-v3-rnnt`.
    pub fn api_id(&self) -> String {
        format!("{}/{}", self.backend.label(), self.id)
    }

    /// Parse an API model identifier. `None` for anything that is not
    /// `Engine/id` with a known engine.
    pub fn parse(text: &str) -> Option<Self> {
        let (engine, id) = text.trim().split_once('/')?;
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        let backend = match engine.trim().to_ascii_lowercase().as_str() {
            "whisper" => Backend::Whisper,
            "onnx" => Backend::Onnx,
            _ => return None,
        };
        Some(Self {
            backend,
            id: id.to_string(),
        })
    }

    /// Apply the selection to a copy of the user's settings.
    pub fn apply_to(&self, cfg: &mut Config) {
        cfg.backend = self.backend;
        match self.backend {
            Backend::Whisper => {
                cfg.whisper_model = self.id.clone();
                cfg.whisper_file = None;
            }
            Backend::Onnx => {
                cfg.onnx_model = self.id.clone();
                cfg.onnx_folder = None;
            }
        }
    }
}

/// A recording session: who started it, with which model, and who ended it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub owner: Interactor,
    /// Model override for this session only; `None` uses the user's settings.
    pub model: Option<ModelChoice>,
    /// Set when the session is being stopped (kept through transcription so the
    /// result can say who ended it).
    pub stopped_by: Option<Interactor>,
}

impl Session {
    /// Model shown in history for this session.
    pub fn model_label(&self) -> Option<String> {
        self.model.as_ref().map(ModelChoice::api_id)
    }
}

/// Audio pushed by an API caller instead of the local microphone: the page (or
/// script) captures its own sound and sends PCM here.
pub struct StreamSession {
    pub id: u64,
    /// Model chosen by the caller for this session only.
    pub model: Option<ModelChoice>,
    /// Sample rate of the incoming mono PCM.
    pub sample_rate: u32,
    /// Mono f32 samples, appended by the API thread.
    pub buffer: Arc<Mutex<Vec<f32>>>,
    pub received_samples: u64,
    /// Latest live preview produced by the stream worker.
    pub live_text: String,
    /// Set when the session stops receiving; the worker then decodes the final
    /// text from the whole buffer.
    pub stop: Arc<AtomicBool>,
}

impl StreamSession {
    /// Milliseconds of audio received so far.
    pub fn received_ms(&self) -> u64 {
        self.received_samples
            .saturating_mul(1000)
            .saturating_div(self.sample_rate.max(1) as u64)
    }
}

/// Result of the last API-owned session, readable by the caller that started
/// it. The server never sees the user's own dictations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiResult {
    pub session: u64,
    pub text: String,
    pub model: String,
    pub at: String,
    pub stopped_by: InteractorKind,
    pub error: Option<String>,
}

/// A lazy API take: audio from a caller that is transcribed whenever the engine
/// is free, so the microphone stays available and nothing is queued in front of
/// the person. It has no `Session` and no `Status::Recording`: that is exactly
/// what tells the phone, the API and the tray that this is background work.
pub struct LazyTake {
    pub id: u64,
    pub model: Option<ModelChoice>,
    pub sample_rate: u32,
    /// Mono f32 samples, appended by the API thread and drained by the session.
    pub buffer: Arc<Mutex<Vec<f32>>>,
    pub received_samples: u64,
    /// Latest preview produced by the session thread.
    pub live_text: String,
    /// The caller said stop: no more audio is accepted.
    pub closed: bool,
    /// Segments cut and not decoded yet.
    pub queued: usize,
    /// The engine belongs to the person right now, so those segments wait.
    pub waiting: bool,
}

/// Milliseconds of audio received by a lazy take.
impl LazyTake {
    pub fn received_ms(&self) -> u64 {
        self.received_samples
            .saturating_mul(1000)
            .saturating_div(self.sample_rate.max(1) as u64)
    }
}

/// Session info published to the UI process (it cannot read the daemon's
/// memory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    pub id: u64,
    pub owner: InteractorKind,
    pub model: Option<String>,
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
    /// Same as above, for the append-to-last-message shortcut.
    pub capturing_append_hotkey: bool,
    /// Same as above, for the record-and-paste shortcut.
    pub capturing_paste_hotkey: bool,
    /// Input device names shown in the Settings microphone picker, cached for
    /// a couple of seconds while the window is open.
    pub input_devices: Vec<String>,
    pub input_devices_at: Option<std::time::Instant>,
    pub history: History,
    pub quit: bool,
    /// Runtime state of the local API server, as published by the daemon and
    /// mirrored into the UI.
    pub api: Option<ApiStatus>,
    /// The recording session that is currently open (daemon-side, authoritative);
    /// it stays set while the recording is being transcribed.
    pub session: Option<Session>,
    /// Same session, mirrored into the UI process through SQLite.
    pub session_view: Option<SessionView>,
    /// A crash-interrupted recording is being transcribed: the microphone is
    /// blocked until it is done.
    pub recovering: bool,
    /// Active audio upload from an API caller; `None` when the app is not
    /// transcribing uploaded sound.
    pub stream: Option<StreamSession>,
    /// Last result produced for an API session.
    pub api_result: Option<ApiResult>,
    /// Progress of an imported file, mirrored from SQLite.
    pub file: Option<crate::ipc::FileSnapshot>,
    /// When the file state was read last, so the tab polls at a sane rate.
    pub file_seen: Option<std::time::Instant>,
    /// Runtime state of the phone server, mirrored from SQLite.
    pub site: Option<crate::ipc::SiteSnapshot>,
    /// Tasks running behind the person's back right now. The tray icon turns
    /// violet and no sound is played for any of them; the phone is one, and
    /// more will come, so this is a set and not a flag.
    pub background: std::collections::BTreeSet<BackgroundJob>,
    /// A lazy API take: audio from a caller, transcribed when the engine is
    /// free. `None` when no such take is open.
    pub lazy: Option<LazyTake>,
    /// A pairing link the window can show as a QR code, read from
    /// the phone database (the daemon owns it; the window only mirrors the
    /// client record).
    pub site_pairing: Option<SitePairing>,
    /// What the internet-access script did last time, read from SQLite.
    pub site_network: Option<crate::site::network::State>,
    /// The script is running right now.
    pub site_network_busy: bool,
}

/// Work the person is not waiting for in front of the window: the tray icon
/// turns violet and no sound is played for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BackgroundJob {
    /// The phone's audio is being received or transcribed.
    Phone,
    /// A Telegram file is being downloaded, decoded or transcribed.
    Telegram,
    /// A lazy API take the engine is decoding right now.
    Api,
}

impl Data {
    /// Is any background task running? The violet icon means exactly this.
    pub fn background_active(&self) -> bool {
        !self.background.is_empty()
    }

    /// Is this particular background task running? Telegram waits for the
    /// phone, not for its own violet icon.
    pub fn background_has(&self, job: BackgroundJob) -> bool {
        self.background.contains(&job)
    }

    /// Is the recognizer somebody else's right now? The phone's queue and a
    /// lazy API take both ask this before every segment: the person at the
    /// computer (and the live API session) always comes first.
    ///
    /// A lazy take is deliberately absent from this list — it is the state
    /// this predicate protects, not a claim on the engine.
    pub fn engine_busy(&self) -> bool {
        self.status != Status::Idle
            || self.session.is_some()
            || self.stream.is_some()
            || self.recovering
            || self
                .file
                .as_ref()
                .map(|file| file.running || file.total_seconds <= 0.0)
                .unwrap_or(false)
    }
}

/// Runtime status of the local API server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiStatus {
    pub running: bool,
    pub host: String,
    pub port: u16,
    pub error: Option<String>,
}

/// The pairing link a phone may still use: its token and deadline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SitePairing {
    pub token: String,
    /// When the link stops working, in the same format as the stored stamp.
    pub until: String,
}

impl SitePairing {
    /// True while the link is inside its five minutes. Both stamps come from
    /// the phone-independent `rfc3339` clock the daemon stores.
    pub fn alive(&self, now: &str) -> bool {
        !self.token.is_empty() && !self.until.is_empty() && self.until.as_str() > now
    }
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
                capturing_append_hotkey: false,
                capturing_paste_hotkey: false,
                input_devices: Vec::new(),
                input_devices_at: None,
                history,
                quit: false,
                api: None,
                session: None,
                session_view: None,
                recovering: false,
                file: None,
                file_seen: None,
                site: None,
                background: std::collections::BTreeSet::new(),
                lazy: None,
                site_pairing: None,
                site_network: None,
                site_network_busy: false,
                stream: None,
                api_result: None,
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

#[cfg(test)]
mod tests {
    use super::{
        BackgroundJob, Interactor, LazyTake, ModelChoice, Session, Shared, Status, StreamSession,
    };
    use crate::config::{Backend, Config};
    use crate::dates::DateFilter;
    use crate::history::History;
    use crate::site::db::{message_count, message_page, monolog_count, monolog_page, Db};
    use crate::site::monolog::Stage;
    use crate::store::Store;
    use std::sync::Arc;

    #[test]
    fn the_window_reads_saved_monologs_and_messages_by_page() {
        let root = std::env::temp_dir().join(format!("vb-state-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        {
            let mut db = Db::open(&root).unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            let kept = db.add_message(
                monolog.id,
                "2026-11-12T10:01:00+05:00",
                "2026-11-12T10:01:02+05:00",
            );
            db.add_seconds(kept.id, 3.0);
            db.set_stage(kept.id, Stage::Queued);
            db.set_stage(kept.id, Stage::Transcribing);
            db.set_progress(kept.id, "первый текст", 1);
            db.set_stage(kept.id, Stage::Done);
            let hidden = db.add_message(
                monolog.id,
                "2026-11-12T10:02:00+05:00",
                "2026-11-12T10:02:01+05:00",
            );
            db.delete(hidden.id);
            db.archive_monolog(monolog.id, true);
            db.save().unwrap();
        }
        // The window opens the file the daemon left, never its own copy.
        let store = Store::open(&root).unwrap();
        assert_eq!(monolog_count(&store, true, "", DateFilter::everything()), 1);
        let monolog = &monolog_page(&store, true, "", DateFilter::everything(), 0, 10)[0];
        assert_eq!(monolog.name, "Работа");
        assert!(monolog.archived, "an archived monolog is still listed");
        assert_eq!(monolog.count, 1, "a deleted message is not counted");
        assert_eq!(message_count(&store, monolog.id), 1);
        let message = &message_page(&store, monolog.id, 0, 10)[0];
        assert_eq!(message.text, "первый текст");
        assert_eq!(message.stage, Stage::Done);
        let _ = std::fs::remove_dir_all(root);
    }

    fn session(owner: Interactor) -> Session {
        Session {
            id: 1,
            owner,
            model: None,
            stopped_by: None,
        }
    }

    #[test]
    fn the_user_stops_any_session_and_the_server_only_its_own() {
        let user_session = session(Interactor::user());
        let server_session = session(Interactor::server());
        assert!(Interactor::user().may_stop(&user_session));
        assert!(Interactor::user().may_stop(&server_session));
        assert!(!Interactor::server().may_stop(&user_session));
        assert!(Interactor::server().may_stop(&server_session));
    }

    #[test]
    fn model_choice_round_trips_through_its_api_id() {
        for text in ["Onnx/gigaam-v3-rnnt", "Whisper/medium-q5_0"] {
            let choice = ModelChoice::parse(text).expect("valid id");
            assert_eq!(choice.api_id(), text);
        }
        assert!(ModelChoice::parse("gigaam-v3-rnnt").is_none());
        assert!(ModelChoice::parse("Other/model").is_none());
        assert!(ModelChoice::parse("Onnx/").is_none());
    }

    #[test]
    fn a_model_override_does_not_leak_into_the_user_settings() {
        let mut cfg = Config::default();
        cfg.backend = Backend::Onnx;
        cfg.onnx_model = "gigaam-v3-rnnt-punct".to_string();
        let mut overridden = cfg.clone();
        ModelChoice::parse("Whisper/medium-q5_0")
            .unwrap()
            .apply_to(&mut overridden);
        assert_eq!(overridden.backend, Backend::Whisper);
        assert_eq!(overridden.whisper_model, "medium-q5_0");
        // The user's own selection is untouched.
        assert_eq!(cfg.backend, Backend::Onnx);
        assert_eq!(cfg.onnx_model, "gigaam-v3-rnnt-punct");
    }

    fn fresh_shared() -> Shared {
        Shared::new(Config::default(), History::from_entries(Vec::new()))
    }

    fn lazy_take(id: u64) -> LazyTake {
        LazyTake {
            id,
            model: None,
            sample_rate: 16_000,
            buffer: Arc::new(std::sync::Mutex::new(Vec::new())),
            received_samples: 0,
            live_text: String::new(),
            closed: false,
            queued: 0,
            waiting: false,
        }
    }

    /// The whole point of the lazy mode: it is a user of the engine, not an
    /// owner of it. The microphone stays available and the phone keeps moving.
    #[test]
    fn a_lazy_take_does_not_claim_the_engine() {
        let shared = fresh_shared();
        assert!(!shared.lock().engine_busy());
        shared.lock().lazy = Some(lazy_take(3));
        let data = shared.lock();
        assert_eq!(data.status, Status::Idle);
        assert!(data.session.is_none(), "a lazy take is not a session");
        assert!(data.stream.is_none(), "a lazy take is not a live stream");
        assert!(
            !data.engine_busy(),
            "the microphone and the phone must stay free while a lazy take waits"
        );
        assert!(!data.background_has(BackgroundJob::Api));
    }

    #[test]
    fn every_live_state_still_claims_the_engine() {
        let streaming = fresh_shared();
        streaming.lock().stream = Some(StreamSession {
            id: 1,
            model: None,
            sample_rate: 16_000,
            buffer: Arc::new(std::sync::Mutex::new(Vec::new())),
            received_samples: 0,
            live_text: String::new(),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        assert!(
            streaming.lock().engine_busy(),
            "a live API session owns the engine"
        );

        let dictating = fresh_shared();
        dictating.lock().status = Status::Recording;
        assert!(
            dictating.lock().engine_busy(),
            "the person's dictation owns the engine"
        );

        let sessioned = fresh_shared();
        sessioned.lock().session = Some(Session {
            id: 2,
            owner: Interactor::server(),
            model: None,
            stopped_by: None,
        });
        assert!(sessioned.lock().engine_busy());
    }
}
