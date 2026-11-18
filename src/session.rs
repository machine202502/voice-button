//! One dictation session: drains captured audio, writes it to disk, cuts it
//! into segments, asks the ASR worker for text and publishes the growing
//! transcript.
//!
//! The session is the only place that touches the recognizer, so the rules that
//! keep the app alive live here: audio is on disk before it is decoded, a
//! segment is never longer than `MAX_SEGMENT`, and a failing engine costs the
//! session nothing but that segment.

use crate::asr::Reply;
use crate::asr_engine::{Engine, Priority};
use crate::audio::{read_wav, Resampler, WavWriter};
use crate::config::AppDirs;
use crate::history::EntryState;
use crate::ipc;
use crate::log;
use crate::segmenter::{Segment, Segmenter, MIN_SPEECH};
use crate::state::{AppEvent, BackgroundJob, Interactor, InteractorKind, ModelChoice, Shared};
use crate::stitch::Stitcher;
use crate::transcribe;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Audio handed to the recognizer is 16 kHz mono, like every model expects.
pub const SAMPLE_RATE: u32 = 16_000;

/// How a session gets the engine.
///
/// A live session asks for it immediately: the person is speaking, or an API
/// caller is streaming and waiting for text. A lazy API take waits instead —
/// it is background work, so a segment waits while the person at the computer
/// (or the phone's queue) holds the engine, and goes in as soon as it is free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMode {
    Live,
    Lazy,
}

impl SessionMode {
    pub fn is_lazy(self) -> bool {
        self == SessionMode::Lazy
    }

    /// The word the API uses for this mode.
    pub fn label(self) -> &'static str {
        match self {
            SessionMode::Live => "live",
            SessionMode::Lazy => "lazy",
        }
    }
}

/// The recognizer a session talks to. Production passes the scheduler in
/// `asr_engine`; tests pass a fake and look at what was asked of it.
pub trait Recognizer: Send + Sync {
    fn decode(&self, priority: Priority, model: &str, samples: &[f32], rate: u32) -> Result<Reply>;
}

impl Recognizer for Engine {
    fn decode(&self, priority: Priority, model: &str, samples: &[f32], rate: u32) -> Result<Reply> {
        Engine::decode(self, priority, model, samples, rate)
    }
}

#[derive(Debug, Clone)]
pub struct SessionStart {
    pub id: u64,
    pub interactor: Interactor,
    pub model: Option<ModelChoice>,
    /// Rate of the audio that arrives in `buffer`.
    pub rate: u32,
    pub append: bool,
    pub paste_target: Option<isize>,
    pub clipboard: bool,
    pub buffer: Arc<Mutex<Vec<f32>>>,
    pub mode: SessionMode,
    /// Real time allowed for the tail once the stream stops, for an exclusive
    /// API session. `None` finishes the tail however long it takes (the user's
    /// own dictation and a lazy take have no budget).
    pub tail_budget: Option<Duration>,
}

pub struct SessionHandle {
    pub id: u64,
    pub dir: PathBuf,
    control: mpsc::Sender<Control>,
}

enum Control {
    Stop,
}

impl SessionHandle {
    /// Ask the session to finish: the tail is decoded and the result is sent to
    /// the daemon as `AppEvent::Transcribed`.
    pub fn stop(&self) {
        let _ = self.control.send(Control::Stop);
    }
}

/// What a recording directory remembers across a crash.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedState {
    pub id: u64,
    pub started: String,
    pub interactor: InteractorKind,
    pub model: String,
    pub seconds: f64,
    pub done: bool,
    #[serde(default)]
    pub failed_segments: usize,
    /// Recovery gave up on this recording: it is kept on disk but is not
    /// retried at every start, otherwise a model that always crashes would
    /// crash the app on every launch.
    #[serde(default)]
    pub failed: bool,
    /// Kept because the transcription is not guaranteed and the history links
    /// to it: never deleted automatically by the finished-recording sweep.
    #[serde(default)]
    pub kept: bool,
}

pub fn recordings_dir(root: &Path) -> PathBuf {
    root.join("recordings")
}

fn state_root(dir: &Path) -> Option<&Path> {
    dir.parent()?.parent()
}

fn recording_name(dir: &Path) -> Option<String> {
    Some(dir.file_name()?.to_string_lossy().to_string())
}

pub fn audio_path(dir: &Path) -> PathBuf {
    dir.join("audio.wav")
}

fn write_state(dir: &Path, state: &PersistedState) {
    let (Some(root), Some(name)) = (state_root(dir), recording_name(dir)) else {
        return;
    };
    let Ok(store) = crate::store::Store::open(root) else {
        return;
    };
    let _ = store.conn().execute(
        crate::sql::query("session/write"),
        rusqlite::params![
            name,
            state.id as i64,
            state.started,
            state.interactor.label(),
            state.model,
            state.seconds,
            state.done as i64,
            state.failed_segments as i64,
            state.failed as i64,
            state.kept as i64
        ],
    );
}

pub fn read_state(dir: &Path) -> Option<PersistedState> {
    let (root, name) = (state_root(dir)?, recording_name(dir)?);
    let store = crate::store::Store::open(root).ok()?;
    let conn = store.conn();
    conn.query_row(
        crate::sql::query("session/read"),
        [name],
        |row| {
            Ok(PersistedState {
                id: row.get::<_, i64>(0)? as u64,
                started: row.get(1)?,
                interactor: crate::state::InteractorKind::from_label(&row.get::<_, String>(2)?),
                model: row.get(3)?,
                seconds: row.get(4)?,
                done: row.get::<_, i64>(5)? != 0,
                failed_segments: row.get::<_, i64>(6)? as usize,
                failed: row.get::<_, i64>(7)? != 0,
                kept: row.get::<_, i64>(8)? != 0,
            })
        },
    )
    .ok()
}

fn delete_state(dir: &Path) {
    let (Some(root), Some(name)) = (state_root(dir), recording_name(dir)) else {
        return;
    };
    if let Ok(store) = crate::store::Store::open(root) {
        let _ = store.conn().execute(crate::sql::query("session/delete"), [name]);
    }
}

/// Recording directories that were never finished: the daemon died mid-recording.
pub fn pending(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(recordings_dir(root)) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        match read_state(&dir) {
            Some(state) if !state.done && !state.failed => out.push(dir),
            _ => {}
        }
    }
    out.sort();
    out.reverse();
    out
}

/// Remove directories that are finished or unreadable (left behind by an
/// earlier run that died between saving the text and cleaning up).
pub fn clean_finished(root: &Path) {
    let Ok(entries) = std::fs::read_dir(recordings_dir(root)) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let state = read_state(&dir);
        let trash = match &state {
            Some(state) => state.done && !state.kept,
            // An unreadable state file is a leftover, not a user file.
            None => true,
        };
        if trash {
            let _ = std::fs::remove_dir_all(&dir);
            delete_state(&dir);
        }
    }
}

pub fn start(
    shared: Arc<Shared>,
    dirs: AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<dyn Recognizer>,
    start: SessionStart,
) -> Result<SessionHandle> {
    let dir = recordings_dir(&dirs.root).join(format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        start.id
    ));
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let (tx, rx) = mpsc::channel();
    let handle_dir = dir.clone();
    let id = start.id;
    std::thread::Builder::new()
        .name(format!("session-{id}"))
        .spawn(move || run(shared, dirs, events, asr, start, dir, rx))
        .context("spawn the recording session")?;
    Ok(SessionHandle {
        id,
        dir: handle_dir,
        control: tx,
    })
}

/// Name the history and the recovered state record for a session: the model the
/// session asked for, or the one selected in Settings — the same rule the
/// worker applies to an empty id. A local dictation asks for nothing, so
/// without this fallback its entries said "unknown model".
pub fn recorded_model(model: Option<&ModelChoice>, cfg: &crate::config::Config) -> String {
    match model {
        Some(model) => model.id.clone(),
        None => transcribe::selected_id(cfg),
    }
}

fn run(
    shared: Arc<Shared>,
    dirs: AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<dyn Recognizer>,
    start: SessionStart,
    dir: PathBuf,
    control: mpsc::Receiver<Control>,
) {
    let model_id = start
        .model
        .as_ref()
        .map(|model| model.api_id())
        .unwrap_or_default();
    // The model that really decodes this recording, so the history and the
    // state left for the recovery both name it.
    let model_label = recorded_model(start.model.as_ref(), &shared.lock().config);
    let mut persisted = PersistedState {
        id: start.id,
        started: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        interactor: start.interactor.kind,
        model: model_label.clone(),
        seconds: 0.0,
        done: false,
        failed_segments: 0,
        failed: false,
        kept: false,
    };
    write_state(&dir, &persisted);

    let mut wav = match WavWriter::create(&audio_path(&dir), SAMPLE_RATE) {
        Ok(wav) => wav,
        Err(e) => {
            log::log(&format!("session {}: cannot write audio: {e:#}", start.id));
            let _ = events.send(AppEvent::Failed {
                message: crate::i18n::trf(
                    "Could not write the recording: {error}",
                    &[("error", &format!("{e:#}"))],
                ),
                lazy: start.mode.is_lazy(),
            });
            return;
        }
    };
    let mut resampler = (start.rate != SAMPLE_RATE).then(|| Resampler::new(start.rate, SAMPLE_RATE));
    // The model may accept less than the default 25 s; the sensitivity comes
    // from the user's settings.
    let (limit, sensitivity) = {
        let data = shared.lock();
        let limit = start
            .model
            .as_ref()
            .map(transcribe::segment_limit)
            .unwrap_or_else(|| transcribe::limit_for_config(&data.config));
        (limit, data.config.vad_sensitivity)
    };
    log::log(&format!(
        "session {}: segments up to {:.0} s{}",
        start.id,
        limit,
        if crate::model_manager::find_model(&model_label).is_none() && !model_label.is_empty() {
            " (the model is not in the catalogue, the default applies)"
        } else {
            ""
        }
    ));
    let mut segmenter =
        Segmenter::configured(SAMPLE_RATE, limit, (limit * 1.2).min(crate::segmenter::MAX_SEGMENT), sensitivity);
    let mut stitcher = Stitcher::new();
    let started_ms = chrono::Local::now().timestamp_millis();
    ipc::write_live(
        &dirs.root,
        &ipc::LiveView {
            text: String::new(),
            started: started_ms,
        },
    );

    let mut resampled = Vec::new();
    let mut pending: VecDeque<Segment> = VecDeque::new();
    let mut last_state = std::time::Instant::now();
    // If the engine dies on every segment, stop hammering it: the audio is
    // still written to disk and the recovery at the next start can retry.
    let mut consecutive_failures = 0usize;
    let mut degraded = false;

    // Audio always reaches the disk and the segmenter, whatever the engine is
    // doing: a lazy take must not lose sound while it waits, and a live
    // recording must not stall behind the cut.
    #[allow(clippy::too_many_arguments)]
    fn accept_audio(
        samples: &[f32],
        wav: &mut WavWriter,
        segmenter: &mut Segmenter,
        resampler: &mut Option<Resampler>,
        resampled: &mut Vec<f32>,
        pending: &mut VecDeque<Segment>,
        id: u64,
    ) {
        resampled.clear();
        match resampler.as_mut() {
            Some(resampler) => resampler.process(samples, resampled),
            None => resampled.extend_from_slice(samples),
        }
        if resampled.is_empty() {
            return;
        }
        if let Err(e) = wav.write(resampled) {
            log::log(&format!("session {id}: audio write failed: {e:#}"));
        }
        pending.extend(segmenter.push(resampled));
    }

    let mut closed = false;
    let mut flushed = false;
    // The tail budget starts when the caller stops: from that moment the
    // microphone is held by the app, so the person gets it back after at most
    // this much real time — the segment already in the engine is finished.
    let mut tail_deadline: Option<std::time::Instant> = None;
    let mut tail_expired = false;
    loop {
        if !closed {
            let drained = {
                let mut buffer = start.buffer.lock().unwrap();
                std::mem::take(&mut *buffer)
            };
            if !drained.is_empty() {
                accept_audio(
                    &drained,
                    &mut wav,
                    &mut segmenter,
                    &mut resampler,
                    &mut resampled,
                    &mut pending,
                    start.id,
                );
            }
            match control.try_recv() {
                Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => closed = true,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if closed && !flushed {
            // Everything still buffered belongs to the last moments of the
            // recording; the tail joins the same queue instead of being decoded
            // out of order.
            let drained = {
                let mut buffer = start.buffer.lock().unwrap();
                std::mem::take(&mut *buffer)
            };
            if !drained.is_empty() {
                accept_audio(
                    &drained,
                    &mut wav,
                    &mut segmenter,
                    &mut resampler,
                    &mut resampled,
                    &mut pending,
                    start.id,
                );
            }
            if let Some(tail) = segmenter.flush() {
                pending.push_back(tail);
            }
            flushed = true;
            tail_deadline = start
                .tail_budget
                .map(|budget| std::time::Instant::now() + budget);
        }

        // The engine's owner is read once per step: a segment already being
        // decoded is not interrupted, so the next one re-checks.
        let engine_busy = shared.lock().engine_busy();
        if start.mode.is_lazy() {
            publish_lazy(&shared, pending.len(), engine_busy);
            // The same violet the phone and Telegram paint: background audio is
            // in the app's hands — arriving, waiting for the engine, or being
            // decoded. A take that has received nothing stays quiet, so an
            // abandoned empty session cannot tint the tray for ever.
            set_lazy_background(&shared, lazy_holds_audio(&shared, &pending, degraded));
        }
        // The budget is checked between segments, so the one already in the
        // engine is finished rather than thrown away.
        while !degraded && !pending.is_empty() && may_decode(start.mode, engine_busy) {
            if let Some(deadline) = tail_deadline {
                if std::time::Instant::now() >= deadline {
                    tail_expired = true;
                    break;
                }
            }
            let segment = pending.pop_front().expect("checked above");
            let failures_before = persisted.failed_segments;
            decode_segment(
                &asr,
                &model_id,
                &start,
                &shared,
                &dirs,
                &segment,
                &mut stitcher,
                &mut persisted,
                started_ms,
            );
            if persisted.failed_segments > failures_before {
                consecutive_failures += 1;
                if consecutive_failures >= 3 {
                    degraded = true;
                    log::log(&format!(
                        "session {}: the engine keeps failing; the rest of the recording goes to disk only",
                        start.id
                    ));
                }
            } else {
                consecutive_failures = 0;
            }
        }
        persisted.seconds = wav.seconds();
        if last_state.elapsed() > Duration::from_millis(900) {
            last_state = std::time::Instant::now();
            write_state(&dir, &persisted);
        }
        if closed && flushed && (tail_expired || pending.is_empty() || degraded) {
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    if start.mode.is_lazy() {
        // The take is over: the violet icon goes out with it.
        set_lazy_background(&shared, false);
    }
    if tail_expired && !pending.is_empty() {
        // The budget ran out: what is left stays on disk, and the entry is
        // marked partial so the audio can be found and retried later.
        let left = pending.len();
        persisted.failed_segments += left;
        pending.clear();
        log::log(&format!(
            "session {}: tail budget spent, {left} segment(s) left untranscribed",
            start.id
        ));
    }
    if degraded && !pending.is_empty() {
        // The audio is on disk; the recovery can read it later.
        persisted.failed_segments += pending.len();
        pending.clear();
    }

    let seconds = wav.seconds();
    if let Err(e) = wav.finish() {
        log::log(&format!("session {}: cannot close the audio file: {e:#}", start.id));
    }
    persisted.seconds = seconds;
    persisted.done = true;
    write_state(&dir, &persisted);
    // A copy for the user, if they asked for one. The recording directory is
    // the app's own working copy and is cleaned up below; the saved file is
    // what stays.
    let recording = crate::audio_save::Recording {
        origin: match start.interactor.kind {
            InteractorKind::Server => crate::audio_save::Origin::Api,
            _ => crate::audio_save::Origin::User,
        },
        account: "",
        label: "",
        at: &persisted.started,
    };
    if let Err(e) =
        crate::audio_save::save_wav(&shared.lock().config, &recording, &audio_path(&dir))
    {
        log::log(&format!(
            "session {}: cannot save a copy of the recording: {e:#}",
            start.id
        ));
    }
    let text = stitcher.text();
    log::log(&format!(
        "session {} finished: {:.2}s audio, {} chars, {} failed segments",
        start.id,
        seconds,
        text.chars().count(),
        persisted.failed_segments
    ));
    if !start.mode.is_lazy() {
        ipc::write_live(
            &dirs.root,
            &ipc::LiveView {
                text: text.clone(),
                started: started_ms,
            },
        );
    }
    let state = if persisted.failed_segments > 0 {
        EntryState::Partial
    } else {
        EntryState::Complete
    };
    let _ = events.send(AppEvent::Transcribed {
        text,
        model: model_label,
        append: start.append,
        paste_target: start.paste_target,
        clipboard: start.clipboard,
        state,
        dir: Some(dir),
        source: match start.interactor.kind {
            InteractorKind::Server => crate::history::Source::Server,
            _ => crate::history::Source::User,
        },
        audio: None,
        at: None,
        session: start.id,
        // Lazy work has no cue: nobody at the computer is waiting for it.
        lazy: start.mode.is_lazy(),
    });
}

/// A lazy take gives the engine up the moment the person needs it; a live one
/// never waits.
pub fn may_decode(mode: SessionMode, engine_busy: bool) -> bool {
    !mode.is_lazy() || !engine_busy
}

/// Keep the API's view of a lazy take current: what the caller can see while it
/// waits is exactly this.
fn publish_lazy(shared: &Shared, queued: usize, engine_busy: bool) {
    if let Some(take) = shared.lock().lazy.as_mut() {
        take.queued = queued;
        take.waiting = engine_busy && queued > 0;
    }
}

/// Does the lazy take hold audio the app is responsible for? True from the first
/// chunk until the text is out, so the tray shows the take the whole time it is
/// the app's work — the same "background" the phone and Telegram paint.
fn lazy_holds_audio(shared: &Shared, pending: &VecDeque<Segment>, degraded: bool) -> bool {
    if degraded {
        return false;
    }
    if !pending.is_empty() {
        return true;
    }
    shared
        .lock()
        .lazy
        .as_ref()
        .map(|take| take.received_samples > 0)
        .unwrap_or(false)
}

/// The tray is violet while the engine is decoding a lazy take — exactly the
/// rule the phone's queue follows, so the two background jobs look the same.
fn set_lazy_background(shared: &Shared, active: bool) {
    let mut data = shared.lock();
    if active {
        data.background.insert(BackgroundJob::Api);
    } else {
        data.background.remove(&BackgroundJob::Api);
    }
}

#[allow(clippy::too_many_arguments)]
fn priority(kind: InteractorKind) -> Priority {
    match kind {
        InteractorKind::User => Priority::User,
        InteractorKind::Server => Priority::Api,
    }
}

fn decode_segment(
    asr: &Arc<dyn Recognizer>,
    model_id: &str,
    start: &SessionStart,
    shared: &Arc<Shared>,
    dirs: &AppDirs,
    segment: &Segment,
    stitcher: &mut Stitcher,
    persisted: &mut PersistedState,
    started: i64,
) {
    if segment.speech_seconds < MIN_SPEECH {
        return;
    }
    let began = std::time::Instant::now();
    let mut result = asr.decode(
        priority(start.interactor.kind),
        model_id,
        &segment.samples,
        SAMPLE_RATE,
    );
    if result.is_err() {
        // The worker is restarted by the client; one retry covers a crash.
        log::log(&format!(
            "session {}: segment {} failed ({}), retrying once",
            start.id,
            segment.index,
            result.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default()
        ));
        result = asr.decode(
            priority(start.interactor.kind),
            model_id,
            &segment.samples,
            SAMPLE_RATE,
        );
    }
    match result {
        Ok(reply) => {
            let added = stitcher.commit(
                &reply.tokens,
                &reply.times,
                segment.audio_start,
                segment.commit_start,
                segment.commit_end,
            );
            log::log(&format!(
                "session {}: segment {} [{:.1}..{:.1}] {:.2}s audio, {} words, {} ms (engine {} ms)",
                start.id,
                segment.index,
                segment.audio_start,
                segment.audio_end,
                segment.speech_seconds,
                added,
                began.elapsed().as_millis(),
                reply.ms
            ));
            let text = stitcher.text();
            // A lazy take is invisible to the window: its preview belongs to
            // the caller that sent the audio, not to the person's live box.
            if !start.mode.is_lazy() {
                ipc::write_live(
                    &dirs.root,
                    &ipc::LiveView {
                        text: text.clone(),
                        started,
                    },
                );
            }
            if start.interactor.kind == InteractorKind::Server {
                let mut data = shared.lock();
                if start.mode.is_lazy() {
                    if let Some(take) = data.lazy.as_mut() {
                        take.live_text = text;
                    }
                } else if let Some(stream) = data.stream.as_mut() {
                    stream.live_text = text;
                }
            }
        }
        Err(e) => {
            persisted.failed_segments += 1;
            log::log(&format!(
                "session {}: segment {} could not be transcribed: {e:#}",
                start.id, segment.index
            ));
        }
    }
}

/// Control over an imported file transcription.
pub struct FileHandle {
    cancelled: Arc<AtomicBool>,
}

impl FileHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Transcribe a file the user picked. The file is read where it is: nothing is
/// copied into the application directory, and the audio stays the user's.
pub fn start_file(
    shared: Arc<Shared>,
    dirs: AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<Engine>,
    path: PathBuf,
) -> Result<FileHandle> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    std::thread::Builder::new()
        .name("file-import".into())
        .spawn(move || run_file(shared, dirs, events, asr, path, flag))
        .context("spawn the file transcription")?;
    Ok(FileHandle { cancelled })
}

fn run_file(
    shared: Arc<Shared>,
    dirs: AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<Engine>,
    path: PathBuf,
    cancelled: Arc<AtomicBool>,
) {
    use crate::ipc::{self, FileSnapshot};
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio")
        .to_string();
    // The copy is named after the moment the import started, like a dictation.
    let started_at = chrono::Local::now()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let (limit, sensitivity, model_label, save_config) = {
        let data = shared.lock();
        (
            transcribe::limit_for_config(&data.config),
            data.config.vad_sensitivity,
            transcribe::selected_id(&data.config),
            data.config.clone(),
        )
    };
    // The imported file stays where it is. When the user asks for saved copies,
    // one is encoded while the file is decoded: a long recording never has to
    // fit in memory twice.
    let export_path = recordings_dir(&dirs.root).join(format!(
        "export-{}-{}.wav",
        std::process::id(),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    let mut export = if save_config.save_recordings {
        match WavWriter::create(&export_path, SAMPLE_RATE) {
            Ok(writer) => Some(writer),
            Err(e) => {
                log::log(&format!(
                    "file {}: cannot prepare the saved copy: {e:#}",
                    path.display()
                ));
                None
            }
        }
    } else {
        None
    };
    let total = crate::decode::probe(&path)
        .ok()
        .and_then(|info| info.seconds)
        .unwrap_or(0.0);
    let mut snapshot = FileSnapshot {
        path: path.display().to_string(),
        name,
        done_seconds: 0.0,
        total_seconds: total,
        running: true,
        ..Default::default()
    };
    ipc::write_file_state(&dirs.root, &snapshot);
    log::log(&format!(
        "file {}: transcribing, segments up to {:.0} s",
        path.display(),
        limit
    ));

    let mut segmenter = Segmenter::configured(
        SAMPLE_RATE,
        limit,
        (limit * 1.2).min(crate::segmenter::MAX_SEGMENT),
        sensitivity,
    );
    let mut stitcher = Stitcher::new();
    let mut seconds = 0.0f64;
    let mut failed = 0usize;
    let mut consecutive = 0usize;
    let mut degraded = false;
    let mut last = std::time::Instant::now();

    let result = crate::decode::decode_file(&path, |samples| {
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        seconds += samples.len() as f64 / SAMPLE_RATE as f64;
        if let Some(wav) = export.as_mut() {
            if let Err(e) = wav.write(samples) {
                log::log(&format!(
                    "file {}: cannot write the saved copy: {e:#}",
                    path.display()
                ));
            }
        }
        for segment in segmenter.push(samples) {
            if degraded {
                continue;
            }
            if decode_file_segment(&asr, &segment, &mut stitcher) {
                consecutive = 0;
            } else {
                failed += 1;
                consecutive += 1;
                if consecutive >= 3 {
                    degraded = true;
                    log::log(&format!(
                        "file {}: the engine keeps failing, the rest is skipped",
                        path.display()
                    ));
                }
            }
        }
        if last.elapsed() > Duration::from_millis(250) {
            last = std::time::Instant::now();
            snapshot.done_seconds = seconds;
            snapshot.text = stitcher.text();
            ipc::write_file_state(&dirs.root, &snapshot);
        }
    });

    if cancelled.load(Ordering::SeqCst) {
        if let Some(wav) = export.take() {
            drop(wav);
            let _ = std::fs::remove_file(&export_path);
        }
        snapshot.running = false;
        snapshot.cancelled = true;
        snapshot.text = stitcher.text();
        ipc::write_file_state(&dirs.root, &snapshot);
        log::log(&format!("file {}: cancelled", path.display()));
        let _ = events.send(AppEvent::Cancelled);
        return;
    }
    if let Err(e) = result {
        if let Some(wav) = export.take() {
            drop(wav);
            let _ = std::fs::remove_file(&export_path);
        }
        let message = crate::i18n::trf(
            "Could not read the file: {error}",
            &[("error", &format!("{e:#}"))],
        );
        snapshot.running = false;
        snapshot.error = Some(message.clone());
        ipc::write_file_state(&dirs.root, &snapshot);
        let _ = events.send(AppEvent::Failed {
            message,
            lazy: false,
        });
        return;
    }
    if let Some(tail) = segmenter.flush() {
        if !degraded && !decode_file_segment(&asr, &tail, &mut stitcher) {
            failed += 1;
        }
    }
    // The file was fully read: the saved copy can be finished now. Its name
    // keeps the original file's name, so the folder reads like the user's own.
    if let Some(wav) = export.take() {
        if let Err(e) = wav.finish() {
            log::log(&format!(
                "file {}: cannot close the saved copy: {e:#}",
                path.display()
            ));
        }
        let label = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("audio")
            .to_string();
        let recording = crate::audio_save::Recording {
            origin: crate::audio_save::Origin::File,
            account: "",
            label: &label,
            at: &started_at,
        };
        if let Err(e) = crate::audio_save::save_wav(&save_config, &recording, &export_path) {
            log::log(&format!(
                "file {}: cannot save a copy: {e:#}",
                path.display()
            ));
        }
        let _ = std::fs::remove_file(&export_path);
    }
    let text = stitcher.text();
    snapshot.running = false;
    snapshot.done_seconds = seconds;
    snapshot.text = text.clone();
    ipc::write_file_state(&dirs.root, &snapshot);
    let state = if failed > 0 {
        EntryState::Partial
    } else {
        EntryState::Complete
    };
    log::log(&format!(
        "file {}: done, {} chars, {} failed segments",
        path.display(),
        text.chars().count(),
        failed
    ));
    let _ = events.send(AppEvent::Transcribed {
        text,
        model: model_label,
        append: false,
        paste_target: None,
        clipboard: false,
        state,
        dir: None,
        source: crate::history::Source::File,
        audio: Some(path.display().to_string()),
        at: None,
        session: 0,
        lazy: false,
    });
}

/// Decode one segment of an imported file (two attempts, like a recording).
fn decode_file_segment(
    asr: &Arc<Engine>,
    segment: &Segment,
    stitcher: &mut Stitcher,
) -> bool {
    if segment.speech_seconds < MIN_SPEECH {
        return true;
    }
    // An empty model id means "the model selected in Settings", which also
    // covers custom model folders.
    let mut result = asr.decode(Priority::File, "", &segment.samples, SAMPLE_RATE);
    if result.is_err() {
        result = asr.decode(Priority::File, "", &segment.samples, SAMPLE_RATE);
    }
    match result {
        Ok(reply) => {
            stitcher.commit(
                &reply.tokens,
                &reply.times,
                segment.audio_start,
                segment.commit_start,
                segment.commit_end,
            );
            true
        }
        Err(e) => {
            log::log(&format!(
                "file segment {} could not be transcribed: {e:#}",
                segment.index
            ));
            false
        }
    }
}

/// Transcribe one recording directory end to end. The audio is re-segmented from
/// the start: the file is the source of truth, so recovery never depends on how
/// far the interrupted run got.
pub fn recover(
    asr: &Arc<Engine>,
    dir: &Path,
    model: &str,
    limit: f32,
    sensitivity: f32,
) -> Result<(String, f64, usize)> {
    let wav = read_wav(&audio_path(dir)).context("read the recorded audio")?;
    if wav.samples.is_empty() {
        anyhow::bail!("the recording is empty");
    }
    log::log(&format!(
        "recovery: {} segments up to {:.0} s",
        dir.display(),
        limit
    ));
    let mut segmenter =
        Segmenter::configured(SAMPLE_RATE, limit, (limit * 1.2).min(crate::segmenter::MAX_SEGMENT), sensitivity);
    let mut stitcher = Stitcher::new();
    let mut failed = 0usize;
    let samples = if wav.rate == SAMPLE_RATE {
        wav.samples.clone()
    } else {
        let mut out = Vec::new();
        Resampler::new(wav.rate, SAMPLE_RATE).process(&wav.samples, &mut out);
        out
    };
    let decode = |segment: &Segment, stitcher: &mut Stitcher, failed: &mut usize| {
        let mut result = asr.decode(Priority::User, model, &segment.samples, SAMPLE_RATE);
        if result.is_err() {
            result = asr.decode(Priority::User, model, &segment.samples, SAMPLE_RATE);
        }
        match result {
            Ok(reply) => {
                stitcher.commit(
                    &reply.tokens,
                    &reply.times,
                    segment.audio_start,
                    segment.commit_start,
                    segment.commit_end,
                );
            }
            Err(e) => {
                *failed += 1;
                log::log(&format!(
                    "recovery: segment {} failed: {e:#}",
                    segment.index
                ));
            }
        }
    };
    // Feed the segmenter in one-second steps to keep peak memory flat.
    let step = SAMPLE_RATE as usize;
    let mut at = 0usize;
    while at < samples.len() {
        let end = (at + step).min(samples.len());
        for segment in segmenter.push(&samples[at..end]) {
            decode(&segment, &mut stitcher, &mut failed);
            // Five dead segments in a row with nothing to show for them: the
            // engine is not coming back, so stop wasting the user's start-up.
            if failed >= 5 && stitcher.is_empty() {
                anyhow::bail!(
                    "the transcription engine failed on the first {failed} segments"
                );
            }
        }
        at = end;
    }
    if let Some(tail) = segmenter.flush() {
        decode(&tail, &mut stitcher, &mut failed);
    }
    Ok((stitcher.text(), wav.samples.len() as f64 / wav.rate as f64, failed))
}

/// History state of a recovered recording: its text went to the history only,
/// and it may be missing a segment.
pub fn recovered_state(failed: usize) -> EntryState {
    if failed > 0 {
        EntryState::Partial
    } else {
        EntryState::Complete
    }
}

/// Delete the audio of a finished session (the text is already in the history).
pub fn discard(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::log(&format!("cannot remove {}: {e}", dir.display()));
        }
    }
}

/// Mark a recording as failed: the audio stays, but recovery will not loop on it.
pub fn mark_failed(dir: &Path) {
    if let Some(mut state) = read_state(dir) {
        state.failed = true;
        state.done = false;
        write_state(dir, &state);
    }
    log::log(&format!(
        "{} is kept but will not be retried: the engine failed on it",
        dir.display()
    ));
}

/// Keep the audio because the history links to it: the text is partial, so it
/// is not deleted and the recovery does not retry it (the text is already there).
pub fn keep_for_user(dir: &Path) {
    if let Some(mut state) = read_state(dir) {
        state.kept = true;
        state.done = true;
        write_state(dir, &state);
    }
    log::log(&format!(
        "keeping {}: the transcription is partial",
        dir.display()
    ));
}

/// Delete kept recordings older than days so the data directory stays bounded.
pub fn cleanup_kept(root: &Path, days: u64) {
    let Ok(entries) = std::fs::read_dir(recordings_dir(root)) else {
        return;
    };
    let limit = std::time::Duration::from_secs(days * 24 * 3600);
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let age = std::fs::metadata(&dir)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok());
        if let Some(age) = age {
            if age > limit {
                log::log(&format!(
                    "removing kept audio older than {days} days: {}",
                    dir.display()
                ));
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

/// Keep the audio for the next start when the text could not be saved: marking
/// the session unfinished makes the recovery thread transcribe it again.
pub fn keep_for_recovery(dir: &Path) {
    if let Some(mut state) = read_state(dir) {
        state.done = false;
        write_state(dir, &state);
    }
    log::log(&format!(
        "keeping {} for recovery: the text was not saved",
        dir.display()
    ));
}

#[cfg(test)]
mod tests {
    use super::{mark_failed, pending, recorded_model, recordings_dir, write_state, PersistedState};

    #[test]
    fn a_failed_recording_is_not_retried_at_the_next_start() {
        let root = std::env::temp_dir().join(format!(
            "voice-button-session-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let dir = recordings_dir(&root).join("20260101-000000-1");
        std::fs::create_dir_all(&dir).expect("recording dir");
        write_state(
            &dir,
            &PersistedState {
                id: 1,
                done: false,
                ..Default::default()
            },
        );
        assert_eq!(pending(&root).len(), 1);
        mark_failed(&dir);
        assert!(
            pending(&root).is_empty(),
            "a failed recording must not be picked up again"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_local_dictation_records_the_model_selected_in_settings() {
        use crate::config::{Backend, Config};
        use crate::state::ModelChoice;

        let mut cfg = Config::default();
        cfg.backend = Backend::Onnx;
        cfg.onnx_model = "gigaam-v3-rnnt-punct".to_string();
        // The window, the hotkey and the tray start a recording without a model
        // of their own: the history must still name the one that runs.
        assert_eq!(recorded_model(None, &cfg), "gigaam-v3-rnnt-punct");

        let asked = ModelChoice {
            backend: Backend::Whisper,
            id: "large-v3".to_string(),
        };
        assert_eq!(
            recorded_model(Some(&asked), &cfg),
            "large-v3",
            "a session that names its model keeps it"
        );
    }

    /// The two ways a session takes the engine. These tests drive the real
    /// session thread and the real segmenter; only the recognizer is a fake, so
    /// "waits" and "continues" are observed rather than assumed.
    mod modes {
        use super::super::*;
        use crate::config::Config;
        use crate::history::History;
        use crate::state::{Interactor, LazyTake, Status};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};

        /// A recognizer that only counts: no model, no worker, no audio.
        struct Counting {
            calls: Arc<AtomicUsize>,
        }

        impl Recognizer for Counting {
            fn decode(
                &self,
                _priority: Priority,
                _model: &str,
                _samples: &[f32],
                _rate: u32,
            ) -> Result<Reply> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(Reply {
                    ok: true,
                    ms: 1,
                    ..Default::default()
                })
            }
        }

        fn temp_dirs(name: &str) -> AppDirs {
            let root = std::env::temp_dir().join(format!(
                "voice-button-session-{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("models")).expect("temp models");
            AppDirs {
                root: root.clone(),
                models: root.join("models"),
            }
        }

        /// Enough loud audio for the segmenter to cut a segment: speech is
        /// measured by energy, and a tone is above the floor.
        fn loud(seconds: f32) -> Vec<f32> {
            let rate = SAMPLE_RATE as f32;
            let count = (rate * seconds) as usize;
            (0..count)
                .map(|i| {
                    let t = i as f32 / rate;
                    0.35 * (2.0 * std::f32::consts::PI * 220.0 * t).sin()
                })
                .collect()
        }

        fn wait_for(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
            let started = Instant::now();
            loop {
                if done() {
                    return true;
                }
                if started.elapsed() >= limit {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn session_start(
            id: u64,
            buffer: Arc<Mutex<Vec<f32>>>,
            mode: SessionMode,
        ) -> SessionStart {
            SessionStart {
                id,
                interactor: Interactor::server(),
                model: None,
                rate: SAMPLE_RATE,
                append: false,
                paste_target: None,
                clipboard: false,
                buffer,
                mode,
                tail_budget: None,
            }
        }

        #[test]
        fn only_a_lazy_take_yields_the_engine() {
            assert!(may_decode(SessionMode::Live, true));
            assert!(may_decode(SessionMode::Live, false));
            assert!(!may_decode(SessionMode::Lazy, true));
            assert!(may_decode(SessionMode::Lazy, false));
        }

        #[test]
        fn a_lazy_take_waits_for_the_user_and_then_continues() {
            let shared = Arc::new(Shared::new(
                Config::default(),
                History::from_entries(Vec::new()),
            ));
            // The person is dictating: the engine is theirs.
            shared.lock().status = Status::Recording;
            let buffer = Arc::new(Mutex::new(loud(31.0)));
            let calls = Arc::new(AtomicUsize::new(0));
            shared.lock().lazy = Some(LazyTake {
                id: 7,
                model: None,
                sample_rate: SAMPLE_RATE,
                buffer: buffer.clone(),
                received_samples: 31 * SAMPLE_RATE as u64,
                live_text: String::new(),
                closed: false,
                queued: 0,
                waiting: false,
            });
            let dirs = temp_dirs("lazy-waits");
            let (events, _events_rx) = mpsc::channel();
            let handle = start(
                shared.clone(),
                dirs.clone(),
                events,
                Arc::new(Counting {
                    calls: calls.clone(),
                }),
                session_start(7, buffer, SessionMode::Lazy),
            )
            .expect("start the lazy take");

            assert!(
                wait_for(Duration::from_secs(5), || shared
                    .lock()
                    .lazy
                    .as_ref()
                    .map(|take| take.waiting)
                    .unwrap_or(false)),
                "the take must say that it is waiting for the engine"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "a lazy take must not decode while the person records"
            );
            assert!(
                shared.lock().background_has(BackgroundJob::Api),
                "a take that holds audio is background work: the tray is violet"
            );
            assert_eq!(
                shared.lock().status,
                Status::Recording,
                "a lazy take never changes the status: the mic is not its"
            );

            // The dictation is over: the segment that waited goes in.
            shared.lock().status = Status::Idle;
            assert!(
                wait_for(Duration::from_secs(5), || calls.load(Ordering::SeqCst) > 0),
                "the queued segment must run once the engine is free"
            );
            assert!(
                !shared
                    .lock()
                    .lazy
                    .as_ref()
                    .map(|take| take.waiting)
                    .unwrap_or(true),
                "the take is no longer waiting"
            );

            handle.stop();
            assert!(
                wait_for(Duration::from_secs(5), || !shared
                    .lock()
                    .background_has(BackgroundJob::Api)),
                "the violet icon must go out when nothing is left"
            );
            let _ = std::fs::remove_dir_all(&dirs.root);
        }

        /// A recognizer that stops inside its first `decode` until the test lets
        /// it go, so the state *while the engine works* can be observed.
        struct Gated {
            started: mpsc::Sender<()>,
            release: Mutex<Option<mpsc::Receiver<()>>>,
        }

        impl Recognizer for Gated {
            fn decode(
                &self,
                _priority: Priority,
                _model: &str,
                _samples: &[f32],
                _rate: u32,
            ) -> Result<Reply> {
                if let Some(release) = self.release.lock().unwrap().take() {
                    let _ = self.started.send(());
                    let _ = release.recv_timeout(Duration::from_secs(10));
                }
                Ok(Reply {
                    ok: true,
                    ms: 1,
                    ..Default::default()
                })
            }
        }

        /// The lazy take is shown like the phone's work: the tray is violet
        /// while the take holds audio, and goes out when it ends.
        #[test]
        fn a_lazy_take_is_violet_while_it_holds_audio_and_goes_out_with_it() {
            let shared = Arc::new(Shared::new(
                Config::default(),
                History::from_entries(Vec::new()),
            ));
            let buffer = Arc::new(Mutex::new(loud(31.0)));
            shared.lock().lazy = Some(LazyTake {
                id: 9,
                model: None,
                sample_rate: SAMPLE_RATE,
                buffer: buffer.clone(),
                received_samples: 31 * SAMPLE_RATE as u64,
                live_text: String::new(),
                closed: false,
                queued: 0,
                waiting: false,
            });
            let dirs = temp_dirs("lazy-violet");
            let (events, _events_rx) = mpsc::channel();
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let handle = start(
                shared.clone(),
                dirs.clone(),
                events,
                Arc::new(Gated {
                    started: started_tx,
                    release: Mutex::new(Some(release_rx)),
                }),
                session_start(9, buffer, SessionMode::Lazy),
            )
            .expect("start the lazy take");

            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the segment must reach the engine");
            assert!(
                shared.lock().background_has(BackgroundJob::Api),
                "the tray must be violet while the engine decodes lazy audio"
            );

            release_tx.send(()).expect("let the decode finish");
            handle.stop();
            assert!(
                wait_for(Duration::from_secs(5), || !shared
                    .lock()
                    .background_has(BackgroundJob::Api)),
                "the violet icon goes out when the take is over"
            );
            let _ = std::fs::remove_dir_all(&dirs.root);
        }

        /// A recognizer that takes real time, like a model on a busy machine.
        struct Slow {
            per_segment: Duration,
            calls: Arc<AtomicUsize>,
        }

        impl Recognizer for Slow {
            fn decode(
                &self,
                _priority: Priority,
                _model: &str,
                _samples: &[f32],
                _rate: u32,
            ) -> Result<Reply> {
                std::thread::sleep(self.per_segment);
                self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(Reply {
                    ok: true,
                    ms: 1,
                    ..Default::default()
                })
            }
        }

        fn api_session_start(
            id: u64,
            buffer: Arc<Mutex<Vec<f32>>>,
            budget: Option<Duration>,
        ) -> SessionStart {
            SessionStart {
                id,
                interactor: Interactor::server(),
                model: None,
                rate: SAMPLE_RATE,
                append: false,
                paste_target: None,
                clipboard: false,
                buffer,
                mode: SessionMode::Live,
                tail_budget: budget,
            }
        }

        /// The caller stops with a backlog: the app finishes at most the
        /// configured real time of it, keeps the rest on disk and hands the
        /// microphone back.
        #[test]
        fn an_exclusive_tail_stops_when_its_budget_is_spent() {
            let shared = Arc::new(Shared::new(
                Config::default(),
                History::from_entries(Vec::new()),
            ));
            let buffer = Arc::new(Mutex::new(loud(80.0)));
            let calls = Arc::new(AtomicUsize::new(0));
            let dirs = temp_dirs("api-tail-budget");
            let (events, events_rx) = mpsc::channel();
            let handle = start(
                shared.clone(),
                dirs.clone(),
                events,
                Arc::new(Slow {
                    per_segment: Duration::from_millis(400),
                    calls: calls.clone(),
                }),
                api_session_start(11, buffer, Some(Duration::from_millis(250))),
            )
            .expect("start the API session");
            // The stream ends at once, with several segments still queued.
            handle.stop();

            let event = events_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the session must finish");
            match event {
                AppEvent::Transcribed { state, .. } => assert!(
                    state.is_partial(),
                    "the budget left part of the tail untranscribed"
                ),
                other => panic!("unexpected event: {other:?}"),
            }
            assert!(
                calls.load(Ordering::SeqCst) < 4,
                "the budget must stop the tail before the whole backlog is read"
            );
            let _ = std::fs::remove_dir_all(&dirs.root);
        }

        /// Zero in the settings means "no limit": the old behaviour, the whole
        /// tail is transcribed.
        #[test]
        fn a_tail_without_a_budget_is_transcribed_to_the_end() {
            let shared = Arc::new(Shared::new(
                Config::default(),
                History::from_entries(Vec::new()),
            ));
            let buffer = Arc::new(Mutex::new(loud(80.0)));
            let calls = Arc::new(AtomicUsize::new(0));
            let dirs = temp_dirs("api-tail-full");
            let (events, events_rx) = mpsc::channel();
            let handle = start(
                shared.clone(),
                dirs.clone(),
                events,
                Arc::new(Slow {
                    per_segment: Duration::from_millis(20),
                    calls: calls.clone(),
                }),
                api_session_start(12, buffer, None),
            )
            .expect("start the API session");
            handle.stop();

            let event = events_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the session must finish");
            match event {
                AppEvent::Transcribed { state, .. } => assert!(
                    !state.is_partial(),
                    "without a budget nothing is dropped"
                ),
                other => panic!("unexpected event: {other:?}"),
            }
            assert!(
                calls.load(Ordering::SeqCst) >= 4,
                "every segment, including the flushed tail, reaches the engine"
            );
            let _ = std::fs::remove_dir_all(&dirs.root);
        }

        /// The take is over: the user asked for a copy, so one appears in the
        /// folder that belongs to where the take came from.
        #[test]
        fn a_finished_take_leaves_a_saved_copy() {
            let saved = std::env::temp_dir().join(format!(
                "voice-button-saved-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&saved);
            let shared = Arc::new(Shared::new(
                Config::default(),
                History::from_entries(Vec::new()),
            ));
            {
                let mut data = shared.lock();
                data.status = Status::Idle;
                data.config.save_recordings = true;
                data.config.save_recordings_dir = saved.display().to_string();
            }
            let dirs = temp_dirs("saved-copy");
            let (events, events_rx) = mpsc::channel();
            let handle = start(
                shared.clone(),
                dirs.clone(),
                events,
                Arc::new(Counting {
                    calls: Arc::new(AtomicUsize::new(0)),
                }),
                session_start(21, Arc::new(Mutex::new(loud(1.0))), SessionMode::Live),
            )
            .expect("start the live session");
            handle.stop();
            let event = events_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("the session must finish");
            assert!(matches!(event, AppEvent::Transcribed { .. }));
            let copies: Vec<_> = std::fs::read_dir(saved.join("api"))
                .expect("the api folder")
                .flatten()
                .collect();
            assert_eq!(copies.len(), 1, "one take, one saved copy");
            assert!(copies[0].file_name().to_string_lossy().ends_with(".ogg"));
            let _ = std::fs::remove_dir_all(&dirs.root);
            let _ = std::fs::remove_dir_all(&saved);
        }

        #[test]
        fn a_live_session_does_not_wait_for_the_user() {
            let shared = Arc::new(Shared::new(
                Config::default(),
                History::from_entries(Vec::new()),
            ));
            shared.lock().status = Status::Recording;
            let buffer = Arc::new(Mutex::new(loud(31.0)));
            let calls = Arc::new(AtomicUsize::new(0));
            let dirs = temp_dirs("live-busy");
            let (events, _events_rx) = mpsc::channel();
            let handle = start(
                shared.clone(),
                dirs.clone(),
                events,
                Arc::new(Counting {
                    calls: calls.clone(),
                }),
                session_start(8, buffer, SessionMode::Live),
            )
            .expect("start the live session");

            assert!(
                wait_for(Duration::from_secs(5), || calls.load(Ordering::SeqCst) > 0),
                "a live session asks for the engine at once, even mid-dictation"
            );
            assert!(
                !shared.lock().background_has(BackgroundJob::Api),
                "a live session is not background work"
            );
            handle.stop();
            let _ = std::fs::remove_dir_all(&dirs.root);
        }
    }
}
