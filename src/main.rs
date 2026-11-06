#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use voice_button::recorder::{Recorder, Recording};
use voice_button::state::{self, Shared, Status};
use voice_button::{autostart, config, ipc, log, model_manager, tray, ui};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

enum AppEvent {
    Transcribed(String, String),
    Failed(String),
}

fn selected_model(config: &config::Config) -> String {
    match config.backend {
        config::Backend::Whisper => config.whisper_model.clone(),
        config::Backend::Onnx => config.onnx_model.clone(),
    }
}

/// Shared control between the main loop and the live-transcription worker.
struct LiveCtx {
    recording: Arc<AtomicBool>,
    final_tx: mpsc::SyncSender<Recording>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: AtomicU32,
}

static LIVE: OnceLock<LiveCtx> = OnceLock::new();

fn set_status(
    shared: &Shared,
    tray_cmds: &mpsc::Sender<tray::TrayCommand>,
    dirs: &config::AppDirs,
    status: Status,
) {
    shared.lock().status = status;
    ipc::write_status(&dirs.root, status);
    let _ = tray_cmds.send(tray::TrayCommand::SetStatus(status));
}

/// Recording is allowed only when the selected model is usable AND no
/// download is currently running.
fn recording_allowed(shared: &Shared, dirs: &config::AppDirs) -> bool {
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

/// Transcribe a finished recording and report the result, used as a fallback
/// when the live worker is unavailable.
fn spawn_final_transcription(
    shared: Arc<Shared>,
    dirs: config::AppDirs,
    events_tx: mpsc::Sender<AppEvent>,
    recording: Recording,
) {
    std::thread::spawn(move || {
        let config = shared.lock().config.clone();
        let model = selected_model(&config);
        let event = match voice_button::transcribe::resolve(&config, &dirs)
            .and_then(|t| t.transcribe(recording))
        {
            Ok(text) => {
                log::log(&format!("transcription done ({} chars)", text.chars().count()));
                AppEvent::Transcribed(text, model)
            }
            Err(e) => {
                log::log(&format!("transcription failed: {e:#}"));
                AppEvent::Failed(format!("{e:#}"))
            }
        };
        let _ = events_tx.send(event);
    });
}

fn transcribe_stream_piece(
    transcriber: &dyn voice_button::transcribe::Transcriber,
    samples: &[f32],
    sample_rate: u32,
    kind: &str,
) -> anyhow::Result<String> {
    let seconds = samples.len() as f64 / sample_rate as f64;
    let began = std::time::Instant::now();
    let result = transcriber.transcribe(Recording {
        samples: samples.to_vec(),
        sample_rate,
    });
    match &result {
        Ok(text) => log::log(&format!(
            "{kind} transcription: {seconds:.2}s audio, {} chars, {} ms",
            text.chars().count(),
            began.elapsed().as_millis()
        )),
        Err(e) => log::log(&format!(
            "{kind} transcription failed after {} ms: {e:#}",
            began.elapsed().as_millis()
        )),
    }
    result
}

/// Transcribe the growing buffer while recording: every pass decodes the whole
/// utterance so far and replaces the preview text. Stopping always re-decodes
/// the complete recording, so the result never depends on where a live pass
/// happened to stop.
fn spawn_live_worker(
    shared: Arc<Shared>,
    dirs: config::AppDirs,
    events_tx: mpsc::Sender<AppEvent>,
    final_rx: mpsc::Receiver<Recording>,
) {
    std::thread::spawn(move || loop {
        let Some(ctx) = LIVE.get() else { return };
        if !ctx.recording.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let config = shared.lock().config.clone();
        let model = selected_model(&config);
        let transcriber = match voice_button::transcribe::resolve(&config, &dirs) {
            Ok(t) => t,
            Err(e) => {
                let message = format!("{e:#}");
                log::log(&format!("live transcription unavailable: {message}"));
                let started = chrono::Local::now().timestamp_millis();
                ipc::write_live(
                    &dirs.root,
                    &ipc::LiveView {
                        text: format!("Transcription unavailable: {message}"),
                        started,
                    },
                );
                // Keep the recording state coherent and consume the final
                // buffer on Stop; otherwise the UI would remain Transcribing.
                while ctx.recording.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = final_rx.recv_timeout(Duration::from_secs(5));
                let _ = events_tx.send(AppEvent::Failed(message));
                ipc::clear_live(&dirs.root);
                continue;
            }
        };
        let started = chrono::Local::now().timestamp_millis();
        ipc::write_live(&dirs.root, &ipc::LiveView { text: String::new(), started });
        let sample_rate = ctx.sample_rate.load(Ordering::SeqCst);
        // A live preview re-decodes the whole buffer, which only pays off for
        // the light GigaAM models. Whisper needs seconds per pass, so it stays
        // quiet until the final decode instead of starving the CPU.
        let preview = matches!(config.backend, crate::config::Backend::Onnx);
        let attempt_interval = Duration::from_millis(1000);
        let mut last_attempt = std::time::Instant::now()
            .checked_sub(attempt_interval)
            .unwrap_or_else(std::time::Instant::now);
        let mut last_len = 0usize;
        let mut live_text = String::new();
        while ctx.recording.load(Ordering::SeqCst) {
            if preview && last_attempt.elapsed() >= attempt_interval {
                let samples = ctx.buffer.lock().unwrap().clone();
                // Re-decode the whole utterance so far and replace the preview.
                // A partial window cuts words and loses the context the model
                // needs, which is what the old eight-second commits did.
                if samples.len() >= last_len + sample_rate as usize / 4 {
                    last_attempt = std::time::Instant::now();
                    last_len = samples.len();
                    match transcribe_stream_piece(transcriber.as_ref(), &samples, sample_rate, "live") {
                        Ok(text) => {
                            if text != live_text {
                                live_text.clone_from(&text);
                                ipc::write_live(&dirs.root, &ipc::LiveView { text, started });
                            }
                        }
                        Err(e) => log::log(&format!("live transcription failed: {e:#}")),
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        match final_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(recording) => {
                let seconds = recording.samples.len() as f64 / recording.sample_rate.max(1) as f64;
                // Always re-decode the complete recording: the live preview is
                // only a hint, never the result.
                let event = match transcribe_stream_piece(
                    transcriber.as_ref(),
                    &recording.samples,
                    recording.sample_rate,
                    "final",
                ) {
                    Ok(text) => {
                        log::log(&format!(
                            "transcription done ({} chars, {seconds:.2}s audio)",
                            text.chars().count()
                        ));
                        AppEvent::Transcribed(text, model.clone())
                    }
                    Err(e) if !live_text.is_empty() => {
                        log::log(&format!(
                            "final transcription failed ({e:#}); keeping the last live transcript"
                        ));
                        AppEvent::Transcribed(live_text, model.clone())
                    }
                    Err(e) => AppEvent::Failed(format!("{e:#}")),
                };
                let _ = events_tx.send(event);
            }
            Err(_) => log::log("no final recording received by live worker"),
        }
        ipc::clear_live(&dirs.root);
    });
}

fn recording_toggle_allowed(status: Status, recorder_active: bool) -> bool {
    recorder_active || status != Status::Transcribing
}

fn toggle_record(
    recorder: &mut Recorder,
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<tray::TrayCommand>,
    events_tx: mpsc::Sender<AppEvent>,
) {
    let ctx = LIVE.get().expect("live context initialized");
    if !recording_toggle_allowed(shared.lock().status, recorder.is_active()) {
        log::log("record start ignored while transcription is finishing");
        return;
    }
    if recorder.is_active() {
        let recording = match recorder.stop() {
            Ok(r) => r,
            Err(e) => {
                log::log(&format!("failed to stop recording: {e:#}"));
                shared.set_error(format!("{e:#}"));
                set_status(shared, tray_cmds, dirs, Status::Idle);
                return;
            }
        };
        ctx.recording.store(false, Ordering::SeqCst);
        // The end cue belongs to the finished transcription, not to the
        // keypress: it plays once the text is in the clipboard.
        set_status(shared, tray_cmds, dirs, Status::Transcribing);
        match ctx.final_tx.try_send(recording) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Disconnected(recording))
            | Err(mpsc::TrySendError::Full(recording)) => {
                // Live worker is gone (or overloaded): transcribe standalone.
                spawn_final_transcription(shared.clone(), dirs.clone(), events_tx, recording);
            }
        }
    } else {
        match recorder.start() {
            Ok(()) => {
                ctx.sample_rate.store(recorder.sample_rate(), Ordering::SeqCst);
                ctx.recording.store(true, Ordering::SeqCst);
                ipc::clear_live(&dirs.root);
                set_status(shared, tray_cmds, dirs, Status::Recording);
                let (play, volume) = {
                    let data = shared.lock();
                    (data.config.play_start_sound, data.config.sound_volume)
                };
                if play {
                    voice_button::sound::play_start(volume);
                }
            }
            Err(e) => {
                log::log(&format!("failed to start recording: {e:#}"));
                shared.set_error(format!("{e:#}"));
                set_status(shared, tray_cmds, dirs, Status::Idle);
            }
        }
    }
}

/// Bring the existing UI window to the front, if the UI process is running.
fn focus_ui_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow};
    let mut title: Vec<u16> = ui::WINDOW_TITLE.encode_utf16().collect();
    title.push(0);
    // SAFETY: title is a valid NUL-terminated UTF-16 string.
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if !hwnd.is_null() {
        // SAFETY: hwnd is the UI window we located.
        unsafe {
            SetForegroundWindow(hwnd);
        }
    }
}

/// The UI binary next to the daemon.
fn sibling_ui_exe() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap_or_else(|_| "voice-button.exe".into());
    exe.parent()
        .map(|dir| dir.join("voice-button-ui.exe"))
        .unwrap_or_else(|| "voice-button-ui.exe".into())
}

fn show_ui(child: &mut Option<std::process::Child>, tab: &str) {
    if let Some(c) = &mut *child {
        match c.try_wait() {
            Ok(None) => {
                focus_ui_window();
                return;
            }
            _ => *child = None,
        }
    }
    let spawn_exe = sibling_ui_exe();
    if !spawn_exe.exists() {
        log::log(&format!("UI binary not found: {}", spawn_exe.display()));
        return;
    }
    match std::process::Command::new(&spawn_exe)
        .args([
            "--ui",
            tab,
            "--parent",
            &std::process::id().to_string(),
        ])
        .spawn()
    {
        Ok(c) => *child = Some(c),
        Err(e) => log::log(&format!("failed to spawn UI: {e:#}")),
    }
}

/// Only one daemon instance may run; a second start exits quietly. The mutex
/// handle lives for the whole process.
fn ensure_single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\voice-button-daemon\0".encode_utf16().collect();
    // SAFETY: name is a valid NUL-terminated UTF-16 string.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return true;
    }
    // SAFETY: no state needed beyond the error value.
    unsafe { GetLastError() != ERROR_ALREADY_EXISTS }
}


/// Watch the physical keyboard while the Settings window captures a shortcut.
/// Returns the canonical text of the first complete combination, or "cancel" for
/// a bare Escape. Bare letters are ignored: they are ordinary typing.
fn poll_capture_shortcut(key_state: &mut [bool; 256]) -> Option<String> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    fn down(vk: u32) -> bool {
        // SAFETY: GetAsyncKeyState has no preconditions and reads the system
        // key state; the high bit means the key is currently down.
        unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
    }
    // Generic, left/right and Windows modifiers must not become the key part of
    // the shortcut: pressing Ctrl alone produces both VK_CONTROL and VK_LCONTROL.
    const MODIFIERS: [u32; 11] = [
        0x10, 0x11, 0x12, 0x5B, 0x5C, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
    ];
    for vk in 8..=254u32 {
        let is_down = down(vk);
        let was = key_state[vk as usize];
        key_state[vk as usize] = is_down;
        if !is_down || was || MODIFIERS.contains(&vk) {
            continue;
        }
        let ctrl = down(0x11);
        let alt = down(0x12);
        let shift = down(0x10);
        let win = down(0x5B) || down(0x5C);
        if vk == 0x1B && !(ctrl || alt || shift || win) {
            return Some("cancel".to_string());
        }
        if !(ctrl || alt || shift || win) && !(0x70..=0x87).contains(&vk) {
            continue;
        }
        let hotkey = voice_button::hotkey::Hotkey {
            ctrl,
            alt,
            shift,
            win,
            vk,
        };
        return Some(hotkey.label());
    }
    None
}
fn run_daemon() {
    let dirs = config::app_dirs();
    log::init(&dirs.root);
    // A capture request left behind by a crashed settings window would keep the
    // hotkey released forever, so start from a clean slate.
    ipc::set_hotkey_capture(&dirs.root, false);
    if !ensure_single_instance() {
        log::log("another daemon instance is already running, exiting");
        std::process::exit(0);
    }
    let mut cfg = config::load_config(&dirs.root);
    if cfg.autostart {
        let _ = autostart::set_enabled(cfg.silent_start, true);
    }
    let history = state::load_history(&dirs.root);
    let shared = Arc::new(Shared::new(cfg.clone(), history));
    let (action_tx, action_rx) = mpsc::channel::<tray::TrayAction>();
    let tray_cmds = tray::start(action_tx, voice_button::hotkey::parse(&cfg.hotkey));
    let (events_tx, events_rx) = mpsc::channel::<AppEvent>();
    let mut recorder = Recorder::new();
    let (final_tx, final_rx) = mpsc::sync_channel::<Recording>(1);
    let _ = LIVE.set(LiveCtx {
        recording: Arc::new(AtomicBool::new(false)),
        final_tx,
        buffer: recorder.buffer_handle(),
        sample_rate: AtomicU32::new(48_000),
    });
    spawn_live_worker(shared.clone(), dirs.clone(), events_tx.clone(), final_rx);
    let mut last_click: Option<std::time::Instant> = None;
    let mut last_hotkey_capture = false;
    let mut key_state = [false; 256];

    let mut last_model_ready: Option<bool> = None;
    let mut ui_child: Option<std::process::Child> = None;
    let mut last_config_mtime: Option<(u64, u32)> = None;
    let mut published_download: Option<String> = None;
    let mut last_publish = std::time::Instant::now();
    let mut last_rotate = std::time::Instant::now();
    set_status(&shared, &tray_cmds, &dirs, Status::Idle);
    loop {
        let action = match action_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(action) => Some(action),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(action) = action {
            log::log(&format!("main got action {action:?}"));
            match action {
                tray::TrayAction::RecordToggle => {
                    if !recording_allowed(&shared, &dirs) {
                        // No usable model or a download is running: point the
                        // user at Settings instead of failing to record.
                        show_ui(&mut ui_child, "settings");
                    } else {
                        let now = std::time::Instant::now();
                        let not_double_click = last_click
                            .map(|t| now.duration_since(t) > Duration::from_millis(450))
                            .unwrap_or(true);
                        if not_double_click {
                            last_click = Some(now);
                            toggle_record(
                                &mut recorder,
                                &shared,
                                &dirs,
                                &tray_cmds,
                                events_tx.clone(),
                            );
                        }
                    }
                }
                tray::TrayAction::OpenSettings => show_ui(&mut ui_child, "settings"),
                tray::TrayAction::OpenCapture => show_ui(&mut ui_child, "capture"),
                tray::TrayAction::CopyLast => {
                    match shared.lock().history.entry(0) {
                        Ok(entry) => copy_to_clipboard(&entry.text),
                        Err(e) => log::log(&format!("failed to read last history entry: {e:#}")),
                    }
                }
                tray::TrayAction::OpenModelsFolder => {
                    let _ = std::process::Command::new("explorer").arg(&dirs.models).spawn();
                }
                tray::TrayAction::Quit => break,
            }
        }
        while let Ok(event) = events_rx.try_recv() {
            match event {
                AppEvent::Transcribed(text, model) => {
                    copy_to_clipboard(&text);
                    // The transcription is complete and the text is already in
                    // the clipboard, so the end cue now means "text is ready".
                    let (play, volume) = {
                        let data = shared.lock();
                        (data.config.play_end_sound, data.config.sound_volume)
                    };
                    if play {
                        voice_button::sound::play_end(volume);
                    }
                    if !text.trim().is_empty() {
                        let at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                        let entry = state::HistoryEntry { at, text, model };
                        match voice_button::history::append(&dirs.root, &entry) {
                            Ok(row) => shared.lock().history.push_row(row),
                            Err(e) => {
                                log::log(&format!("failed to append history: {e:#}"));
                                shared.set_error(format!("failed to save history: {e:#}"));
                            }
                        }
                    }
                    set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                }
                AppEvent::Failed(msg) => {
                    shared.set_error(msg);
                    set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                }
            }
        }
        // UI → daemon: model download requests. The download thread is owned
        // by the daemon, so it keeps running while the UI is closed.
        if let Some(model_id) = ipc::take_download_request(&dirs.root) {
            let busy = shared
                .lock()
                .download
                .as_ref()
                .map(|d| !d.done && d.error.is_none())
                .unwrap_or(false);
            if !busy {
                model_manager::start_download(shared.clone(), dirs.models.clone(), model_id);
            }
        }
        // UI → daemon: start/stop recording.
        if ipc::take_record_request(&dirs.root) {
            if recording_allowed(&shared, &dirs) || recorder.is_active() {
                toggle_record(&mut recorder, &shared, &dirs, &tray_cmds, events_tx.clone());
            } else {
                show_ui(&mut ui_child, "settings");
            }
        }
        // Publish download progress for the UI process (throttled).
        let snap_string = {
            let data = shared.lock();
            data.download.as_ref().map(|dl| {
                serde_json::to_string(&ipc::DownloadSnapshot {
                    model: dl.model.clone(),
                    file: dl.file,
                    files: dl.files,
                    downloaded: dl.downloaded,
                    total: dl.total,
                    done_bytes: dl.done_bytes,
                    done_total: dl.done_total,
                    done: dl.done,
                    error: dl.error.clone(),
                })
                .unwrap_or_default()
            })
        };
        if snap_string != published_download && last_publish.elapsed() > Duration::from_millis(300)
        {
            last_publish = std::time::Instant::now();
            match &snap_string {
                Some(s) if !s.is_empty() => {
                    ipc::write_download_state(
                        &dirs.root,
                        &serde_json::from_str::<ipc::DownloadSnapshot>(s).unwrap_or_default(),
                    );
                }
                _ => ipc::remove_download_state(&dirs.root),
            }
            published_download = snap_string;
        }
        // Config changes written by the UI process.
        let mtime_key = std::fs::metadata(dirs.root.join("config.json"))
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()));
        if mtime_key.is_some() && mtime_key != last_config_mtime {
            last_config_mtime = mtime_key;
            cfg = config::load_config(&dirs.root);
            shared.lock().config = cfg.clone();
            let _ = tray_cmds.send(tray::TrayCommand::SetHotkey(voice_button::hotkey::parse(
                &cfg.hotkey,
            )));
        }
        // The Settings window asks for the shortcut to be released while it
        // captures a new combination, otherwise its keystrokes would toggle
        // recording instead of reaching the input.
        let capturing = ipc::hotkey_capture_active(&dirs.root);
        if capturing {
            // The settings window may not have keyboard focus and a released
            // global hotkey no longer swallows the keys, so the daemon watches
            // the physical keyboard itself and reports the combination back.
            if let Some(text) = poll_capture_shortcut(&mut key_state) {
                crate::log::log(&format!("capture shortcut: {text}"));
                ipc::write_hotkey_result(&dirs.root, &text);
            }
        }
        if capturing != last_hotkey_capture {
            last_hotkey_capture = capturing;
            let _ = tray_cmds.send(tray::TrayCommand::SetHotkey(if capturing {
                None
            } else {
                voice_button::hotkey::parse(&cfg.hotkey)
            }));
        }
        // Keep logs bounded: rotate log.txt before it grows past the limit.
        if last_rotate.elapsed() > Duration::from_secs(2) {
            last_rotate = std::time::Instant::now();
            if log::rotate_if_needed() {
                log::log("log file rotated");
            }
        }
        // Gray out the tray icon while recording is unavailable (no usable
        // model or a download is running).
        let allowed = recording_allowed(&shared, &dirs);
        if last_model_ready != Some(allowed) {
            last_model_ready = Some(allowed);
            let status = shared.lock().status;
            if status == Status::Idle || status == Status::NoModel {
                set_status(
                    &shared,
                    &tray_cmds,
                    &dirs,
                    if allowed { Status::Idle } else { Status::NoModel },
                );
            }
        }
    }
    if let Some(mut child) = ui_child {
        let _ = child.kill();
    }
    shared.lock().quit = true;
    let _ = tray_cmds.send(tray::TrayCommand::Shutdown);
    std::process::exit(0);
}

pub fn copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_owned());
    }
}

fn main() {
    // Snapshot/test mode: the UI is a separate binary; run it and mirror its
    // exit code so the integration test can assert on it.
    if let Ok(dir) = std::env::var("VB_SNAPSHOT") {
        let ui_exe = sibling_ui_exe();
        let code = std::process::Command::new(&ui_exe)
            .env("VB_SNAPSHOT", dir)
            .status()
            .map(|s| s.code().unwrap_or(1))
            .unwrap_or(1);
        std::process::exit(code);
    }
    run_daemon();
}

#[cfg(test)]
mod tests {
    use super::recording_toggle_allowed;
    use voice_button::state::Status;

    #[test]
    fn transcribing_cannot_start_a_new_recording() {
        assert!(!recording_toggle_allowed(Status::Transcribing, false));
        assert!(recording_toggle_allowed(Status::Transcribing, true));
        assert!(recording_toggle_allowed(Status::Idle, false));
        assert!(recording_toggle_allowed(Status::Recording, true));
    }
}
