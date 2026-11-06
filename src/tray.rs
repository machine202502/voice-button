use crate::hotkey::Hotkey;
use crate::icons;
use crate::state::Status;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

/// Identifiers of the three global hotkeys this process owns.
const HOTKEY_ID: i32 = 1;
const APPEND_HOTKEY_ID: i32 = 2;
const PASTE_HOTKEY_ID: i32 = 3;

#[derive(Clone, Debug)]
pub enum TrayAction {
    RecordToggle,
    /// Start/stop a recording whose result is appended to the last message.
    RecordAppend,
    /// Start/stop a recording whose result is pasted at the caret of the
    /// window that was active when recording started.
    RecordPaste,
    OpenSettings,
    OpenCapture,
    CopyLast,
    OpenModelsFolder,
    Quit,
}

pub enum TrayCommand {
    SetStatus(Status),
    /// Apply a new global shortcut; `None` disables it.
    SetHotkey(Option<Hotkey>),
    /// Apply a new append shortcut; `None` disables it.
    SetAppendHotkey(Option<Hotkey>),
    /// Apply a new paste shortcut; `None` disables it.
    SetPasteHotkey(Option<Hotkey>),
    Shutdown,
}

pub fn start(
    to_app: Sender<TrayAction>,
    hotkey: Option<Hotkey>,
    append_hotkey: Option<Hotkey>,
    paste_hotkey: Option<Hotkey>,
) -> Sender<TrayCommand> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        crate::log::log("tray thread starting");
        run(to_app, rx, hotkey, append_hotkey, paste_hotkey);
    });
    tx
}

/// Set by the hidden hotkey window's procedure when a shortcut fires.
static HOTKEY_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static APPEND_HOTKEY_FIRED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static PASTE_HOTKEY_FIRED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Window procedure for the hidden window that owns the global hotkeys. Windows
/// posts WM_HOTKEY to this window rather than to the thread queue, which is the
/// only delivery form that survives a busy tray loop.
unsafe extern "system" fn hotkey_window_proc(
    hwnd: *mut core::ffi::c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_HOTKEY;
    if msg == WM_HOTKEY && wparam == HOTKEY_ID as usize {
        HOTKEY_FIRED.store(true, std::sync::atomic::Ordering::SeqCst);
        return 0;
    }
    if msg == WM_HOTKEY && wparam == APPEND_HOTKEY_ID as usize {
        APPEND_HOTKEY_FIRED.store(true, std::sync::atomic::Ordering::SeqCst);
        return 0;
    }
    if msg == WM_HOTKEY && wparam == PASTE_HOTKEY_ID as usize {
        PASTE_HOTKEY_FIRED.store(true, std::sync::atomic::Ordering::SeqCst);
        return 0;
    }
    windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Create the hidden window that receives WM_HOTKEY. Runs on the tray thread.
fn create_hotkey_window() -> *mut core::ffi::c_void {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, RegisterClassW, WNDCLASSW};
    let class_name: Vec<u16> = "VoiceButtonHotkey\0".encode_utf16().collect();
    // SAFETY: the class and window are registered on this thread with a valid
    // procedure; the window stays hidden and lives for the thread's lifetime.
    unsafe {
        let instance = GetModuleHandleW(core::ptr::null());
        let mut class: WNDCLASSW = core::mem::zeroed();
        class.lpfnWndProc = Some(hotkey_window_proc);
        class.hInstance = instance;
        class.lpszClassName = class_name.as_ptr();
        RegisterClassW(&class);
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            instance,
            core::ptr::null(),
        )
    }
}

/// Replace a registered shortcut on this thread's message queue.
fn apply_hotkey(
    window: *mut core::ffi::c_void,
    id: i32,
    current: &mut Option<Hotkey>,
    next: Option<Hotkey>,
    kind: &str,
) {
    unsafe {
        if current.is_some() {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(window, id);
        }
    }
    let had = current.is_some();
    *current = None;
    if next.is_none() && had {
        crate::log::log(&format!("{kind} hotkey disabled for shortcut capture"));
    }
    if let Some(hotkey) = next {
        // SAFETY: registering a thread-wide hotkey needs no window handle; the
        // id is unique to this process, and the modifiers/vk come from a parsed
        // shortcut.
        let registered = unsafe {
            windows_sys::Win32::UI::Input::KeyboardAndMouse::RegisterHotKey(
                window,
                id,
                hotkey.modifiers(),
                hotkey.vk,
            )
        };
        if registered != 0 {
            crate::log::log(&format!("{kind} hotkey registered: {}", hotkey.label()));
            *current = Some(hotkey);
        } else {
            crate::log::log(&format!(
                "{kind} hotkey registration failed: {}",
                hotkey.label()
            ));
        }
    }
}

fn run(
    to_app: Sender<TrayAction>,
    rx_cmd: Receiver<TrayCommand>,
    hotkey: Option<Hotkey>,
    append_hotkey: Option<Hotkey>,
    paste_hotkey: Option<Hotkey>,
) {
    let menu = Menu::new();
    let copy_last = MenuItem::new("Copy last transcription", true, None);
    let settings = MenuItem::new("Settings", true, None);
    let history = MenuItem::new("Capture", true, None);
    let open_folder = MenuItem::new("Open models folder", true, None);
    let separator = PredefinedMenuItem::separator();
    let quit = MenuItem::new("Quit", true, None);
    menu.append_items(&[
        &copy_last,
        &settings,
        &history,
        &open_folder,
        &separator,
        &quit,
    ])
    .expect("failed to build tray menu");
    let settings_id = settings.id().clone();
    let history_id = history.id().clone();
    let copy_last_id = copy_last.id().clone();
    let open_folder_id = open_folder.id().clone();
    let quit_id = quit.id().clone();
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Voice Button - left click to record")
        .with_icon(icons::icon_idle())
        .build()
        .expect("failed to create tray icon");
    crate::log::log("tray icon created");
    let hotkey_window = create_hotkey_window();
    let mut registered: Option<Hotkey> = None;
    let mut append_registered: Option<Hotkey> = None;
    let mut paste_registered: Option<Hotkey> = None;
    apply_hotkey(hotkey_window, HOTKEY_ID, &mut registered, hotkey, "record");
    apply_hotkey(
        hotkey_window,
        APPEND_HOTKEY_ID,
        &mut append_registered,
        append_hotkey,
        "append",
    );
    apply_hotkey(
        hotkey_window,
        PASTE_HOTKEY_ID,
        &mut paste_registered,
        paste_hotkey,
        "paste",
    );
    loop {
        pump_messages();
        if HOTKEY_FIRED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            crate::log::log("hotkey pressed");
            let _ = to_app.send(TrayAction::RecordToggle);
        }
        if APPEND_HOTKEY_FIRED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            crate::log::log("append hotkey pressed");
            let _ = to_app.send(TrayAction::RecordAppend);
        }
        if PASTE_HOTKEY_FIRED.swap(false, std::sync::atomic::Ordering::SeqCst) {
            crate::log::log("paste hotkey pressed");
            let _ = to_app.send(TrayAction::RecordPaste);
        }
        while let Ok(command) = rx_cmd.try_recv() {
            match command {
                TrayCommand::SetStatus(status) => {
                    let icon = match status {
                        Status::Idle => icons::icon_idle(),
                        Status::Recording => icons::icon_recording(),
                        Status::Transcribing => icons::icon_transcribing(),
                        Status::NoModel => icons::icon_disabled(),
                    };
                    let _ = tray.set_icon(Some(icon));
                }
                TrayCommand::SetHotkey(next) => {
                    apply_hotkey(hotkey_window, HOTKEY_ID, &mut registered, next, "record")
                }
                TrayCommand::SetAppendHotkey(next) => apply_hotkey(
                    hotkey_window,
                    APPEND_HOTKEY_ID,
                    &mut append_registered,
                    next,
                    "append",
                ),
                TrayCommand::SetPasteHotkey(next) => apply_hotkey(
                    hotkey_window,
                    PASTE_HOTKEY_ID,
                    &mut paste_registered,
                    next,
                    "paste",
                ),
                TrayCommand::Shutdown => {
                    apply_hotkey(hotkey_window, HOTKEY_ID, &mut registered, None, "record");
                    apply_hotkey(
                        hotkey_window,
                        APPEND_HOTKEY_ID,
                        &mut append_registered,
                        None,
                        "append",
                    );
                    apply_hotkey(
                        hotkey_window,
                        PASTE_HOTKEY_ID,
                        &mut paste_registered,
                        None,
                        "paste",
                    );
                    let _ = tray.set_visible(false);
                    return;
                }
            }
        }
        for (button, button_state) in poll_clicks() {
            if button == MouseButton::Left && button_state == MouseButtonState::Down {
                crate::log::log("tray left click");
                let _ = to_app.send(TrayAction::RecordToggle);
            }
        }
        for id in poll_menu_ids() {
            let action = if id == settings_id {
                Some(TrayAction::OpenSettings)
            } else if id == history_id {
                Some(TrayAction::OpenCapture)
            } else if id == copy_last_id {
                Some(TrayAction::CopyLast)
            } else if id == open_folder_id {
                Some(TrayAction::OpenModelsFolder)
            } else if id == quit_id {
                Some(TrayAction::Quit)
            } else {
                None
            };
            if let Some(action) = action {
                crate::log::log(&format!("tray sends action {action:?}"));
                let _ = to_app.send(action);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Drain the thread's message queue. Returns true when the global recording
/// hotkey fired during this pass.
fn pump_messages() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, WM_HOTKEY,
    };
    let mut hotkey_pressed = false;
    // SAFETY: this thread owns the hotkey and tray windows; PeekMessageW and
    // DispatchMessageW only dispatch this thread's messages, and MSG is
    // zero-initialized with valid alignment for the API.
    unsafe {
        let mut msg: MSG = core::mem::zeroed();
        while PeekMessageW(&mut msg, core::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            if msg.message == WM_HOTKEY && msg.wParam == HOTKEY_ID as usize {
                hotkey_pressed = true;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    hotkey_pressed
}

/// Drain all queued click events. Every event must be processed — a quick
/// left click queues both `Down` and `Up`, and keeping only the last one
/// would silently swallow the `Down` we act on.
fn poll_clicks() -> Vec<(MouseButton, MouseButtonState)> {
    let mut result = Vec::new();
    while let Ok(event) = TrayIconEvent::receiver().try_recv() {
        if let TrayIconEvent::Click {
            button,
            button_state,
            ..
        } = event
        {
            result.push((button, button_state));
        }
    }
    result
}

fn poll_menu_ids() -> Vec<MenuId> {
    let mut ids = Vec::new();
    while let Ok(event) = MenuEvent::receiver().try_recv() {
        ids.push(event.id.clone());
    }
    ids
}
