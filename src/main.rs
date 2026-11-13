#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use voice_button::asr_engine::Engine;
use voice_button::daemon::{self, Runtime};
use voice_button::recorder::Recorder;
use voice_button::session::{self, SessionStart};
use voice_button::state::{
    self, ApiResult, AppEvent, Interactor, InteractorKind, ModelChoice, Session, Shared, Status,
};
use voice_button::{autostart, config, ipc, log, model_manager, tray, ui};

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

/// The end cue means "the text is ready on your screen". Background work has
/// no cue: nobody is waiting for it in front of the window. The phone is such
/// a task today, and the rule is the same for every one that follows.
fn end_cue(play: bool, background: bool) -> bool {
    play && !background
}

/// Text copied for record-and-paste ends with the configured separator (a
/// space by default, or a line break), so the next dictation inserted at the
/// same caret does not run into this one. History keeps the clean text.
fn with_separator(text: &str, separator: config::PasteSeparator) -> String {    let mut out = text.to_string();
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
    let runtime = daemon::runtime();
    // An API session has no microphone of its own; the user's press means stop
    // it (they are privileged and can always end it).
    if shared.lock().stream.is_some() {
        if interactor.is_user() {
            daemon::stop_stream_session(shared, dirs, tray_cmds, interactor);
        }
        return;
    }
    if shared.lock().recovering {
        log::log("record start ignored: an interrupted recording is being transcribed");
        return;
    }
    if !daemon::recording_toggle_allowed(shared.lock().status, recorder.is_active()) {
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
        if let Err(e) = recorder.stop() {
            log::log(&format!("failed to stop recording: {e:#}"));
        }
        // The session drains the last audio, decodes the tail and reports the
        // text through `AppEvent::Transcribed`.
        if let Some(handle) = runtime.session.lock().unwrap().take() {
            handle.stop();
        }
        // Who ended the session is part of its result: the server learns that
        // the user stopped its recording.
        if let Some(session) = shared.lock().session.as_mut() {
            session.stopped_by = Some(interactor);
        }
        // The end cue belongs to the finished transcription, not to the
        // keypress: it plays once the text is in the clipboard.
        daemon::set_status(shared, tray_cmds, dirs, Status::Transcribing);
        return;
    }

    let preferred = shared.lock().config.input_device.clone();
    match recorder.start(Some(preferred.as_str())) {
        Ok(()) => {
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
            let id = daemon::next_session_id();
            {
                let mut data = shared.lock();
                data.session = Some(Session {
                    id,
                    owner: interactor,
                    model: model.clone(),
                    stopped_by: None,
                });
            }
            ipc::write_session(&dirs.root, shared.lock().session.as_ref());
            let start = SessionStart {
                id,
                interactor,
                model,
                rate: recorder.sample_rate(),
                append,
                paste_target: (target != 0).then_some(target),
                clipboard: true,
                buffer: runtime.buffer.clone(),
            };
            match session::start(
                shared.clone(),
                dirs.clone(),
                events_tx,
                runtime.asr.clone(),
                start,
            ) {
                Ok(handle) => {
                    *runtime.session.lock().unwrap() = Some(handle);
                    ipc::clear_live(&dirs.root);
                    daemon::set_status(shared, tray_cmds, dirs, Status::Recording);
                    let (play, volume) = {
                        let data = shared.lock();
                        (data.config.play_start_sound, data.config.sound_volume)
                    };
                    if play {
                        voice_button::sound::play_start(volume);
                    }
                }
                Err(e) => {
                    log::log(&format!("cannot start the recording session: {e:#}"));
                    let _ = recorder.stop();
                    shared.lock().session = None;
                    ipc::write_session(&dirs.root, None);
                    shared.set_error(format!("{e:#}"));
                    daemon::set_status(shared, tray_cmds, dirs, Status::Idle);
                }
            }
        }
        Err(e) => {
            log::log(&format!("failed to start recording: {e:#}"));
            shared.set_error(format!("{e:#}"));
            daemon::set_status(shared, tray_cmds, dirs, Status::Idle);
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

/// Publish the phone server state for the UI: what is bound, where the QR
/// points, and whether a phone is connected.
fn publish_site_state(root: &std::path::Path, running: bool, port: u16, url: String, paired: bool) {
    ipc::write_site_state(
        root,
        &ipc::SiteSnapshot {
            running,
            port,
            url,
            paired,
        },
    );
}

/// What the window asked the daemon to change in `site/db.json`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SiteDbChange {
    pairing: bool,
    revoked: bool,
    archive: bool,
}

/// Apply the requests the window leaves as files to the daemon-owned phone
/// database. Only the daemon writes that file; the window only asks.
fn apply_site_db_requests(
    root: &std::path::Path,
    db: &mut voice_button::site::db::Db,
) -> SiteDbChange {
    let mut change = SiteDbChange::default();
    if ipc::take_site_pair_request(root) {
        let token = voice_button::site::gate::new_token();
        db.start_pairing(&token, &chrono::Local::now().to_rfc3339());
        change.pairing = true;
    }
    if ipc::take_site_revoke_request(root) {
        db.revoke();
        change.revoked = true;
    }
    if let Some(request) = ipc::take_site_archive_request(root) {
        db.archive_monolog(request.monolog, request.archived);
        change.archive = true;
    }
    if let Some(request) = ipc::take_telegram_token_request(root) {
        if let Err(error) =
            voice_button::telegram::write_secure_root(root, &request.token, &request.proxy)
        {
            voice_button::log::log(&format!("telegram: cannot store the token: {error:#}"));
        }
    }
    if ipc::take_telegram_reset(root) {
        voice_button::telegram::reset_owner(root);
    }
    if change != SiteDbChange::default() {
        if let Err(e) = db.save() {
            log::log(&format!("cannot save site/db.json: {e:#}"));
        }
    }
    change
}

/// Start, stop or move the phone server to match the settings.
///
/// The window writes `config.json` at any time, so the server cannot be created
/// once at startup: switching it on, off or to another port takes effect at
/// once, the same way the local API server already does.
fn apply_phone(
    root: &std::path::Path,
    enabled: bool,
    port: u16,
    shared: &Arc<Shared>,
    asr: &Arc<Engine>,
    phone: &mut Option<voice_button::site::runtime::PhoneRuntime>,
    paired: &mut Option<bool>,
) {
    if let Some(mut runtime) = phone.take() {
        runtime.stop();
    }
    if !enabled {
        publish_site_state(root, false, port, String::new(), false);
        *paired = None;
        return;
    }
    match voice_button::site::runtime::PhoneRuntime::start(root, port, shared.clone(), asr.clone(), None) {
        Ok(runtime) => {
            let url = voice_button::site::gate::display_url(runtime.port);
            let connected = runtime.hub().db_handle().lock().unwrap().data().client.paired;
            log::log(&format!("phone: listening on port {} at {url}", runtime.port));
            publish_site_state(root, true, runtime.port, url, connected);
            *paired = Some(connected);
            *phone = Some(runtime);
        }
        Err(e) => {
            log::log(&format!("phone: did not start: {e:#}"));
            publish_site_state(root, false, port, String::new(), false);
            *paired = None;
        }
    }
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

/// Only one daemon instance may run per data directory; a second start exits
/// quietly. The mutex handle lives for the whole process. The directory is part
/// of the name, so a test run with its own `VB_DATA_DIR` does not fight the
/// real application.
fn ensure_single_instance(root: &std::path::Path) -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in root.to_string_lossy().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let name: Vec<u16> = format!("Local\\voice-button-daemon-{hash:016x}\0")
        .encode_utf16()
        .collect();
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
/// Transcribe recordings the previous run did not finish. Runs on its own
/// thread: the tray and the API keep working while it decodes, and the
/// microphone stays blocked until it is done.
/// How long the audio of a not-guaranteed transcription is kept.
const KEEP_AUDIO_DAYS: u64 = 30;

fn start_recovery(
    shared: Arc<Shared>,
    dirs: config::AppDirs,
    events: mpsc::Sender<AppEvent>,
    asr: Arc<Engine>,
    tray_cmds: mpsc::Sender<tray::TrayCommand>,
    crashed: bool,
) {
    let pending = session::pending(&dirs.root);
    session::clean_finished(&dirs.root);
    // Kept recordings are linked from the history; drop the ones nobody will
    // open any more so the data directory stays bounded.
    session::cleanup_kept(&dirs.root, KEEP_AUDIO_DAYS);
    if !crashed && pending.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("recovery".into())
        .spawn(move || {
            shared.lock().recovering = true;
            daemon::set_status(&shared, &tray_cmds, &dirs, Status::Recovering);
            // Loud only for a real crash: the guard reports a non-zero exit
            // code within seconds, while a shutdown or a kill leaves no report
            // and must not beep at the next start.
            let loud = crashed
                && (!pending.is_empty() || log::crash_report_recent(&dirs.root, 600));
            if crashed {
                log::append_crash_report(&dirs.root, "the daemon restarted after an unclean exit");
            }
            if loud {
                let (play, volume) = {
                    let data = shared.lock();
                    (data.config.play_error_sound, data.config.sound_volume)
                };
                if play {
                    voice_button::sound::play_error(volume);
                }
                let _ = tray_cmds.send(tray::TrayCommand::Notify(
                    voice_button::i18n::tr("Voice Button: recovering the interrupted recording"),
                ));
            }
            let mut recovered = 0usize;
            let cfg = config::load_config(&dirs.root);
            let sensitivity = cfg.vad_sensitivity;
            for dir in pending {
                let state = session::read_state(&dir).unwrap_or_default();
                let model = state.model.clone();
                let limit = match voice_button::state::ModelChoice::parse(&model) {
                    Some(choice) => voice_button::transcribe::segment_limit(&choice),
                    None => voice_button::transcribe::limit_for_config(&cfg),
                };
                match session::recover(&asr, &dir, &model, limit, sensitivity) {
                    Ok((text, seconds, failed)) => {
                        if text.trim().is_empty() {
                            log::log(&format!(
                                "recovery: {} produced no text, keeping the audio and not retrying it",
                                dir.display()
                            ));
                            // Retrying at every start would crash the app on
                            // every start when the model itself is broken.
                            session::mark_failed(&dir);
                            let _ = tray_cmds.send(tray::TrayCommand::Notify(
                                voice_button::i18n::tr(
                                    "Voice Button: the recording could not be transcribed, the audio is kept",
                                ),
                            ));
                            continue;
                        }
                        let entry = state::HistoryEntry {
                            at: now_stamp(),
                            text: text.clone(),
                            model: if model.is_empty() {
                                "unknown model".to_string()
                            } else {
                                model
                            },
                            parts: Vec::new(),
                            source: match state.interactor {
                                InteractorKind::Server => state::Source::Server,
                                _ => state::Source::User,
                            },
                            state: session::recovered_state(failed),
                            audio: (failed > 0)
                                .then(|| session::audio_path(&dir).display().to_string()),
                            group: None,
                            message: None,
                        };
                        match voice_button::history::append(&dirs.root, &entry) {
                            Ok(row) => shared.lock().history.push_row(row),
                            Err(e) => {
                                log::log(&format!("recovery: cannot save the text: {e:#}"));
                                continue;
                            }
                        }
                        recovered += 1;
                        log::log(&format!(
                            "recovery: {} transcribed ({:.1}s audio, {} chars, {} failed segments)",
                            dir.display(),
                            seconds,
                            text.chars().count(),
                            failed
                        ));
                        if failed > 0 {
                            // Not guaranteed: keep the file and let the user
                            // open it from the history.
                            session::keep_for_user(&dir);
                        } else {
                            session::discard(&dir);
                        }
                        let _ = events.send(AppEvent::Recovered {
                            chars: text.chars().count(),
                            failed_segments: failed,
                            at: now_stamp(),
                        });
                    }
                    Err(e) => {
                        log::log(&format!(
                            "recovery: {} could not be transcribed: {e:#}",
                            dir.display()
                        ));
                        // Keep the audio but stop retrying it: a model that
                        // always crashes would otherwise crash the app on
                        // every start.
                        session::mark_failed(&dir);
                        let _ = tray_cmds.send(tray::TrayCommand::Notify(
                            voice_button::i18n::tr(
                                "Voice Button: the recording could not be transcribed, the audio is kept",
                            ),
                        ));
                    }
                }
            }
            shared.lock().recovering = false;
            daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
            if recovered > 0 {
                let _ = tray_cmds.send(tray::TrayCommand::Notify(
                    voice_button::i18n::tr("Voice Button: the interrupted recording is in the history"),
                ));
            }
        });
}

fn run_daemon() {
    let dirs = config::app_dirs();
    // A second instance must not rotate the log or leave a crash flag behind:
    // it exits before touching anything the running daemon owns. The snapshot
    // mode shares that rule: it writes into the real profile too, so it may only
    // run when the app is closed.
    if !ensure_single_instance(&dirs.root) {
        std::process::exit(0);
    }
    log::init(&dirs.root);
    // The flag distinguishes a crash from a clean quit: it is removed on a
    // normal exit and the guard records the exit code of a dying daemon.
    let crashed = log::previous_run_crashed(&dirs.root);
    log::mark_running(&dirs.root);
    // A capture request left behind by a crashed settings window would keep the
    // hotkey released forever, so start from a clean slate.
    ipc::set_hotkey_capture(&dirs.root, false);
    ipc::set_append_hotkey_capture(&dirs.root, false);
    ipc::set_paste_hotkey_capture(&dirs.root, false);
    // A session file left behind by a crashed run would keep the window
    // claiming that the API server is recording.
    ipc::write_session(&dirs.root, None);
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
    // The phone layer is started once, below, when the settings say so. The
    // daemon keeps this runtime for the whole session and stops it on exit.
    let mut phone: Option<voice_button::site::runtime::PhoneRuntime> = None;
    let mut phone_applied: Option<(bool, u16)>;
    let mut last_site_paired: Option<bool> = None;
    let mut last_site_check = std::time::Instant::now();
    let mut last_background = false;
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
    // The recognizer runs in `voice-button.exe --asr-worker`, started lazily on
    // the first segment: a crash inside ONNX Runtime then costs one process,
    // never the recording.
    let asr = Arc::new(Engine::new(
        std::env::current_exe().unwrap_or_else(|_| "voice-button.exe".into()),
    ));
    // The Telegram bot waits for its switch and token, then polls in its own
    // threads; it is lower priority than the phone and the microphone.
    voice_button::telegram::spawn(dirs.clone(), shared.clone(), asr.clone());
    // The phone layer: the runtime opens the database, starts the queue pump
    // and binds the HTTPS port. Binding happens here, so a busy port is
    // reported instead of vanishing into a thread.
    phone_applied = Some((cfg.site_enabled, cfg.site_port));
    apply_phone(
        &dirs.root,
        cfg.site_enabled,
        cfg.site_port,
        &shared,
        &asr,
        &mut phone,
        &mut last_site_paired,
    );
    daemon::install(Runtime {
        buffer: recorder.buffer_handle(),
        asr: asr.clone(),
        session: Mutex::new(None),
        file: Mutex::new(None),
    });
    // Recordings interrupted by the previous crash are transcribed before the
    // microphone is offered again.
    start_recovery(
        shared.clone(),
        dirs.clone(),
        events_tx.clone(),
        asr.clone(),
        tray_cmds.clone(),
        crashed,
    );
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
    daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
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
                    if !daemon::recording_allowed(&shared, &dirs) {
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
                tray::TrayAction::OpenHistory => show_ui(&mut ui_child, "history"),
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
                    state: entry_state,
                    dir,
                    source,
                    audio,
                    at,
                } => {
                    // The session is still open while its text is being stored;
                    // it says who started this recording.
                    let api_text = text.clone();
                    let api_model = model.clone();
                    // A partial transcription is not guaranteed: the audio stays
                    // and the history points at it, so the user can open the
                    // file instead of losing the words.
                    let mut saved_audio: Option<String> = audio;


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
                    // Whether the text really reached the history: the audio is
                    // only deleted when it did. An empty text from a session
                    // whose engine kept failing is not a result: keep the
                    // audio so recovery can retry it later.
                    let mut saved = text_is_empty && !entry_state.is_partial();
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
                    if end_cue(play, source == voice_button::history::Source::Site) {
                        voice_button::sound::play_end(volume);
                    }
                    if !text_is_empty {
                        match previous {
                            Some(prev) => {
                                let mut merged = merged_entry(prev, &text);
                                if entry_state.is_partial() {
                                    merged.state = entry_state;
                                    merged.audio = entry_state
                                        .is_partial()
                                        .then(|| dir.as_ref().map(|dir| session::audio_path(dir).display().to_string()))
                                        .flatten();
                                    saved_audio = merged.audio.clone();
                                }
                                // update_last replaces the newest row in
                                // place, both on disk and in the index.
                                let result = shared.lock().history.update_last(&dirs.root, &merged);
                                if result.is_ok() {
                                    saved = true;
                                }
                                if let Err(e) = result {
                                    // Never lose the new piece: if the last
                                    // line cannot be rewritten, save the
                                    // merged message as a fresh entry.
                                    log::log(&format!(
                                        "failed to rewrite the last history entry ({e:#}); appending a new one"
                                    ));
                                    match voice_button::history::append(&dirs.root, &merged) {
                                        Ok(row) => {
                                            shared.lock().history.push_row(row);
                                            saved = true;
                                        }
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
                                let audio = entry_state
                                    .is_partial()
                                    .then(|| dir.as_ref().map(|dir| session::audio_path(dir).display().to_string()))
                                    .flatten();
                                saved_audio = audio.clone();
                                // The phone owns the time of its own messages:
                                // a message recorded offline and delivered an
                                // hour later still belongs to the hour it was
                                // spoken.
                                let entry = state::HistoryEntry {
                                    at: at.unwrap_or_else(now_stamp),
                                    text,
                                    model,
                                    parts: Vec::new(),
                                    source,
                                    state: entry_state,
                                    audio,
                                    group: None,
                                    message: None,
                                };
                                match voice_button::history::append(&dirs.root, &entry) {
                                    Ok(row) => {
                                        shared.lock().history.push_row(row);
                                        saved = true;
                                    }
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
                    // Guaranteed text: the audio is not needed. Partial text or
                    // a failed save: keep it, either for the user (linked from
                    // the history) or for the recovery at the next start.
                    if let Some(dir) = dir {
                        if saved && saved_audio.is_none() {
                            session::discard(&dir);
                        } else if saved {
                            session::keep_for_user(&dir);
                        } else {
                            session::keep_for_recovery(&dir);
                        }
                    }
                    // Close the session: the server (if it owns the session) can
                    // now read the text, and the next request may start.
                    let finished = shared.lock().session.take();
                    if let Some(runtime) = daemon::try_runtime() {
                        *runtime.session.lock().unwrap() = None;
                        *runtime.file.lock().unwrap() = None;
                    }
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
                    if entry_state.is_partial() {
                        let _ = tray_cmds.send(tray::TrayCommand::Notify(
                            voice_button::i18n::tr(
                                "Voice Button: part of the recording was not transcribed",
                            ),
                        ));
                    }
                    ipc::clear_live(&dirs.root);
                    daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                }
                AppEvent::Cancelled => {
                    if let Some(runtime) = daemon::try_runtime() {
                        *runtime.file.lock().unwrap() = None;
                    }
                    ipc::clear_live(&dirs.root);
                    daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                }
                AppEvent::Recovered {
                    chars,
                    failed_segments,
                    at,
                } => {
                    log::log(&format!(
                        "recovered the recording from {at}: {chars} chars, {failed_segments} failed segments"
                    ));
                }
                AppEvent::Failed(msg) => {
                    shared.set_error(msg.clone());
                    if let Some(runtime) = daemon::try_runtime() {
                        *runtime.session.lock().unwrap() = None;
                    }
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
                    ipc::clear_live(&dirs.root);
                    daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                }
            }
        }
        // UI → daemon: cancel the imported file that is being transcribed.
        if ipc::file_cancel_requested(&dirs.root) {
            ipc::clear_file_cancel(&dirs.root);
            if let Some(runtime) = daemon::try_runtime() {
                if let Some(handle) = runtime.file.lock().unwrap().take() {
                    handle.cancel();
                }
            }
        }
        // UI → daemon: transcribe an audio file the user picked.
        if let Some(path) = ipc::take_file_request(&dirs.root) {
            let name = std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("audio")
                .to_string();
            let busy = {
                let data = shared.lock();
                data.session.is_some()
                    || recorder.is_active()
                    || data.recovering
                    || matches!(
                        data.status,
                        Status::Recording | Status::Transcribing | Status::File
                    )
            };
            if let Some(runtime) = daemon::try_runtime() {
                if busy {
                    ipc::write_file_state(
                        &dirs.root,
                        &ipc::FileSnapshot {
                            path: path.clone(),
                            name,
                            error: Some(voice_button::i18n::tr(
                                "The app is busy with another recording",
                            )),
                            running: false,
                            ..Default::default()
                        },
                    );
                } else {
                    daemon::set_status(&shared, &tray_cmds, &dirs, Status::File);
                    match session::start_file(
                        shared.clone(),
                        dirs.clone(),
                        events_tx.clone(),
                        runtime.asr.clone(),
                        std::path::PathBuf::from(&path),
                    ) {
                        Ok(handle) => *runtime.file.lock().unwrap() = Some(handle),
                        Err(e) => {
                            log::log(&format!("cannot start the file transcription: {e:#}"));
                            ipc::write_file_state(
                                &dirs.root,
                                &ipc::FileSnapshot {
                                    path: path.clone(),
                                    name,
                                    error: Some(format!("{e:#}")),
                                    running: false,
                                    ..Default::default()
                                },
                            );
                            daemon::set_status(&shared, &tray_cmds, &dirs, Status::Idle);
                        }
                    }
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
            daemon::handle_api_request(
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
            if daemon::recording_allowed(&shared, &dirs) || recorder.is_active() {
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
        // Violet tray icon while any background task runs: the microphone stays
        // available, and no sound is played for that work.
        let background = shared.lock().background_active();
        if background != last_background {
            last_background = background;
            let _ = tray_cmds.send(tray::TrayCommand::SetBackground(background));
        }
        // The phone server follows the settings the way the local API does:
        // switching it on, off or to another port happens without a restart.
        let wanted_phone = (cfg.site_enabled, cfg.site_port);
        if phone_applied != Some(wanted_phone) {
            phone_applied = Some(wanted_phone);
            apply_phone(
                &dirs.root,
                wanted_phone.0,
                wanted_phone.1,
                &shared,
                &asr,
                &mut phone,
                &mut last_site_paired,
            );
        }
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
        // "Connect phone" and "Disconnect" arrive as request files, like the
        // file import does: the UI process never touches the phone storage.
        // The daemon owns the database and applies the change there.
        if let Some(runtime) = phone.as_ref() {
            let db = runtime.hub().db_handle();
            let url = voice_button::site::gate::display_url(runtime.port);
            let change = {
                let mut db = db.lock().unwrap();
                apply_site_db_requests(&dirs.root, &mut db)
            };
            if change.pairing {
                log::log("phone: a new pairing token was issued");
            }
            if change.revoked {
                log::log("phone: the token was revoked");
            }
            if change.pairing || change.revoked {
                let paired = db.lock().unwrap().data().client.paired;
                publish_site_state(&dirs.root, true, runtime.port, url.clone(), paired);
            }
            // The pairing happens inside the server thread: the window only
            // knows what is published, so a change must be published.
            if last_site_check.elapsed() >= Duration::from_secs(1) {
                last_site_check = std::time::Instant::now();
                let paired = db.lock().unwrap().data().client.paired;
                if last_site_paired != Some(paired) {
                    last_site_paired = Some(paired);
                    publish_site_state(&dirs.root, true, runtime.port, url, paired);
                }
            }
        } else if let Some(request) = ipc::take_site_archive_request(&dirs.root) {
            // With the server off the daemon still owns the file and may
            // archive a monolog: nothing else writes it right now.
            match voice_button::site::db::Db::open(&dirs.root) {
                Ok(mut db) => {
                    db.archive_monolog(request.monolog, request.archived);
                    if let Err(e) = db.save() {
                        log::log(&format!("cannot save site/db.json: {e:#}"));
                    }
                }
                Err(e) => log::log(&format!("cannot open site/db.json: {e:#}")),
            }
        }
        // Internet access: the script opens the firewall and, where it can,
        // the router. It shows a UAC prompt, so it runs on its own thread.
        if !ipc::site_network_busy(&dirs.root) {
            if let Some(enable) = ipc::take_site_network(&dirs.root) {
                ipc::set_site_network_busy(&dirs.root, true);
                let root = dirs.root.clone();
                let port = cfg.site_port;
                let internal = voice_button::site::gate::private_ipv4()
                    .map(|ip| ip.to_string())
                    .unwrap_or_default();
                let skip_upnp = voice_button::site::gate::has_public_ipv4();
                std::thread::spawn(move || {
                    let result = if enable {
                        if let Some(state) = voice_button::site::network::read(&root) {
                            if voice_button::site::network::stale(&state, port) {
                                let _ = voice_button::site::network::clear(&root);
                            }
                        }
                        voice_button::site::network::apply(&root, port, &internal, skip_upnp)
                    } else {
                        voice_button::site::network::clear(&root)
                    };
                    if let Err(e) = result {
                        // The script exits non-zero when the prompt is
                        // dismissed: say so instead of leaving the last good
                        // state on screen.
                        log::log(&format!("network: {e:#}"));
                        voice_button::site::network::record_failure(
                            &root,
                            &voice_button::i18n::tr(
                                "The setup did not finish. Approve the administrator prompt and try again.",
                            ),
                        );
                    }
                    ipc::set_site_network_busy(&root, false);
                });
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
        let allowed = daemon::recording_allowed(&shared, &dirs);
        if last_model_ready != Some(allowed) {
            last_model_ready = Some(allowed);
            let status = shared.lock().status;
            if status == Status::Idle || status == Status::NoModel {
                daemon::set_status(
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
    // Stop the phone layer before the process goes away: the queue thread and
    // the HTTPS port must not outlive the daemon.
    if let Some(mut runtime) = phone.take() {
        runtime.stop();
    }
    publish_site_state(&dirs.root, false, cfg.site_port, String::new(), false);
    shared.lock().quit = true;
    // A clean exit must not look like a crash to the guard or the next start.
    log::clear_running(&dirs.root);
    let _ = tray_cmds.send(tray::TrayCommand::Shutdown);
    std::process::exit(0);
}

pub fn copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_owned());
    }
}

fn main() {
    // Child roles first: they are plain processes with their own stdio and must
    // not touch the tray, the microphone or the single-instance mutex.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "--asr-worker") {
        voice_button::asr_worker::run();
    }
    if args.iter().any(|arg| arg == "--guard") {
        voice_button::guard::run();
    }
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
    use super::{end_cue, merged_entry, with_separator};
    use voice_button::daemon;
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

    #[test]
    fn background_work_never_plays_the_end_cue() {
        // The phone is a background task: the person is not waiting for it, so
        // there is no cue. A local dictation still gets one.
        assert!(!end_cue(true, true));
        assert!(!end_cue(false, false));
        assert!(end_cue(true, false));
    }

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            at: "2026-11-07 10:00:00".to_string(),
            text: text.to_string(),
            model: "model".to_string(),
            parts: Vec::new(),
            source: voice_button::state::Source::User,
            state: voice_button::history::EntryState::Complete,
            audio: None,
            group: None,
            message: None,
        }
    }

    /// The daemon sets the runtime once; the tests that drive the API handler
    /// need the same slots to exist.
    fn init_runtime() {
        voice_button::daemon::install(voice_button::daemon::Runtime {
            buffer: Arc::new(Mutex::new(Vec::new())),
            asr: Arc::new(voice_button::asr_engine::Engine::new(
                std::path::PathBuf::from("voice-button.exe"),
            )),
            session: Mutex::new(None),
            file: Mutex::new(None),
        });
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
        init_runtime();
        let (reply, responses) = mpsc::channel();
        let (tray_cmds, _tray_rx) = mpsc::channel();
        let (events_tx, _events_rx) = mpsc::channel();
        daemon::handle_api_request(
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
        assert!(super::daemon::stop_stream_session(
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
        assert!(!daemon::recording_toggle_allowed(Status::Transcribing, false));
        assert!(daemon::recording_toggle_allowed(Status::Transcribing, true));
        assert!(daemon::recording_toggle_allowed(Status::Idle, false));
        assert!(daemon::recording_toggle_allowed(Status::Recording, true));
    }

    #[test]
    fn recovering_blocks_new_recordings() {
        assert!(!daemon::recording_toggle_allowed(Status::Recovering, false));
        assert!(!daemon::recording_toggle_allowed(Status::Recovering, true));
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

    #[test]
    fn an_archive_request_reaches_the_daemon_database() {
        let dirs = temp_dirs("archive-request");
        let id = {
            let mut db = voice_button::site::db::Db::open(&dirs.root).unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            db.save().unwrap();
            monolog.id
        };
        // The window only leaves a request file; the daemon applies it.
        voice_button::ipc::request_site_archive(&dirs.root, id, true);
        let change = {
            let mut db = voice_button::site::db::Db::open(&dirs.root).unwrap();
            super::apply_site_db_requests(&dirs.root, &mut db)
        };
        assert!(change.archive);
        assert!(!dirs.root.join("site.archive.request").exists(), "consumed");
        let db = voice_button::site::db::Db::open(&dirs.root).unwrap();
        assert!(db.monolog(id).unwrap().archived, "the daemon saved it");

        voice_button::ipc::request_site_archive(&dirs.root, id, false);
        let mut db = voice_button::site::db::Db::open(&dirs.root).unwrap();
        super::apply_site_db_requests(&dirs.root, &mut db);
        let db = voice_button::site::db::Db::open(&dirs.root).unwrap();
        assert!(!db.monolog(id).unwrap().archived, "and it can come back");
        let _ = std::fs::remove_dir_all(&dirs.root);
    }
}
