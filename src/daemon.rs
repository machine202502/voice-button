//! Daemon-side glue that the API server talks to.
//!
//! Everything here needs the recorder and the session pipeline, and nothing
//! needs the tray, so it lives in the library: the end-to-end API test drives
//! exactly the same code the daemon runs, over a real HTTP socket.

use crate::asr::AsrClient;
use crate::config;
use crate::ipc;
use crate::log;
use crate::model_manager;
use crate::recorder::Recorder;
use crate::session::{self, SessionHandle, SessionStart};
use crate::sound;
use crate::state::{Interactor, ModelChoice, Session, Shared, Status, StreamSession};
use crate::tray::TrayCommand;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};

/// State the daemon loop and the recording sessions share. The recognizer lives
/// in a child process, so the daemon itself only needs the client handle.
pub struct Runtime {
    /// Audio captured by the microphone; the session drains it every 60 ms.
    pub buffer: Arc<Mutex<Vec<f32>>>,
    pub asr: Arc<Mutex<AsrClient>>,
    /// The session that is currently open, if any.
    pub session: Mutex<Option<SessionHandle>>,
    /// The imported file that is being transcribed, for cancellation.
    pub file: Mutex<Option<crate::session::FileHandle>>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

pub fn install(runtime: Runtime) {
    let _ = RUNTIME.set(runtime);
}

pub fn runtime() -> &'static Runtime {
    RUNTIME.get().expect("daemon runtime initialized")
}

pub fn try_runtime() -> Option<&'static Runtime> {
    RUNTIME.get()
}

/// Publish the status to the UI (file) and to the tray (icon).
pub fn set_status(
    shared: &Shared,
    tray_cmds: &mpsc::Sender<TrayCommand>,
    dirs: &config::AppDirs,
    status: Status,
) {
    shared.lock().status = status;
    ipc::write_status(&dirs.root, status);
    let _ = tray_cmds.send(TrayCommand::SetStatus(status));
}

/// Monotonic id of a recording session, so stops and results can be attributed
/// to the session that produced them. Starts at 1.
pub fn next_session_id() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::SeqCst) + 1
}

/// A model the API server may use is one this build offers and that is already
/// on disk.
pub fn model_choice_ready(dirs: &config::AppDirs, model: &ModelChoice) -> bool {
    model_manager::find_model(&model.id)
        .map(|known| known.format == model.backend)
        .unwrap_or(false)
        && model_manager::is_ready(&dirs.models, &model.id)
}

/// Recording is allowed only when the selected model is usable AND no
/// download is currently running.
pub fn recording_allowed(shared: &Shared, dirs: &config::AppDirs) -> bool {
    if shared.lock().recovering || shared.lock().status == Status::File {
        return false;
    }
    if !model_manager::selection_ready(&dirs.models, &shared.lock().config) {
        return false;
    }
    let download_busy = shared
        .lock()
        .download
        .as_ref()
        .map(|d| !d.done && d.error.is_none())
        .unwrap_or(false);
    !download_busy
}

pub fn recording_toggle_allowed(status: Status, recorder_active: bool) -> bool {
    if status == Status::Recovering || status == Status::File {
        return false;
    }
    recorder_active || status != Status::Transcribing
}

/// Turn incoming audio into one session: the caller's sound replaces the local
/// microphone, so nothing here touches the recorder.
pub fn begin_stream_session(
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<TrayCommand>,
    events_tx: mpsc::Sender<crate::state::AppEvent>,
    model: Option<ModelChoice>,
    sample_rate: u32,
) -> u64 {
    let runtime = runtime();
    let id = next_session_id();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    {
        let mut data = shared.lock();
        data.session = Some(Session {
            id,
            owner: Interactor::server(),
            model: model.clone(),
            stopped_by: None,
        });
        data.stream = Some(StreamSession {
            id,
            model: model.clone(),
            sample_rate,
            buffer: buffer.clone(),
            received_samples: 0,
            live_text: String::new(),
            stop: Arc::new(AtomicBool::new(false)),
        });
    }
    ipc::write_session(&dirs.root, shared.lock().session.as_ref());
    log::log(&format!(
        "api stream session {id} started ({sample_rate} Hz)"
    ));
    set_status(shared, tray_cmds, dirs, Status::Recording);
    let start = SessionStart {
        id,
        interactor: Interactor::server(),
        model,
        rate: sample_rate,
        append: false,
        paste_target: None,
        clipboard: false,
        buffer,
    };
    match session::start(
        shared.clone(),
        dirs.clone(),
        events_tx,
        runtime.asr.clone(),
        start,
    ) {
        Ok(handle) => *runtime.session.lock().unwrap() = Some(handle),
        Err(e) => {
            log::log(&format!("cannot start the API session: {e:#}"));
            let mut data = shared.lock();
            data.stream = None;
            data.session = None;
            drop(data);
            ipc::write_session(&dirs.root, None);
            set_status(shared, tray_cmds, dirs, Status::Idle);
            return id;
        }
    }
    let (play, volume) = {
        let data = shared.lock();
        (data.config.play_start_sound, data.config.sound_volume)
    };
    if play {
        sound::play_start(volume);
    }
    id
}

/// Stop receiving audio and let the session decode the whole buffer. Kept in one
/// place so the caller and the user's window share exactly one mechanism.
pub fn stop_stream_session(
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<TrayCommand>,
    interactor: Interactor,
) -> bool {
    let stopped = {
        let mut data = shared.lock();
        match data.stream.take() {
            Some(stream) => {
                // Tell the session to finish; it holds its own handles.
                stream.stop.store(true, Ordering::SeqCst);
                if let Some(session) = data.session.as_mut() {
                    if interactor.may_stop(session) {
                        session.stopped_by = Some(interactor);
                    }
                }
                true
            }
            None => false,
        }
    };
    if stopped {
        if let Some(runtime) = try_runtime() {
            if let Some(handle) = runtime.session.lock().unwrap().take() {
                handle.stop();
            }
        }
        log::log("api stream session stopped, transcribing the received audio");
        set_status(shared, tray_cmds, dirs, Status::Transcribing);
    }
    stopped
}

/// Answer one command from the API server. The daemon owns the transcription
/// session, so the HTTP thread asks here instead of holding a model.
pub fn handle_api_request(
    request: crate::api::ApiRequest,
    recorder: &mut Recorder,
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<TrayCommand>,
    events_tx: mpsc::Sender<crate::state::AppEvent>,
) {
    use crate::api::{ApiAction, ApiResponse};
    let reply = request.reply;
    match request.action {
        ApiAction::StreamStart { model, sample_rate } => {
            let busy = {
                let data = shared.lock();
                data.session.is_some()
                    || recorder.is_active()
                    || data.recovering
                    || matches!(data.status, Status::Recording | Status::Transcribing | Status::File)
            };
            if busy {
                // One session at a time: the user's dictation or another API
                // session is still running.
                let _ = reply.send(ApiResponse::Rejected(
                    "the app is busy with another session".to_string(),
                ));
                return;
            }
            let ready = match &model {
                Some(model) => model_choice_ready(dirs, model),
                None => recording_allowed(shared, dirs),
            };
            if !ready {
                let _ = reply.send(ApiResponse::Rejected(
                    "no downloaded model to transcribe with".to_string(),
                ));
                return;
            }
            let session = begin_stream_session(
                shared,
                dirs,
                tray_cmds,
                events_tx,
                model,
                sample_rate,
            );
            let _ = reply.send(ApiResponse::StreamStarted { session });
        }
        ApiAction::StreamChunk { session, samples } => {
            let buffer = {
                let data = shared.lock();
                match data.stream.as_ref().filter(|s| s.id == session) {
                    Some(stream) => Some(stream.buffer.clone()),
                    None => None,
                }
            };
            let Some(buffer) = buffer else {
                let _ = reply.send(ApiResponse::Rejected(
                    "no active stream session with that id".to_string(),
                ));
                return;
            };
            let count = samples.len() as u64;
            buffer.lock().unwrap().extend(samples);
            let (received_ms, live) = {
                let mut data = shared.lock();
                match data.stream.as_mut().filter(|s| s.id == session) {
                    Some(stream) => {
                        stream.received_samples += count;
                        let ms = stream
                            .received_samples
                            .saturating_mul(1000)
                            .saturating_div(stream.sample_rate.max(1) as u64);
                        (ms, stream.live_text.clone())
                    }
                    None => (0, String::new()),
                }
            };
            let _ = reply.send(ApiResponse::ChunkAccepted { received_ms, live });
        }
        ApiAction::StreamStop { session } => {
            let active = shared.lock().stream.as_ref().map(|stream| stream.id);
            match active {
                Some(id) if session == 0 || session == id => {
                    stop_stream_session(shared, dirs, tray_cmds, Interactor::server());
                    let _ = reply.send(ApiResponse::StreamStopped);
                }
                Some(_) => {
                    let _ = reply.send(ApiResponse::Rejected(
                        "that is not the active session".to_string(),
                    ));
                }
                None => {
                    let _ = reply.send(ApiResponse::StreamStopped);
                }
            }
        }
    }
}
