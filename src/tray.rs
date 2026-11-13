use crate::hotkey::Hotkey;
use crate::icons;
use crate::state::Status;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;
use crate::i18n::tr;
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
    OpenHistory,
    OpenCapture,
    /// Open the Telegram page of the external-access tab.
    OpenTelegram,
    /// Open the phone (monologs) page of the external-access tab.
    OpenMonologs,
    CopyLast,
    OpenModelsFolder,
    Quit,
}

pub enum TrayCommand {
    SetStatus(Status),
    /// A background task is running: the icon is violet and no sound is played.
    SetBackground(bool),
    /// Replace the tray tooltip with a one-off message.
    Notify(String),
    /// Apply a new global shortcut; `None` disables it.
    SetHotkey(Option<Hotkey>),
    /// Apply a new append shortcut; `None` disables it.
    SetAppendHotkey(Option<Hotkey>),
    /// Apply a new paste shortcut; `None` disables it.
    SetPasteHotkey(Option<Hotkey>),
    /// Swap the left and middle tray mouse actions.
    SetMouseSwap(bool),
    /// Which holiday microphone the idle tray icon shows; `None` for an ordinary
    /// day, or when holiday icons are turned off.
    SetHoliday(Option<String>),
    /// Enable the entries that need a running service: the bot needs a token,
    /// the monologs page needs the phone server.
    SetEntries { telegram: bool, monologs: bool },
    /// Rebuild the menu and tooltip after the interface language changed.
    SetLanguage,
    Shutdown,
}

pub fn start(
    to_app: Sender<TrayAction>,
    hotkey: Option<Hotkey>,
    append_hotkey: Option<Hotkey>,
    paste_hotkey: Option<Hotkey>,
    swap_mouse_buttons: bool,
) -> Sender<TrayCommand> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        crate::log::log("tray thread starting");
        run(
            to_app,
            rx,
            hotkey,
            append_hotkey,
            paste_hotkey,
            swap_mouse_buttons,
        );
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

/// Menu entries and their ids, rebuilt when the interface language changes.
struct MenuIds {
    settings: MenuId,
    history: MenuId,
    capture: MenuId,
    copy_last: MenuId,
    telegram: MenuId,
    monologs: MenuId,
    open_folder: MenuId,
    quit: MenuId,
}

/// The two entries whose availability follows a service are kept as handles so
/// their state can change without rebuilding the menu.
struct ServiceItems {
    telegram: MenuItem,
    monologs: MenuItem,
}

fn build_menu(telegram: bool, monologs: bool) -> (Menu, MenuIds, ServiceItems) {
    let menu = Menu::new();
    // Everything that was there stays, in this order: the last transcription
    // first, the everyday actions, then the two services, the folder, quit.
    let copy_last = MenuItem::new(tr("Copy last transcription"), true, None);
    let capture = MenuItem::new(tr("Capture"), true, None);
    let history = MenuItem::new(tr("History"), true, None);
    let settings = MenuItem::new(tr("Settings"), true, None);
    let telegram_item = MenuItem::new(tr("Telegram"), telegram, None);
    let monologs_item = MenuItem::new(tr("Monologs"), monologs, None);
    let open_folder = MenuItem::new(tr("Open models folder"), true, None);
    let separator = PredefinedMenuItem::separator();
    let quit = MenuItem::new(tr("Quit"), true, None);
    menu.append_items(&[
        &copy_last,
        &capture,
        &history,
        &settings,
        &telegram_item,
        &monologs_item,
        &open_folder,
        &separator,
        &quit,
    ])
    .expect("failed to build tray menu");
    let ids = MenuIds {
        settings: settings.id().clone(),
        history: history.id().clone(),
        capture: capture.id().clone(),
        copy_last: copy_last.id().clone(),
        telegram: telegram_item.id().clone(),
        monologs: monologs_item.id().clone(),
        open_folder: open_folder.id().clone(),
        quit: quit.id().clone(),
    };
    let items = ServiceItems {
        telegram: telegram_item,
        monologs: monologs_item,
    };
    (menu, ids, items)
}

/// Which icon a status asks for. Idle plus background work is a background task
/// transcribed; anything the user does outranks that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IconKind {
    Idle,
    Background,
    Recording,
    Transcribing,
    Recovering,
    File,
    Disabled,
}

fn icon_choice(status: Status, background: bool) -> IconKind {
    match status {
        Status::Idle if background => IconKind::Background,
        Status::Idle => IconKind::Idle,
        Status::Recording => IconKind::Recording,
        Status::Transcribing => IconKind::Transcribing,
        Status::Recovering => IconKind::Recovering,
        Status::File => IconKind::File,
        Status::NoModel => IconKind::Disabled,
    }
}

/// The colour of each status. A holiday changes the shape of the microphone, and
/// the status always owns the colour: a holiday in its own palette read as a
/// fourth status and made the icon hard to trust.
fn status_color(kind: IconKind) -> [u8; 3] {
    match kind {
        IconKind::Idle => [0xff, 0xff, 0xff],
        IconKind::Background => [0xb9, 0xa2, 0xf0],
        IconKind::Recording => [0xff, 0x5c, 0x5c],
        IconKind::Transcribing => [0xff, 0x9f, 0x43],
        IconKind::Recovering => [0x6e, 0x9b, 0xff],
        IconKind::File => [0x5c, 0xe0, 0x7a],
        IconKind::Disabled => [0x8a, 0x8a, 0x8f],
    }
}

/// The icon for a status: the holiday head when one is showing, the plain
/// microphone otherwise, always in the status colour. A missing model shows the
/// greyed-out icon — a decoration must not hide that recording is unavailable.
fn icon_for(kind: IconKind, holiday: Option<&str>) -> tray_icon::Icon {
    // Without a model the whole icon is grey: a coloured holiday head would read
    // as "ready", which is exactly the state that is unavailable.
    let holiday = if kind == IconKind::Disabled { None } else { holiday };
    icons::tray_icon(holiday, status_color(kind))
}

fn status_icon(status: Status, background: bool, holiday: Option<&str>) -> tray_icon::Icon {
    icon_for(icon_choice(status, background), holiday)
}

fn tooltip(swap_mouse_buttons: bool) -> String {
    if swap_mouse_buttons {
        tr("Voice Button - left click: record and paste, middle click: record")
    } else {
        tr("Voice Button - left click: record, middle click: record and paste")
    }
}

fn run(
    to_app: Sender<TrayAction>,
    rx_cmd: Receiver<TrayCommand>,
    hotkey: Option<Hotkey>,
    append_hotkey: Option<Hotkey>,
    paste_hotkey: Option<Hotkey>,
    mut swap_mouse_buttons: bool,
) {
    let mut telegram_enabled = false;
    let mut monologs_enabled = false;
    let (menu, mut ids, mut service_items) = build_menu(telegram_enabled, monologs_enabled);
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(tooltip(swap_mouse_buttons))
        .with_icon(icons::tray_icon(None, status_color(IconKind::Idle)))
        .build()
        .expect("failed to create tray icon");
    crate::log::log("tray icon created");
    let hotkey_window = create_hotkey_window();
    let mut registered: Option<Hotkey> = None;
    let mut append_registered: Option<Hotkey> = None;
    let mut paste_registered: Option<Hotkey> = None;
    // Blinking blue icon while a crash-interrupted recording is transcribed:
    // a static icon is easy to miss.
    let mut current_status = Status::Idle;
    let mut background = false;
    let mut holiday: Option<String> = None;
    let mut blink_at = std::time::Instant::now();
    let mut blink_bright = true;
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
        if current_status == Status::Recovering
            && blink_at.elapsed() >= std::time::Duration::from_millis(450)
        {
            blink_at = std::time::Instant::now();
            blink_bright = !blink_bright;
            // The dim frame is the same recovering blue, darkened.
            let color = if blink_bright {
                status_color(IconKind::Recovering)
            } else {
                [0x38, 0x50, 0x85]
            };
            let icon = icons::tray_icon(holiday.as_deref(), color);
            let _ = tray.set_icon(Some(icon));
        }
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
                    current_status = status;
                    blink_at = std::time::Instant::now();
                    blink_bright = true;
                    let icon = status_icon(status, background, holiday.as_deref());
                    let _ = tray.set_icon(Some(icon));
                    let hint = match status {
                        Status::Recovering => tr("Voice Button - recovering a recording"),
                        Status::File => tr("Voice Button - transcribing a file"),
                        _ => tooltip(swap_mouse_buttons),
                    };
                    let _ = tray.set_tooltip(Some(hint));
                }
                TrayCommand::SetBackground(active) => {
                    background = active;
                    let icon = status_icon(current_status, background, holiday.as_deref());
                    let _ = tray.set_icon(Some(icon));
                }
                TrayCommand::Notify(text) => {
                    let _ = tray.set_tooltip(Some(text));
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
                TrayCommand::SetMouseSwap(swap) => {
                    swap_mouse_buttons = swap;
                    let _ = tray.set_tooltip(Some(tooltip(swap)));
                    crate::log::log(&format!("tray mouse buttons swapped: {swap}"));
                }
                TrayCommand::SetHoliday(next) => {
                    holiday = next;
                    let icon = status_icon(current_status, background, holiday.as_deref());
                    let _ = tray.set_icon(Some(icon));
                }
                TrayCommand::SetEntries { telegram, monologs } => {
                    telegram_enabled = telegram;
                    monologs_enabled = monologs;
                    service_items.telegram.set_enabled(telegram);
                    service_items.monologs.set_enabled(monologs);
                }
                TrayCommand::SetLanguage => {
                    let (menu, next, items) = build_menu(telegram_enabled, monologs_enabled);
                    tray.set_menu(Some(Box::new(menu)));
                    ids = next;
                    service_items = items;
                    let _ = tray.set_tooltip(Some(tooltip(swap_mouse_buttons)));
                    crate::log::log("tray menu rebuilt for the new interface language");
                }
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
            if button_state != MouseButtonState::Down {
                continue;
            }
            let action = match button {
                MouseButton::Left => {
                    if swap_mouse_buttons {
                        crate::log::log("tray left click (swapped: record and paste)");
                        Some(TrayAction::RecordPaste)
                    } else {
                        crate::log::log("tray left click");
                        Some(TrayAction::RecordToggle)
                    }
                }
                MouseButton::Middle => {
                    // Middle click mirrors the paste hotkey: record a new
                    // message and insert it at the caret of the focused window.
                    if swap_mouse_buttons {
                        crate::log::log("tray middle click (swapped: record)");
                        Some(TrayAction::RecordToggle)
                    } else {
                        crate::log::log("tray middle click");
                        Some(TrayAction::RecordPaste)
                    }
                }
                MouseButton::Right => None,
            };
            if let Some(action) = action {
                let _ = to_app.send(action);
            }
        }
        for id in poll_menu_ids() {
            let action = if id == ids.copy_last {
                Some(TrayAction::CopyLast)
            } else if id == ids.capture {
                Some(TrayAction::OpenCapture)
            } else if id == ids.history {
                Some(TrayAction::OpenHistory)
            } else if id == ids.settings {
                Some(TrayAction::OpenSettings)
            } else if id == ids.telegram {
                Some(TrayAction::OpenTelegram)
            } else if id == ids.monologs {
                Some(TrayAction::OpenMonologs)
            } else if id == ids.open_folder {
                Some(TrayAction::OpenModelsFolder)
            } else if id == ids.quit {
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

/// Drain all queued click events. Every event must be processed вЂ” a quick
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_work_turns_the_icon_violet_only_when_nothing_else_happens() {
        assert_eq!(icon_choice(Status::Idle, false), IconKind::Idle);
        assert_eq!(icon_choice(Status::Idle, true), IconKind::Background);
    }

    #[test]
    fn a_user_dication_outranks_the_phone() {
        for status in [
            Status::Recording,
            Status::Transcribing,
            Status::Recovering,
            Status::File,
            Status::NoModel,
        ] {
            assert_ne!(
                icon_choice(status, true),
                IconKind::Background,
                "{status:?} must keep its own icon while the phone is transcribed"
            );
        }
    }
}

