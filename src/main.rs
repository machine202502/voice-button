#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;
use voice_button::recorder::{Recorder, Recording};
use voice_button::state::{
    self, ApiResult, Interactor, InteractorKind, ModelChoice, Session, Shared, Status,
};
use voice_button::{autostart, config, ipc, log, model_manager, tray, ui};

enum AppEvent {
    /// A session finished; `append` selects the append-to-last-message mode,
    /// `paste_target` carries the window to paste into (record-and-paste mode),
    /// `clipboard` says whether the text belongs to the user's clipboard (API
    /// audio goes back to its caller instead).
    Transcribed {
        text: String,
        model: String,
        append: bool,
        paste_target: Option<isize>,
        clipboard: bool,
    },
    Failed(String),
}

fn selected_model(config: &config::Config) -> String {
    match config.backend {
        config::Backend::Whisper => config.whisper_model.clone(),
        config::Backend::Onnx => config.onnx_model.clone(),
    }
}

/// The user's settings with the open API session's model override applied. The
/// saved settings are never modified, so the server cannot change what the user
/// picked in Settings.
fn effective_config(data: &state::Data) -> config::Config {
    let mut cfg = data.config.clone();
    if let Some(model) = data.session.as_ref().and_then(|session| session.model.as_ref()) {
        model.apply_to(&mut cfg);
    }
    cfg
}

/// A model the API server may use is one this build offers and that is already
/// on disk.
fn model_choice_ready(dirs: &config::AppDirs, model: &ModelChoice) -> bool {
    model_manager::find_model(&model.id)
        .map(|known| known.format == model.backend)
        .unwrap_or(false)
        && model_manager::is_ready(&dirs.models, &model.id)
}

/// Shared control between the main loop and the live-transcription worker.
struct LiveCtx {
    recording: Arc<AtomicBool>,
    final_tx: mpsc::SyncSender<Recording>,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: AtomicU32,
    /// Whether the current recording was started in the append-to-last mode;
    /// read back when the final transcript arrives.
    append_now: Arc<AtomicBool>,
    /// Target window for record-and-paste mode (0 = plain recording). Captured
    /// when recording starts, so the text goes back to the window the user was
    /// working in even if focus changes during transcription.
    paste_target: Arc<AtomicIsize>,
}

/// Time string used for the history entries and appended chunks.
fn now_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Windows can leave a virtual cable as the default input device; such devices
/// record silence, which looks like broken transcription. When no microphone
/// was chosen explicitly, prefer the first real device and persist it.
fn ensure_real_microphone(cfg: &mut config::Config, root: &std::path::Path) {
    if !cfg.input_device.is_empty() {
        return;
    }
    let Some(default) = voice_button::recorder::default_input_device_name() else {
        return;
    };
    if !voice_button::recorder::is_virtual_device_name(&default) {
        return;
    }
    let Some(real) = voice_button::recorder::first_real_input_device() else {
        return;
    };
    log::log(&format!(
        "default microphone {default:?} is a virtual device; selecting {real:?}"
    ));
    cfg.input_device = real;
    if let Err(e) = config::save_config(root, cfg) {
        log::log(&format!("failed to save the selected microphone: {e:#}"));
    }
}

/// Text copied for record-and-paste ends with the configured separator (a
/// space by default, or a line break), so the next dictation inserted at the
/// same caret does not run into this one. History keeps the clean text.
fn with_separator(text: &str, separator: config::PasteSeparator) -> String {
    let mut out = text.to_string();
    let ends_with_whitespace = out
        .chars()
        .last()
        .map(|c| c.is_whitespace())
        .unwrap_or(true);
    if !ends_with_whitespace {
        match separator {
            config::PasteSeparator::Space => out.push(' '),
            config::PasteSeparator::Newline => out.push('\n'),
        }
    }
    out
}

/// Window that is in the foreground right now, as a raw handle (0 when none).
fn foreground_window() -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    // SAFETY: GetForegroundWindow has no preconditions and only reads state.
    unsafe { GetForegroundWindow() as isize }
}

/// Window class name, empty when the window is gone.
fn window_class(hwnd: isize) -> String {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;
    if hwnd == 0 {
        return String::new();
    }
    let mut buffer = [0u16; 256];
    // SAFETY: buffer is valid for the given length; hwnd is only read.
    let len = unsafe {
        GetClassNameW(
            hwnd as windows_sys::Win32::Foundation::HWND,
            buffer.as_mut_ptr(),
            buffer.len() as i32,
        )
    };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..len as usize])
    }
}

/// Windows that must never receive the dictated text: the taskbar, the tray
/// overflow, the desktop and shell flyouts. Clicking the tray icon moves the
/// foreground to one of these, so they cannot become the paste target.
fn shell_window_class(class: &str) -> bool {
    matches!(
        class,
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "TrayNotifyWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
            | "Windows.UI.Core.CoreWindow"
            | "XamlExplorerHostIslandWindow"
            | "Progman"
            | "WorkerW"
            | "MultitaskingViewFrame"
            | "ForegroundStaging"
            | "tray_icon_app"
    )
}

/// Whether a window can receive the pasted text: alive, visible, owned by
/// another process and not part of the shell.
fn is_paste_window(hwnd: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    };
    if hwnd == 0 {
        return false;
    }
    // SAFETY: plain Win32 window queries on a handle supplied by the OS.
    unsafe {
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        if IsWindow(hwnd) == 0 || IsWindowVisible(hwnd) == 0 {
            return false;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == std::process::id() {
            return false;
        }
    }
    !shell_window_class(&window_class(hwnd))
}

/// The last window that was a real paste target. Clicking the tray icon makes
/// the taskbar the foreground window, so the target has to be remembered from
/// before the click.
static LAST_PASTE_TARGET: AtomicIsize = AtomicIsize::new(0);

/// Remember the current foreground window when it is a usable paste target.
fn track_paste_target() {
    let hwnd = foreground_window();
    if is_paste_window(hwnd) {
        LAST_PASTE_TARGET.store(hwnd, Ordering::SeqCst);
    }
}

/// Window to paste into: the current foreground when it is a real app window,
/// otherwise the last app window that had focus (the tray click case).
fn paste_target_window() -> isize {
    let current = foreground_window();
    if is_paste_window(current) {
        current
    } else {
        LAST_PASTE_TARGET.load(Ordering::SeqCst)
    }
}

/// Bring the remembered window back to the foreground. Windows may refuse the
/// focus change from a background process, so the result is verified instead of
/// trusted; a failed restore means "do not paste into a random window".
fn focus_window(hwnd: isize) -> bool {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, IsIconic, PeekMessageW, SetForegroundWindow,
        ShowWindow, MSG, PM_NOREMOVE, SW_RESTORE,
    };
    if hwnd == 0 {
        return false;
    }
    // SAFETY: plain Win32 window/thread queries; hwnd came from the OS and is
    // checked for validity by the calls themselves.
    unsafe {
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        if windows_sys::Win32::UI::WindowsAndMessaging::IsWindow(hwnd) == 0 {
            return false;
        }
        if GetForegroundWindow() == hwnd {
            return true;
        }
        // AttachThreadInput needs this thread to own a message queue; the
        // paste thread is fresh, so create the queue explicitly.
        let mut msg: MSG = core::mem::zeroed();
        PeekMessageW(&mut msg, core::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        // A background process usually cannot raise a window on its own.
        // Attaching to the target's input queue lifts that restriction for the
        // duration of the call.
        let current = GetCurrentThreadId();
        let target = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        let attached = target != 0 && AttachThreadInput(current, target, 1) != 0;
        let raised = SetForegroundWindow(hwnd);
        if attached {
            // Sharing the input queue also allows moving the keyboard focus,
            // which some applications need before they accept Ctrl+V.
            SetFocus(hwnd);
        }
        if attached {
            AttachThreadInput(current, target, 0);
        }
        raised != 0 && GetForegroundWindow() == hwnd
    }
}

/// Send Ctrl+V to the foreground window.
fn send_ctrl_v() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
    };
    fn key(vk: u16, up: bool) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
    let inputs = [
        key(VK_CONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_CONTROL, true),
    ];
    // SAFETY: the array is valid for the duration of the call; SendInput reads
    // exactly `len` INPUT-sized records.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            core::mem::size_of::<INPUT>() as i32,
        )
    };
    sent == inputs.len() as u32
}

/// Clipboard content saved before a record-and-paste run. Text and images can
/// be restored; other formats (HTML, files) cannot and are left replaced.
enum ClipboardSnapshot {
    Text(String),
    Image(arboard::ImageData<'static>),
}

fn snapshot_clipboard() -> Option<ClipboardSnapshot> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    if let Ok(text) = clipboard.get_text() {
        return Some(ClipboardSnapshot::Text(text));
    }
    clipboard.get_image().ok().map(ClipboardSnapshot::Image)
}

fn restore_clipboard(snapshot: ClipboardSnapshot) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return;
    };
    match snapshot {
        ClipboardSnapshot::Text(text) => {
            let _ = clipboard.set_text(text);
        }
        ClipboardSnapshot::Image(image) => {
            let _ = clipboard.set_image(image);
        }
    }
}

/// Paste the transcribed text into a window without keeping the clipboard
/// hijacked: the previous clipboard content is restored once the target
/// application had time to read the text.
fn paste_at_cursor(target: isize, previous: Option<ClipboardSnapshot>, pasted: String) {
    std::thread::spawn(move || {
        // Let the key that stopped the recording settle before we steal focus.
        std::thread::sleep(Duration::from_millis(120));
        log::log(&format!(
            "pasting into hwnd=0x{target:X} class={}",
            window_class(target)
        ));
        if !focus_window(target) {
            // The window cannot be raised (closed, elevated, or focus locked).
            // Keep the text in the clipboard so the user can paste manually.
            log::log(&format!(
                "paste skipped: could not focus hwnd=0x{target:X} (foreground is 0x{:X} class={})",
                foreground_window(),
                window_class(foreground_window())
            ));
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
        if foreground_window() != target {
            // Focus moved away again: do not type into an unrelated window.
            log::log(&format!(
                "paste skipped: focus left hwnd=0x{target:X} (now 0x{:X} class={})",
                foreground_window(),
                window_class(foreground_window())
            ));
            return;
        }
        if !send_ctrl_v() {
            log::log("paste failed: SendInput was rejected");
        } else {
            log::log("paste sent (Ctrl+V)");
        }
        // Give the application time to read the clipboard, then put the user's
        // previous clipboard back. If something else already replaced the
        // clipboard, leave it alone: restoring would discard that content.
        std::thread::sleep(Duration::from_millis(400));
        let clipboard_still_ours = arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.get_text())
            .map(|current| current == pasted)
            .unwrap_or(false);
        if clipboard_still_ours {
            if let Some(snapshot) = previous {
                restore_clipboard(snapshot);
                log::log("previous clipboard restored");
            }
        } else {
            log::log("clipboard changed after paste; previous clipboard not restored");
        }
    });
}

/// Merge a newly transcribed piece into the last history entry. The full
/// merged text becomes the entry text, so every "copy whole message" path
/// (clipboard after stop, tray menu, UI Copy button) keeps working unchanged.
fn merged_entry(prev: state::HistoryEntry, piece: &str) -> state::HistoryEntry {
    let piece = piece.trim();
    let mut text = prev.text.trim_end().to_string();
    // Every dictation in append mode starts on its own line, so the merged
    // message reads as a sequence of paragraphs both in the clipboard and in
    // the history view.
    if !text.is_empty() && !piece.is_empty() {
        text.push('\n');
    }
    text.push_str(piece);
    let mut entry = prev;
    entry.text = text;
    entry.parts.push(state::HistoryChunk {
        at: now_stamp(),
        text: piece.to_string(),
    });
    entry
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
    append: bool,
    paste_target: Option<isize>,
) {
    std::thread::spawn(move || {
        let config = effective_config(&shared.lock());
        let model = selected_model(&config);
        let event = match voice_button::transcribe::resolve(&config, &dirs)
            .and_then(|t| t.transcribe(recording))
        {
            Ok(text) => {
                log::log(&format!(
                    "transcription done ({} chars)",
                    text.chars().count()
                ));
                AppEvent::Transcribed {
                    text,
                    model,
                    append,
                    paste_target,
                    clipboard: true,
                }
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
    let peak = samples
        .iter()
        .fold(0.0f32, |max, sample| max.max(sample.abs()));
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
    let began = std::time::Instant::now();
    let result = transcriber.transcribe(Recording {
        samples: samples.to_vec(),
        sample_rate,
    });
    match &result {
        Ok(text) => log::log(&format!(
            "{kind} transcription: {seconds:.2}s audio, peak={peak:.4}, rms={rms:.4}, {} chars, {} ms",
            text.chars().count(),
            began.elapsed().as_millis()
        )),
        Err(e) => log::log(&format!(
            "{kind} transcription failed after {} ms: {e:#}",
            began.elapsed().as_millis()
        )),
    }
    if kind == "final" && peak < 0.0005 {
        log::log(
            "recording looks silent (peak below 0.0005): check the microphone selected in Settings",
        );
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
        let config = effective_config(&shared.lock());
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
        ipc::write_live(
            &dirs.root,
            &ipc::LiveView {
                text: String::new(),
                started,
            },
        );
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
                    match transcribe_stream_piece(
                        transcriber.as_ref(),
                        &samples,
                        sample_rate,
                        "live",
                    ) {
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
                // The append/paste modes were fixed when the recording started;
                // read the flags before decoding.
                let append = ctx.append_now.load(std::sync::atomic::Ordering::SeqCst);
                let paste_target = match ctx.paste_target.load(std::sync::atomic::Ordering::SeqCst)
                {
                    0 => None,
                    hwnd => Some(hwnd),
                };
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
                        AppEvent::Transcribed {
                            text,
                            model: model.clone(),
                            append,
                            paste_target,
                            clipboard: true,
                        }
                    }
                    Err(e) if !live_text.is_empty() => {
                        log::log(&format!(
                            "final transcription failed ({e:#}); keeping the last live transcript"
                        ));
                        AppEvent::Transcribed {
                            text: live_text,
                            model: model.clone(),
                            append,
                            paste_target,
                            clipboard: true,
                        }
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

/// Monotonic id of a recording session, so stops and results can be attributed
/// to the session that produced them. Starts at 1.
fn next_session_id() -> u64 {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

/// Parse the interactor out of a record-request file: the UI writes
/// `{"kind":"user"}`, anything else is treated as the user.
fn parse_interactor(content: &str) -> Interactor {
    let value: serde_json::Value = serde_json::from_str(content).unwrap_or(serde_json::Value::Null);
    if value.get("kind").and_then(|k| k.as_str()) == Some("server") {
        return Interactor::server();
    }
    Interactor::user()
}

/// Start or stop a recording on behalf of `interactor`. The session records who
/// owns it and, for an API session, the model it asked for. A stop is refused
/// when the caller may not stop the open session (the server never stops the
/// user's recording).
fn toggle_record(
    interactor: Interactor,
    model: Option<ModelChoice>,
    recorder: &mut Recorder,
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<tray::TrayCommand>,
    events_tx: mpsc::Sender<AppEvent>,
    append: bool,
    paste: bool,
) {
    let ctx = LIVE.get().expect("live context initialized");
    // An API session has no microphone of its own; the user's press means stop
    // it (they are privileged and can always end it).
    if shared.lock().stream.is_some() {
        if interactor.is_user() {
            stop_stream_session(shared, dirs, tray_cmds, interactor);
        }
        return;
    }
    if !recording_toggle_allowed(shared.lock().status, recorder.is_active()) {
        log::log("record start ignored while transcription is finishing");
        return;
    }
    if recorder.is_active() {
        // Stop permission: a human user may stop any session, the API server
        // only the sessions it started itself.
        let session = shared.lock().session.clone();
        let may_stop = session
            .as_ref()
            .map(|s| interactor.may_stop(s))
            .unwrap_or(true);
        if !may_stop {
            log::log("record stop rejected: interactor may not stop this session");
            return;
        }
        let recording = match recorder.stop() {
            Ok(r) => r,
            Err(e) => {
                log::log(&format!("failed to stop recording: {e:#}"));
                shared.set_error(format!("{e:#}"));
                set_status(shared, tray_cmds, dirs, Status::Idle);
                return;
            }
        };
        let append_now = ctx.append_now.load(Ordering::SeqCst);
        let paste_target = match ctx.paste_target.load(Ordering::SeqCst) {
            0 => None,
            hwnd => Some(hwnd),
        };
        ctx.recording.store(false, Ordering::SeqCst);
        // Who ended the session is part of its result: the server learns that
        // the user stopped its recording.
        if let Some(session) = shared.lock().session.as_mut() {
            session.stopped_by = Some(interactor);
        }
        // The end cue belongs to the finished transcription, not to the
        // keypress: it plays once the text is in the clipboard.
        set_status(shared, tray_cmds, dirs, Status::Transcribing);
        match ctx.final_tx.try_send(recording) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Disconnected(recording))
            | Err(mpsc::TrySendError::Full(recording)) => {
                // Live worker is gone (or overloaded): transcribe standalone.
                spawn_final_transcription(
                    shared.clone(),
                    dirs.clone(),
                    events_tx,
                    recording,
                    append_now,
                    paste_target,
                );
            }
        }
    } else {
        let preferred = shared.lock().config.input_device.clone();
        match recorder.start(Some(preferred.as_str())) {
            Ok(()) => {
                ctx.sample_rate
                    .store(recorder.sample_rate(), Ordering::SeqCst);
                ctx.append_now.store(append, Ordering::SeqCst);
                log::log(&format!(
                    "recording device: {} ({} Hz, {} ch)",
                    recorder.device_name(),
                    recorder.sample_rate(),
                    recorder.channels()
                ));
                // The paste target is captured now, while the user's window is
                // still the reference; transcription may take a while and focus
                // can move meanwhile. A tray click moves the foreground to the
                // taskbar, so fall back to the last real app window.
                let target = if paste { paste_target_window() } else { 0 };
                if paste {
                    log::log(&format!(
                        "paste target: hwnd=0x{target:X} class={}",
                        window_class(target)
                    ));
                }
                ctx.paste_target.store(target, Ordering::SeqCst);
                // The session must be visible to the live worker before it
                // picks up the recording, so the model override applies.
                {
                    let mut data = shared.lock();
                    data.session = Some(Session {
                        id: next_session_id(),
                        owner: interactor,
                        model,
                        stopped_by: None,
                    });
                }
                ipc::write_session(&dirs.root, shared.lock().session.as_ref());
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

/// Turn incoming audio into one session: the caller's sound replaces the local
/// microphone, so nothing here touches the recorder.
fn begin_stream_session(
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<tray::TrayCommand>,
    events_tx: mpsc::Sender<AppEvent>,
    model: Option<ModelChoice>,
    sample_rate: u32,
) -> u64 {
    let id = next_session_id();
    {
        let mut data = shared.lock();
        data.session = Some(Session {
            id,
            owner: Interactor::server(),
            model: model.clone(),
            stopped_by: None,
        });
        data.stream = Some(state::StreamSession {
            id,
            model,
            sample_rate,
            buffer: Arc::new(Mutex::new(Vec::new())),
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
    let (play, volume) = {
        let data = shared.lock();
        (data.config.play_start_sound, data.config.sound_volume)
    };
    if play {
        voice_button::sound::play_start(volume);
    }
    spawn_stream_worker(shared.clone(), dirs.clone(), events_tx, id);
    id
}

/// Stop receiving audio and let the worker decode the whole buffer. Kept in one
/// place so the caller and the user's window share exactly one mechanism.
fn stop_stream_session(
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<tray::TrayCommand>,
    interactor: Interactor,
) -> bool {
    let stopped = {
        let mut data = shared.lock();
        match data.stream.take() {
            Some(stream) => {
                // Tell the worker to finish; it holds its own handles.
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
        log::log("api stream session stopped, transcribing the received audio");
        set_status(shared, tray_cmds, dirs, Status::Transcribing);
    }
    stopped
}

/// Preview the received audio while it arrives and decode the final text once
/// the caller stops. Mirrors the live worker used for the local microphone.
fn spawn_stream_worker(
    shared: Arc<Shared>,
    dirs: config::AppDirs,
    events_tx: mpsc::Sender<AppEvent>,
    session_id: u64,
) {
    std::thread::spawn(move || {
        let (buffer, sample_rate, stop, config) = {
            let data = shared.lock();
            let Some(stream) = data.stream.as_ref().filter(|s| s.id == session_id) else {
                return;
            };
            (
                stream.buffer.clone(),
                stream.sample_rate,
                stream.stop.clone(),
                effective_config(&data),
            )
        };
        let model = selected_model(&config);
        let transcriber = match voice_button::transcribe::resolve(&config, &dirs) {
            Ok(transcriber) => transcriber,
            Err(e) => {
                log::log(&format!("stream transcription unavailable: {e:#}"));
                let _ = events_tx.send(AppEvent::Failed(format!("{e:#}")));
                return;
            }
        };
        // A live preview re-decodes the whole buffer, which only pays off for
        // the light ONNX models; Whisper stays quiet until the final decode.
        let preview = matches!(config.backend, config::Backend::Onnx);
        let interval = Duration::from_millis(1000);
        let mut last_attempt = std::time::Instant::now()
            .checked_sub(interval)
            .unwrap_or_else(std::time::Instant::now);
        let mut last_len = 0usize;
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
            if !preview || last_attempt.elapsed() < interval {
                continue;
            }
            let samples = buffer.lock().unwrap().clone();
            if samples.len() < last_len + sample_rate as usize / 4 {
                continue;
            }
            last_attempt = std::time::Instant::now();
            last_len = samples.len();
            match transcribe_stream_piece(
                transcriber.as_ref(),
                &samples,
                sample_rate,
                "stream live",
            ) {
                Ok(text) => {
                    if let Some(stream) = shared
                        .lock()
                        .stream
                        .as_mut()
                        .filter(|s| s.id == session_id)
                    {
                        stream.live_text = text;
                    }
                }
                Err(e) => log::log(&format!("stream live transcription failed: {e:#}")),
            }
        }
        let samples = buffer.lock().unwrap().clone();
        let event = if samples.is_empty() {
            AppEvent::Failed("no audio was received".to_string())
        } else {
            match transcribe_stream_piece(transcriber.as_ref(), &samples, sample_rate, "stream final")
            {
                Ok(text) => AppEvent::Transcribed {
                    text,
                    model,
                    append: false,
                    paste_target: None,
                    clipboard: false,
                },
                Err(e) => AppEvent::Failed(format!("{e:#}")),
            }
        };
        let _ = events_tx.send(event);
    });
}

/// Answer one command from the API server. The daemon owns the transcription
/// session, so the HTTP thread asks here instead of holding a model.
fn handle_api_request(
    request: voice_button::api::ApiRequest,
    recorder: &mut Recorder,
    shared: &Arc<Shared>,
    dirs: &config::AppDirs,
    tray_cmds: &mpsc::Sender<tray::TrayCommand>,
    events_tx: mpsc::Sender<AppEvent>,
) {
    use voice_button::api::{ApiAction, ApiResponse};
    let reply = request.reply;
    match request.action {
        ApiAction::StreamStart { model, sample_rate } => {
            let busy = {
                let data = shared.lock();
                data.session.is_some() || recorder.is_active()
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
        .args(["--ui", tab, "--parent", &std::process::id().to_string()])
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
    ipc::set_append_hotkey_capture(&dirs.root, false);
    ipc::set_paste_hotkey_capture(&dirs.root, false);
    // A session file left behind by a crashed run would keep the window
    // claiming that the API server is recording.
    ipc::write_session(&dirs.root, None);
    if !ensure_single_instance() {
        log::log("another daemon instance is already running, exiting");
        std::process::exit(0);
    }
    let mut cfg = config::load_config(&dirs.root);
    let first_run = !dirs.root.join("config.json").exists();
    // The tray menu is built from the catalog, so load it before the tray starts.
    voice_button::i18n::init(&dirs.root, &cfg.ui_language);
    ensure_real_microphone(&mut cfg, &dirs.root);
    if first_run {
        // Persist the language-suggested model so the choice is visible and
        // stable across restarts.
        if let Err(e) = config::save_config(&dirs.root, &cfg) {
            log::log(&format!("failed to save the first-run settings: {e:#}"));
        } else {
            log::log("first run: saved the defaults for the system language");
        }
    }
    if cfg.autostart {
        let _ = autostart::set_enabled(cfg.silent_start, true);
    }
    let history = state::load_history(&dirs.root);
    let shared = Arc::new(Shared::new(cfg.clone(), history));
    // Commands from the API server thread. The daemon owns the microphone, so
    // the HTTP layer asks here and gets one answer per request.
    let (api_tx, api_rx) = mpsc::channel::<voice_button::api::ApiRequest>();
    let mut api_server: Option<voice_button::api::ApiServer> = None;
    let mut api_applied: Option<(bool, String, u16)> = None;
    let (action_tx, action_rx) = mpsc::channel::<tray::TrayAction>();
    let tray_cmds = tray::start(
        action_tx,
        voice_button::hotkey::parse(&cfg.hotkey),
        voice_button::hotkey::parse(&cfg.append_hotkey),
        voice_button::hotkey::parse(&cfg.paste_hotkey),
        cfg.swap_mouse_buttons,
    );
    let (events_tx, events_rx) = mpsc::channel::<AppEvent>();
    let mut recorder = Recorder::new();
    let (final_tx, final_rx) = mpsc::sync_channel::<Recording>(1);
    let _ = LIVE.set(LiveCtx {
        recording: Arc::new(AtomicBool::new(false)),
        final_tx,
        buffer: recorder.buffer_handle(),
        sample_rate: AtomicU32::new(48_000),
        append_now: Arc::new(AtomicBool::new(false)),
        paste_target: Arc::new(AtomicIsize::new(0)),
    });
    spawn_live_worker(shared.clone(), dirs.clone(), events_tx.clone(), final_rx);
    let mut last_click: Option<std::time::Instant> = None;
    let mut last_hotkey_capture = false;
    let mut key_state = [false; 256];

    let mut last_model_ready: Option<bool> = None;
    let mut ui_language = cfg.ui_language.clone();
    let mut ui_child: Option<std::process::Child> = None;
    let mut last_config_mtime: Option<(u64, u32)> = None;
    let mut published_download: Option<String> = None;
    let mut last_publish = std::time::Instant::now();
    let mut last_rotate = std::time::Instant::now();
    set_status(&shared, &tray_cmds, &dirs, Status::Idle);
    loop {
        // Keep the last real foreground window around: a tray click puts the
        // taskbar in front, and the paste must still find the user's app.
        track_paste_target();
        let action = match action_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(action) => Some(action),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(action) = action {
            log::log(&format!("main got action {action:?}"));
            match &action {
                tray::TrayAction::RecordToggle
                | tray::TrayAction::RecordAppend
                | tray::TrayAction::RecordPaste => {
                    // Append records a piece for the last message; paste records
                    // a new message and inserts it at the remembered caret.
                    let append = matches!(action, tray::TrayAction::RecordAppend);
                    let paste = matches!(action, tray::TrayAction::RecordPaste);
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
                                Interactor::user(),
                                None,
                                &mut recorder,
                                &shared,
                                &dirs,
                                &tray_cmds,
                                events_tx.clone(),
                                append,
                                paste,
                            );
                        }
                    }
                }
                tray::TrayAction::OpenSettings => show_ui(&mut ui_child, "settings"),
                tray::TrayAction::OpenCapture => show_ui(&mut ui_child, "capture"),
                tray::TrayAction::CopyLast => match shared.lock().history.entry(0) {
                    Ok(entry) => copy_to_clipboard(&entry.text),
                    Err(e) => log::log(&format!("failed to read last history entry: {e:#}")),
                },
                tray::TrayAction::OpenModelsFolder => {
                    let _ = std::process::Command::new("explorer")
                        .arg(&dirs.models)
                        .spawn();
                }
                tray::TrayAction::Quit => break,
            }
        }
        while let Ok(event) = events_rx.try_recv() {
            match event {
                AppEvent::Transcribed {
                    text,
                    model,
                    append,
                    paste_target,
                    clipboard,
                } => {
                    // The session is still open while its text is being stored;
                    // it says who started this recording.
                    let session = shared.lock().session.clone();
                    let source = match session.as_ref().map(|s| s.owner.kind) {
                        Some(InteractorKind::Server) => state::Source::Server,
                        _ => state::Source::User,
                    };
                    let api_text = text.clone();
                    let api_model = model.clone();
                    // Append mode: the piece continues the last history entry;
                    // the clipboard gets the full merged message either way.
                    let previous = if append && !text.trim().is_empty() {
                        match shared.lock().history.entry(0) {
                            Ok(prev) => Some(prev),
                            Err(e) => {
                                // Nothing to append to (e.g. first ever
                                // recording): keep the piece as a fresh entry.
                                log::log(&format!("no history entry to append to: {e:#}"));
                                None
                            }
                        }
                    } else {
                        None
                    };
                    let text_is_empty = text.trim().is_empty();
                    let mut clipboard_text = String::new();
                    let mut clipboard_before: Option<ClipboardSnapshot> = None;
                    // A transcription of the caller's audio goes back to the
                    // caller; the user's clipboard stays untouched.
                    if clipboard {
                        let to_copy = previous
                            .as_ref()
                            .map(|prev| merged_entry(prev.clone(), &text).text)
                            .unwrap_or_else(|| text.clone());
                        let paste_mode = paste_target.is_some() && !text_is_empty;
                        // Record-and-paste leaves the configured separator in the
                        // clipboard so the next dictation starts apart.
                        let separator = shared.lock().config.paste_separator;
                        clipboard_text = if paste_mode {
                            with_separator(&to_copy, separator)
                        } else {
                            to_copy
                        };
                        // Record-and-paste borrows the clipboard only for Ctrl+V:
                        // save what the user had there before replacing it.
                        clipboard_before = if paste_mode {
                            snapshot_clipboard()
                        } else {
                            None
                        };
                        if !(paste_target.is_some() && text_is_empty) {
                            copy_to_clipboard(&clipboard_text);
                        }
                    }
                    // The transcription is complete and the text is already in
                    // the clipboard, so the end cue now means "text is ready".
                    let (play, volume) = {
                        let data = shared.lock();
                        (data.config.play_end_sound, data.config.sound_volume)
                    };
                    if play {
                        voice_button::sound::play_end(volume);
                    }
                    if !text_is_empty {
                        match previous {
                            Some(prev) => {
                                let merged = merged_entry(prev, &text);
                                // update_last replaces the newest row in
                                // place, both on disk and in the index.
                                let result = shared.lock().history.update_last(&dirs.root, &merged);
                                if let Err(e) = result {
                                    // Never lose the new piece: if the last
                                    // line cannot be rewritten, save the
                                    // merged message as a fresh entry.
                                    log::log(&format!(
                                        "failed to rewrite the last history entry ({e:#}); appending a new one"
                                    ));
                                    match voice_button::history::append(&dirs.root, &merged) {
                                        Ok(row) => shared.lock().history.push_row(row),
                                        Err(e) => {
                                            log::log(&format!("failed to append history: {e:#}"));
                                            shared.set_error(format!(
                                                "failed to save history: {e:#}"
                                            ));
                                        }
                                    }
                                }
                            }
                            None => {
                                let entry = state::HistoryEntry {
                                    at: now_stamp(),
                                    text,
                                    model,
                                    parts: Vec::new(),
                                    source,
                                };
                                match voice_button::history::append(&dirs.root, &entry) {
                                    Ok(row) => shared.lock().history.push_row(row),
                                    Err(e) => {
                                        log::log(&format!("failed to append history: {e:#}"));
                                        shared.set_error(format!("failed to save history: {e:#}"));
                                    }
                                }
                            }
                        }
                    }
                    // Put the text at the caret of the window the user was in
                    // when recording started. Runs on its own thread so the
                    // daemon keeps handling hotkeys while focus and the
                    // clipboard are juggled.
                    if let Some(hwnd) = paste_target.filter(|_| !text_is_empty) {
                        paste_at_cursor(hwnd, clipboard_before, clipboard_text);
                    }
                    // Close the session: the server (if it owns the session) can
                    // now read the text, and the next queued request may start.
                    let finished = shared.lock().session.take();
                    ipc::write_session(&dirs.root, None);
                    if let Some(session) = finished {
                        if session.owner.kind == InteractorKind::Server {
                            shared.lock().api_result = Some(ApiResult {
                                session: session.id,
                                text: api_text,
                                model: api_model,
                                at: now_stamp(),
                                stopped_by: session
                                    .stopped_by
                                    .map(|interactor| interactor.kind)
                                    .unwrap_or(InteractorKind::Server),
                                error: None,
                            });
                        }
                    }
                    set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                }
                AppEvent::Failed(msg) => {
                    shared.set_error(msg.clone());
                    // A failed API session may still be receiving; let its
                    // worker leave the loop instead of previewing forever.
                    if let Some(stream) = shared.lock().stream.take() {
                        stream.stop.store(true, Ordering::SeqCst);
                    }
                    let finished = shared.lock().session.take();
                    ipc::write_session(&dirs.root, None);
                    if let Some(session) = finished {
                        if session.owner.kind == InteractorKind::Server {
                            let model = selected_model(&effective_config(&shared.lock()));
                            shared.lock().api_result = Some(ApiResult {
                                session: session.id,
                                text: String::new(),
                                model,
                                at: now_stamp(),
                                stopped_by: session
                                    .stopped_by
                                    .map(|interactor| interactor.kind)
                                    .unwrap_or(InteractorKind::Server),
                                error: Some(msg),
                            });
                        }
                    }
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
        // API server → daemon: start/stop requests, answered per request.
        while let Ok(request) = api_rx.try_recv() {
            handle_api_request(
                request,
                &mut recorder,
                &shared,
                &dirs,
                &tray_cmds,
                events_tx.clone(),
            );
        }
        // UI → daemon: start/stop recording.
        if let Some(interactor_json) = ipc::take_record_request(&dirs.root) {
            let interactor = parse_interactor(&interactor_json);
            if recording_allowed(&shared, &dirs) || recorder.is_active() {
                // The UI start button always records a fresh message.
                toggle_record(
                    interactor,
                    None,
                    &mut recorder,
                    &shared,
                    &dirs,
                    &tray_cmds,
                    events_tx.clone(),
                    false,
                    false,
                );
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
            ensure_real_microphone(&mut cfg, &dirs.root);
            shared.lock().config = cfg.clone();
            if cfg.ui_language != ui_language {
                ui_language = cfg.ui_language.clone();
                voice_button::i18n::init(&dirs.root, &ui_language);
                // The menu labels were built at startup; rebuild them in place.
                let _ = tray_cmds.send(tray::TrayCommand::SetLanguage);
            }
            let _ = tray_cmds.send(tray::TrayCommand::SetHotkey(voice_button::hotkey::parse(
                &cfg.hotkey,
            )));
            let _ = tray_cmds.send(tray::TrayCommand::SetAppendHotkey(
                voice_button::hotkey::parse(&cfg.append_hotkey),
            ));
            let _ = tray_cmds.send(tray::TrayCommand::SetPasteHotkey(
                voice_button::hotkey::parse(&cfg.paste_hotkey),
            ));
            let _ = tray_cmds.send(tray::TrayCommand::SetMouseSwap(cfg.swap_mouse_buttons));
        }
        // API server lifecycle follows the settings: enabling, disabling or
        // moving it restarts the listener and republishes its real address.
        let wanted_api = (cfg.api_enabled, cfg.api_host.clone(), cfg.api_port);
        if api_applied.as_ref() != Some(&wanted_api) {
            api_applied = Some(wanted_api.clone());
            if let Some(server) = api_server.take() {
                server.stop();
            }
            if wanted_api.0 {
                match voice_button::api::ApiServer::bind(
                    &wanted_api.1,
                    wanted_api.2,
                    shared.clone(),
                    dirs.root.clone(),
                    api_tx.clone(),
                ) {
                    Ok(server) => {
                        let addr = server.addr();
                        log::log(&format!("api server listening on {addr}"));
                        ipc::write_api_state(
                            &dirs.root,
                            &ipc::ApiSnapshot {
                                running: true,
                                host: addr.ip().to_string(),
                                port: addr.port(),
                                error: None,
                            },
                        );
                        api_server = Some(server);
                    }
                    Err(e) => {
                        let error = format!(
                            "cannot listen on {}:{} ({e})",
                            wanted_api.1, wanted_api.2
                        );
                        log::log(&format!("api server not started: {error}"));
                        ipc::write_api_state(
                            &dirs.root,
                            &ipc::ApiSnapshot {
                                running: false,
                                host: wanted_api.1.clone(),
                                port: wanted_api.2,
                                error: Some(error),
                            },
                        );
                    }
                }
            } else {
                ipc::write_api_state(
                    &dirs.root,
                    &ipc::ApiSnapshot {
                        running: false,
                        host: String::new(),
                        port: 0,
                        error: None,
                    },
                );
            }
        }
        // The Settings window asks for the shortcut(s) to be released while it
        // captures a new combination, otherwise its keystrokes would toggle
        // recording instead of reaching the input.
        let capturing = ipc::hotkey_capture_active(&dirs.root)
            || ipc::append_hotkey_capture_active(&dirs.root)
            || ipc::paste_hotkey_capture_active(&dirs.root);
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
            let (record_hotkey, append_hotkey, paste_hotkey) = if capturing {
                (None, None, None)
            } else {
                (
                    voice_button::hotkey::parse(&cfg.hotkey),
                    voice_button::hotkey::parse(&cfg.append_hotkey),
                    voice_button::hotkey::parse(&cfg.paste_hotkey),
                )
            };
            let _ = tray_cmds.send(tray::TrayCommand::SetHotkey(record_hotkey));
            let _ = tray_cmds.send(tray::TrayCommand::SetAppendHotkey(append_hotkey));
            let _ = tray_cmds.send(tray::TrayCommand::SetPasteHotkey(paste_hotkey));
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
                    if allowed {
                        Status::Idle
                    } else {
                        Status::NoModel
                    },
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
    use super::{handle_api_request, merged_entry, recording_toggle_allowed, with_separator};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use voice_button::api::{ApiAction, ApiRequest, ApiResponse};
    use voice_button::config::{AppDirs, Config, PasteSeparator};
    use voice_button::history::History;
    use voice_button::recorder::Recorder;
    use voice_button::state::{
        HistoryChunk, HistoryEntry, Interactor, Session, Shared, Status, StreamSession,
    };

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            at: "2026-11-07 10:00:00".to_string(),
            text: text.to_string(),
            model: "model".to_string(),
            parts: Vec::new(),
            source: voice_button::state::Source::User,
        }
    }

    fn empty_shared() -> Arc<Shared> {
        Arc::new(Shared::new(Config::default(), History::from_entries(Vec::new())))
    }

    fn temp_dirs(name: &str) -> AppDirs {
        let root = std::env::temp_dir().join(format!(
            "voice-button-main-{name}-{}",
            std::process::id()
        ));
        let models = root.join("models");
        std::fs::create_dir_all(&models).unwrap();
        AppDirs { root, models }
    }

    fn user_session() -> Session {
        Session {
            id: 1,
            owner: Interactor::user(),
            model: None,
            stopped_by: None,
        }
    }

    /// Drive the daemon's API command handler exactly like the main loop does.
    fn ask(
        action: ApiAction,
        recorder: &mut Recorder,
        shared: &Arc<Shared>,
        dirs: &AppDirs,
    ) -> ApiResponse {
        let (reply, responses) = mpsc::channel();
        let (tray_cmds, _tray_rx) = mpsc::channel();
        let (events_tx, _events_rx) = mpsc::channel();
        handle_api_request(
            ApiRequest { action, reply },
            recorder,
            shared,
            dirs,
            &tray_cmds,
            events_tx,
        );
        responses
            .recv_timeout(Duration::from_secs(2))
            .expect("the daemon must answer every API command")
    }

    fn stream_session(id: u64) -> (Session, StreamSession) {
        (
            Session {
                id,
                owner: Interactor::server(),
                model: None,
                stopped_by: None,
            },
            StreamSession {
                id,
                model: None,
                sample_rate: 16_000,
                buffer: Arc::new(Mutex::new(Vec::new())),
                received_samples: 0,
                live_text: String::new(),
                stop: Arc::new(AtomicBool::new(false)),
            },
        )
    }

    #[test]
    fn a_second_session_is_refused_while_the_user_is_dictating() {
        let shared = empty_shared();
        shared.lock().session = Some(user_session());
        shared.lock().status = Status::Recording;
        let dirs = temp_dirs("api-busy");
        let mut recorder = Recorder::new();
        let answer = ask(
            ApiAction::StreamStart {
                model: None,
                sample_rate: 16_000,
            },
            &mut recorder,
            &shared,
            &dirs,
        );
        assert_eq!(
            answer,
            ApiResponse::Rejected("the app is busy with another session".to_string())
        );
        assert!(shared.lock().stream.is_none());
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn audio_chunks_are_appended_to_the_stream_buffer() {
        let shared = empty_shared();
        let (session, stream) = stream_session(3);
        shared.lock().session = Some(session);
        shared.lock().stream = Some(stream);
        shared.lock().status = Status::Recording;
        let dirs = temp_dirs("api-chunk");
        let mut recorder = Recorder::new();
        let answer = ask(
            ApiAction::StreamChunk {
                session: 3,
                samples: vec![0.1; 1600],
            },
            &mut recorder,
            &shared,
            &dirs,
        );
        assert_eq!(
            answer,
            ApiResponse::ChunkAccepted {
                received_ms: 100,
                live: String::new()
            }
        );
        assert_eq!(shared.lock().stream.as_ref().unwrap().buffer.lock().unwrap().len(), 1600);
        // A chunk for a session that is over is refused.
        let answer = ask(
            ApiAction::StreamChunk {
                session: 99,
                samples: vec![0.0; 8],
            },
            &mut recorder,
            &shared,
            &dirs,
        );
        assert!(matches!(answer, ApiResponse::Rejected(_)));
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn stopping_a_stream_marks_who_ended_it_and_goes_to_transcribing() {
        let shared = empty_shared();
        let (session, stream) = stream_session(4);
        shared.lock().session = Some(session);
        shared.lock().stream = Some(stream);
        shared.lock().status = Status::Recording;
        let dirs = temp_dirs("api-stop");
        let mut recorder = Recorder::new();
        let answer = ask(
            ApiAction::StreamStop { session: 4 },
            &mut recorder,
            &shared,
            &dirs,
        );
        assert_eq!(answer, ApiResponse::StreamStopped);
        assert!(shared.lock().stream.is_none());
        assert_eq!(shared.lock().status, Status::Transcribing);
        assert_eq!(
            shared.lock().session.as_ref().unwrap().stopped_by,
            Some(Interactor::server())
        );
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn the_user_can_end_an_api_session() {
        let shared = empty_shared();
        let (session, stream) = stream_session(5);
        shared.lock().session = Some(session);
        shared.lock().stream = Some(stream);
        let dirs = temp_dirs("api-user-stop");
        let (tray_cmds, _tray_rx) = mpsc::channel();
        assert!(super::stop_stream_session(
            &shared,
            &dirs,
            &tray_cmds,
            Interactor::user()
        ));
        assert!(shared.lock().stream.is_none());
        assert_eq!(
            shared.lock().session.as_ref().unwrap().stopped_by,
            Some(Interactor::user())
        );
        let _ = std::fs::remove_dir_all(&dirs.root);
    }

    #[test]
    fn transcribing_cannot_start_a_new_recording() {
        assert!(!recording_toggle_allowed(Status::Transcribing, false));
        assert!(recording_toggle_allowed(Status::Transcribing, true));
        assert!(recording_toggle_allowed(Status::Idle, false));
        assert!(recording_toggle_allowed(Status::Recording, true));
    }

    #[test]
    fn append_keeps_the_whole_message_and_records_the_piece() {
        let merged = merged_entry(entry("Первая мысль."), "  Вторая мысль.  ");
        assert_eq!(merged.text, "Первая мысль.\nВторая мысль.");
        assert_eq!(merged.parts.len(), 1);
        assert_eq!(merged.parts[0].text, "Вторая мысль.");
        // The original timestamp and model stay: the message was created once.
        assert_eq!(merged.at, "2026-11-07 10:00:00");
        assert_eq!(merged.model, "model");
    }

    #[test]
    fn append_twice_accumulates_pieces_on_new_lines() {
        let once = merged_entry(entry("Раз."), "Два.");
        let twice = merged_entry(once, "Три.");
        assert_eq!(twice.text, "Раз.\nДва.\nТри.");
        let texts: Vec<_> = twice
            .parts
            .iter()
            .map(|p: &HistoryChunk| p.text.clone())
            .collect();
        assert_eq!(texts, vec!["Два.", "Три."]);
    }

    #[test]
    fn append_to_an_empty_message_does_not_leave_a_leading_newline() {
        let merged = merged_entry(entry("   "), "Кусочек.");
        assert_eq!(merged.text, "Кусочек.");
    }

    #[test]
    fn paste_text_gets_exactly_one_space_by_default() {
        assert_eq!(with_separator("Привет", PasteSeparator::Space), "Привет ");
        assert_eq!(with_separator("Привет ", PasteSeparator::Space), "Привет ");
        assert_eq!(
            with_separator("Привет\n", PasteSeparator::Space),
            "Привет\n"
        );
        assert_eq!(with_separator("", PasteSeparator::Space), "");
    }

    #[test]
    fn paste_text_can_end_with_a_line_break() {
        assert_eq!(
            with_separator("Привет", PasteSeparator::Newline),
            "Привет\n"
        );
        assert_eq!(
            with_separator("Привет\n", PasteSeparator::Newline),
            "Привет\n"
        );
        assert_eq!(
            with_separator("Привет ", PasteSeparator::Newline),
            "Привет "
        );
    }
}
