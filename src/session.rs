//! One dictation session: drains captured audio, writes it to disk, cuts it
//! into segments, asks the ASR worker for text and publishes the growing
//! transcript.
//!
//! The session is the only place that touches the recognizer, so the rules that
//! keep the app alive live here: audio is on disk before it is decoded, a
//! segment is never longer than `MAX_SEGMENT`, and a failing engine costs the
//! session nothing but that segment.

use crate::asr_engine::{Engine, Priority};
use crate::audio::{read_wav, Resampler, WavWriter};
use crate::config::AppDirs;
use crate::history::EntryState;
use crate::ipc;
use crate::log;
use crate::segmenter::{Segment, Segmenter, MIN_SPEECH};
use crate::state::{AppEvent, Interactor, InteractorKind, ModelChoice, Shared};
use crate::stitch::Stitcher;
use crate::transcribe;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Audio handed to the recognizer is 16 kHz mono, like every model expects.
pub const SAMPLE_RATE: u32 = 16_000;

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

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

pub fn audio_path(dir: &Path) -> PathBuf {
    dir.join("audio.wav")
}

fn write_state(dir: &Path, state: &PersistedState) {
    if let Ok(text) = serde_json::to_string(state) {
        ipc::atomic_write(state_path(dir), text.as_bytes());
    }
}

pub fn read_state(dir: &Path) -> Option<PersistedState> {
    let text = std::fs::read_to_string(state_path(dir)).ok()?;
    serde_json::from_str(&text).ok()
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
        }
    }
}

pub fn start(
    shared: Arc<Shared>,
    dirs: AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<Engine>,
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

fn run(
    shared: Arc<Shared>,
    dirs: AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<Engine>,
    start: SessionStart,
    dir: PathBuf,
    control: mpsc::Receiver<Control>,
) {
    let model_id = start
        .model
        .as_ref()
        .map(|model| model.api_id())
        .unwrap_or_default();
    let model_label = start
        .model
        .as_ref()
        .map(|model| model.id.clone())
        .unwrap_or_default();
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
            let _ = events.send(AppEvent::Failed(format!("cannot write the recording: {e:#}")));
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
    let mut last_state = std::time::Instant::now();
    // If the engine dies on every segment, stop hammering it: the audio is
    // still written to disk and the recovery at the next start can retry.
    let mut consecutive_failures = 0usize;
    let mut degraded = false;

    let mut feed = |samples: &[f32],
                    wav: &mut WavWriter,
                    segmenter: &mut Segmenter,
                    stitcher: &mut Stitcher,
                    persisted: &mut PersistedState| {
        resampled.clear();
        match resampler.as_mut() {
            Some(resampler) => resampler.process(samples, &mut resampled),
            None => resampled.extend_from_slice(samples),
        }
        if resampled.is_empty() {
            return;
        }
        if let Err(e) = wav.write(&resampled) {
            log::log(&format!("session {}: audio write failed: {e:#}", start.id));
        }
        let ready = segmenter.push(&resampled);
        for segment in ready {
            if degraded {
                break;
            }
            let failures_before = persisted.failed_segments;
            decode_segment(&asr, &model_id, &start, &shared, &dirs, &segment, stitcher, persisted, started_ms);
            if persisted.failed_segments > failures_before {
                consecutive_failures += 1;
                if consecutive_failures >= 3 && !degraded {
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
            write_state(&dir, persisted);
        }
    };

    let mut stopped = false;
    while !stopped {
        let drained = {
            let mut buffer = start.buffer.lock().unwrap();
            std::mem::take(&mut *buffer)
        };
        if !drained.is_empty() {
            feed(
                &drained,
                &mut wav,
                &mut segmenter,
                &mut stitcher,
                &mut persisted,
            );
        }
        match control.try_recv() {
            Ok(Control::Stop) | Err(mpsc::TryRecvError::Disconnected) => stopped = true,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        if !stopped {
            std::thread::sleep(Duration::from_millis(60));
        }
    }

    // Everything still buffered belongs to the last moments of the recording.
    let drained = {
        let mut buffer = start.buffer.lock().unwrap();
        std::mem::take(&mut *buffer)
    };
    if !drained.is_empty() {
        feed(
            &drained,
            &mut wav,
            &mut segmenter,
            &mut stitcher,
            &mut persisted,
        );
    }
    if let Some(tail) = segmenter.flush() {
        if !degraded {
            decode_segment(
                &asr,
                &model_id,
                &start,
                &shared,
                &dirs,
                &tail,
                &mut stitcher,
                &mut persisted,
                started_ms,
            );
        } else {
            persisted.failed_segments += 1;
        }
    }

    let seconds = wav.seconds();
    if let Err(e) = wav.finish() {
        log::log(&format!("session {}: cannot close the audio file: {e:#}", start.id));
    }
    persisted.seconds = seconds;
    persisted.done = true;
    write_state(&dir, &persisted);
    let text = stitcher.text();
    log::log(&format!(
        "session {} finished: {:.2}s audio, {} chars, {} failed segments",
        start.id,
        seconds,
        text.chars().count(),
        persisted.failed_segments
    ));
    ipc::write_live(
        &dirs.root,
        &ipc::LiveView {
            text: text.clone(),
            started: started_ms,
        },
    );
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
    });
}

#[allow(clippy::too_many_arguments)]
fn priority(kind: InteractorKind) -> Priority {
    match kind {
        InteractorKind::User => Priority::User,
        InteractorKind::Server => Priority::Api,
    }
}

fn decode_segment(
    asr: &Arc<Engine>,
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
            ipc::write_live(
                &dirs.root,
                &ipc::LiveView {
                    text: text.clone(),
                    started,
                },
            );
            if start.interactor.kind == InteractorKind::Server {
                if let Some(stream) = shared.lock().stream.as_mut() {
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
    let (limit, sensitivity, model_label) = {
        let data = shared.lock();
        (
            transcribe::limit_for_config(&data.config),
            data.config.vad_sensitivity,
            transcribe::selected_id(&data.config),
        )
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
        snapshot.running = false;
        snapshot.cancelled = true;
        snapshot.text = stitcher.text();
        ipc::write_file_state(&dirs.root, &snapshot);
        log::log(&format!("file {}: cancelled", path.display()));
        let _ = events.send(AppEvent::Cancelled);
        return;
    }
    if let Err(e) = result {
        snapshot.running = false;
        snapshot.error = Some(format!("{e:#}"));
        ipc::write_file_state(&dirs.root, &snapshot);
        let _ = events.send(AppEvent::Failed(format!("{e:#}")));
        return;
    }
    if let Some(tail) = segmenter.flush() {
        if !degraded && !decode_file_segment(&asr, &tail, &mut stitcher) {
            failed += 1;
        }
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
    use super::{mark_failed, pending, recordings_dir, write_state, PersistedState};

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
}
