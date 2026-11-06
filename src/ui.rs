use crate::autostart;
use crate::config::{self, AppDirs, Backend};
use crate::model_manager;
use crate::state::{self, Data, Shared};
use arboard;
use eframe::egui;
use egui::{ProgressBar, RichText, ScrollArea};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const WINDOW_TITLE: &str = "Voice Button";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Capture,
    Settings,
}

/// UI process entry point. Launched by the daemon as
/// `voice-button.exe --ui <settings|history> --parent <daemon pid>`.
pub fn run_from_args() {
    let args: Vec<String> = std::env::args().collect();
    let tab = args
        .windows(2)
        .find(|w| w[0] == "--ui")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "settings".to_string());
    let dirs = config::app_dirs();
    crate::log::init_child(&dirs.root);
    crate::log::log(&format!("ui process starting (tab={tab})"));
    let config = config::load_config(&dirs.root);
    let mut history = state::load_history(&dirs.root);
    if std::env::var("VB_SNAPSHOT").is_ok() {
        // Deterministic state for the UI snapshot run.
        let mut cfg = config;
        cfg.backend = Backend::Onnx;
        cfg.onnx_model = "gigaam-v3-sherpa-int8".to_string();
        cfg.language = "auto".to_string();
        history = state::History::from_entries(vec![
            state::HistoryEntry {
                at: "2026-11-05 09:14:32".to_string(),
                text: "Hi! Remind me to call the bank after lunch - I need to check the loan terms and which documents to bring to the meeting with the account manager on Friday, and also ask about early repayment and fees, if any.".to_string(),
                model: "v3_e2e_rnnt".to_string(),
            },
            state::HistoryEntry {
                at: "2026-11-05 11:02:17".to_string(),
                text: "Add milk, bread, eggs and coffee to the shopping list, and do not forget the cat food.".to_string(),
                model: "v3_e2e_ctc".to_string(),
            },
            state::HistoryEntry {
                at: "2026-11-04 18:45:03".to_string(),
                text: "Tomorrow at ten there is a team meeting; prepare the slides for the second section.".to_string(),
                model: "large-v3-turbo-q5_0".to_string(),
            },
            state::HistoryEntry {
                at: "2026-11-04 15:20:44".to_string(),
                text: "Translate the sentence into English: I would be glad to see you at the exhibition on Thursday.".to_string(),
                model: "multi-gigaam-ctc".to_string(),
            },
        ]);
        // Simulate a UI process opened while the daemon is already recording.
        // The snapshot must come through the real initial file-poller read.
        crate::ipc::write_status(&dirs.root, crate::state::Status::Recording);
        crate::ipc::write_live(
            &dirs.root,
            &crate::ipc::LiveView {
                text: "Book a table for two at the Italian place on Saturday evening.".to_string(),
                started: chrono::Local::now().timestamp_millis() - 42_000,
            },
        );
        run_ui(cfg, history, dirs, "capture".to_string());
        return;
    }
    run_ui(config, history, dirs, tab);
}

fn run_ui(config: config::Config, history: state::History, dirs: AppDirs, tab: String) {
    let shared = Arc::new(Shared::new(config, history));
    if let Some(pid) = current_parent_pid() {
        spawn_parent_watcher(pid);
    }
    spawn_file_poller(shared.clone(), dirs.clone());
    let initial_tab = if tab == "settings" { Tab::Settings } else { Tab::Capture };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(WINDOW_TITLE)
            .with_inner_size([560.0, 620.0])
            .with_icon(crate::icons::window_icon()),
        ..Default::default()
    };
    options.viewport = options
        .viewport
        .with_active(true);
    let app = MainWindow {
        shared: shared.clone(),
        dirs: dirs.clone(),
        tab: initial_tab,
        show_about: false,
        expanded_history: None,
        download_done_seen: false,
        snapshot_dir: std::env::var("VB_SNAPSHOT").ok().map(PathBuf::from),
        snapshot_frame: 0,
    };
    let creator: Box<dyn FnOnce(&eframe::CreationContext<'_>) -> Box<dyn eframe::App>> =
        Box::new(move |_cc| Box::new(app));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native("voice-button", options, creator)
    }));
    match result {
        Ok(Err(e)) => crate::log::log(&format!("ui run_native error: {e}")),
        Ok(Ok(())) => crate::log::log("ui window closed normally"),
        Err(_) => crate::log::log("ui PANICKED"),
    }
    // Never leave the daemon hotkey released after the window is gone.
    crate::ipc::set_hotkey_capture(&dirs.root, false);
}

fn current_parent_pid() -> Option<u32> {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|w| w[0] == "--parent")
        .and_then(|w| w[1].parse::<u32>().ok())
}

/// Exit the UI process when the daemon is gone (no orphan windows).
fn spawn_parent_watcher(pid: u32) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: standard Win32 process-handle queries; no memory involved.
        let dead = unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            };
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                true
            } else {
                let mut code: u32 = 0;
                let ok = GetExitCodeProcess(handle, &mut code);
                CloseHandle(handle);
                ok == 0 || code != 259 // STILL_ACTIVE
            }
        };
        if dead {
            crate::log::log("daemon gone, exiting UI");
            std::process::exit(0);
        }
    });
}

/// Comparable key for change detection (Metadata has no PartialEq).
fn meta_key(m: &Option<std::fs::Metadata>) -> Option<(u64, u32)> {
    m.as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs(), d.subsec_nanos()))
}

fn sync_config_from_disk(data: &mut Data, cfg: config::Config) {
    let keep_pending_selection = selection_pending(data);
    let keep_pending_language = data.config.language != data.applied_language;
    let draft = data.config.clone();

    data.applied_backend = cfg.backend;
    data.applied_whisper_model = cfg.whisper_model.clone();
    data.applied_whisper_file = cfg.whisper_file.clone();
    data.applied_onnx_model = cfg.onnx_model.clone();
    data.applied_onnx_folder = cfg.onnx_folder.clone();
    data.applied_language = cfg.language.clone();
    data.config = cfg;

    if keep_pending_selection {
        data.config.backend = draft.backend;
        data.config.whisper_model = draft.whisper_model;
        data.config.whisper_file = draft.whisper_file;
        data.config.onnx_model = draft.onnx_model;
        data.config.onnx_folder = draft.onnx_folder;
    }
    if keep_pending_language {
        data.config.language = draft.language;
    }
}

/// Keep the in-memory state in sync with files written by the daemon
/// (config is also written by us; history and download progress come from
/// the daemon).
fn spawn_file_poller(shared: Arc<Shared>, dirs: AppDirs) {
    std::thread::spawn(move || {
        let config_path = dirs.root.join("config.json");
        let history_path = crate::history::history_path(&dirs.root);
        let download_path = dirs.root.join("download.state.json");
        let status_path = dirs.root.join("status.txt");
        let live_path = dirs.root.join("live.json");
        let mut config_meta = meta_key(&std::fs::metadata(&config_path).ok());
        let mut history_meta = meta_key(&std::fs::metadata(&history_path).ok());
        // None forces an initial read when a file already exists. This is
        // essential when the UI is opened after recording/download has begun.
        let mut download_meta = None;
        let mut status_meta = None;
        let mut live_meta = None;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let m = meta_key(&std::fs::metadata(&config_path).ok());
            if m != config_meta {
                config_meta = m;
                let cfg = config::load_config(&dirs.root);
                sync_config_from_disk(&mut shared.lock(), cfg);
            }
            let m = meta_key(&std::fs::metadata(&history_path).ok());
            if m != history_meta {
                history_meta = m;
                let history = state::load_history(&dirs.root);
                shared.lock().history = history;
            }
            let m = meta_key(&std::fs::metadata(&download_path).ok());
            if m != download_meta {
                download_meta = m;
                let download = crate::ipc::read_download_state(&dirs.root).map(|s| {
                    state::DownloadProgress {
                        model: s.model,
                        file: s.file,
                        files: s.files,
                        downloaded: s.downloaded,
                        total: s.total,
                        done_bytes: s.done_bytes,
                        done_total: s.done_total,
                        done: s.done,
                        error: s.error,
                        // Cancellation is file-based in the UI process.
                        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    }
                });
                shared.lock().download = download;
            }
            let m = meta_key(&std::fs::metadata(&status_path).ok());
            if m != status_meta {
                status_meta = m;
                if let Some(status) = crate::ipc::read_status(&dirs.root) {
                    shared.lock().status = status;
                }
            }
            let m = meta_key(&std::fs::metadata(&live_path).ok());
            if m != live_meta {
                live_meta = m;
                match crate::ipc::read_live(&dirs.root) {
                    Some(live) => {
                        let mut data = shared.lock();
                        data.live_text = live.text;
                        data.live_started_ms = live.started;
                    }
                    None => {
                        let mut data = shared.lock();
                        data.live_text.clear();
                        data.live_started_ms = 0;
                    }
                }
            }
        }
    });
}

pub struct MainWindow {
    shared: Arc<Shared>,
    dirs: AppDirs,
    tab: Tab,
    show_about: bool,
    expanded_history: Option<usize>,
    download_done_seen: bool,
    snapshot_dir: Option<PathBuf>,
    snapshot_frame: u32,
}

impl eframe::App for MainWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the UI fresh (e.g. download progress) between events.
        ctx.request_repaint_after(Duration::from_millis(250));

        let mut data = self.shared.lock();
        egui::CentralPanel::default().show(ctx, |ui| {
            show_download_banner(ui, &mut data, &self.dirs);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Capture, "Capture");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("About").clicked() {
                        self.show_about = true;
                    }
                });
            });
            ui.separator();
            match self.tab {
                Tab::Capture => capture_body(ui, &mut data, &self.dirs, &mut self.expanded_history),
                Tab::Settings => settings_body(ui, &mut data, &self.dirs),
            }
        });
        // A finished download may make a pending model selection applicable.
        let download_done = data.download.as_ref().map(|d| d.done).unwrap_or(false);
        if download_done && !self.download_done_seen {
            self.download_done_seen = true;
            apply_settings(&mut data, &self.dirs);
        }
        if !download_done {
            self.download_done_seen = false;
        }
        drop(data);

        if self.show_about {
            show_about_modal(ctx, &mut self.show_about, &self.dirs);
        }

        self.drive_snapshot();
    }
}

impl MainWindow {
    /// Snapshot test mode (VB_SNAPSHOT=<dir>): walk through the main UI
    /// states and capture the window into PNG files, then exit.
    fn drive_snapshot(&mut self) {
        let Some(dir) = self.snapshot_dir.clone() else {
            return;
        };
        self.snapshot_frame += 1;
        let frame = self.snapshot_frame;
        match frame {
            6 => self.capture(&dir.join("capture-recording.png")),
            8 => {
                let mut data = self.shared.lock();
                data.status = crate::state::Status::Idle;
                data.live_text.clear();
                data.live_started_ms = 0;
                self.tab = Tab::Settings;
            }
            10 => self.capture(&dir.join("settings.png")),
            12 => self.tab = Tab::Capture,
            18 => self.capture(&dir.join("history.png")),
            19 => self.expanded_history = Some(0),
            23 => self.capture(&dir.join("history-expanded.png")),
            25 => {
                self.expanded_history = None;
                self.tab = Tab::Settings;
                self.show_about = true;
            }
            31 => self.capture(&dir.join("about.png")),
            33 => {
                self.show_about = false;
                let mut data = self.shared.lock();
                data.download = Some(state::DownloadProgress {
                    model: "v3_ctc".to_string(),
                    file: 1,
                    files: 2,
                    downloaded: 63_443_213,
                    total: Some(224_893_347),
                    done_bytes: 73_105,
                    done_total: 73_105,
                    done: false,
                    error: None,
                    cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                });
            }
            39 => self.capture(&dir.join("download-progress.png")),
            41 => std::process::exit(0),
            _ => {}
        }
    }

    fn capture(&self, path: &std::path::Path) {
        let hwnd = find_own_window();
        if hwnd != 0 {
            capture_window(hwnd, path);
        }
    }
}

/// Locate our own window by title and verify it belongs to this process.
fn find_own_window() -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};
    let mut title: Vec<u16> = WINDOW_TITLE.encode_utf16().collect();
    title.push(0);
    // SAFETY: title is a valid NUL-terminated UTF-16 string.
    let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
    if hwnd.is_null() {
        return 0;
    }
    let mut pid: u32 = 0;
    // SAFETY: hwnd is valid; pid is a writable out-pointer.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    if pid == std::process::id() {
        hwnd as isize
    } else {
        0
    }
}

/// Capture a window (by HWND) into a PNG file using PrintWindow with
/// PW_RENDERFULLCONTENT so accelerated (glow) windows render correctly.
fn capture_window(hwnd: isize, path: &std::path::Path) {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, HGDIOBJ,
    };
    use windows_sys::Win32::Storage::Xps::PrintWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

    if hwnd == 0 {
        return;
    }
    // SAFETY: all GDI objects created here are deleted before returning; hwnd
    // is our own window handle; the DIB bits pointer stays valid while bmp lives.
    unsafe {
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return;
        }
        let w = (rect.right - rect.left).max(1) as u32;
        let h = (rect.bottom - rect.top).max(1) as u32;
        let screen_dc = GetDC(std::ptr::null_mut());
        if screen_dc.is_null() {
            return;
        }
        let mem_dc = CreateCompatibleDC(screen_dc);
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32), // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(mem_dc, &bmi, 0, &mut bits, std::ptr::null_mut(), 0);
        if bmp.is_null() {
            DeleteDC(mem_dc);
            ReleaseDC(std::ptr::null_mut(), screen_dc);
            return;
        }
        let old = SelectObject(mem_dc, bmp as HGDIOBJ);
        PrintWindow(hwnd, mem_dc, 2);
        let n = (w * h * 4) as usize;
        let src = std::slice::from_raw_parts(bits as *const u8, n);
        let mut rgba = vec![0u8; n];
        for (dst, px) in rgba.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            // DIB is BGRA; image::save_buffer wants RGBA.
            dst[0] = px[2];
            dst[1] = px[1];
            dst[2] = px[0];
            dst[3] = 255;
        }
        let _ = image::save_buffer(path, &rgba, w, h, image::ColorType::Rgba8);
        SelectObject(mem_dc, old);
        DeleteObject(bmp as HGDIOBJ);
        DeleteDC(mem_dc);
        ReleaseDC(std::ptr::null_mut(), screen_dc);
    }
}

/// Progress indicator for the currently running download, shown at the top of
/// the window regardless of which model section is open.
fn show_download_banner(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs) {
    let Some(dl) = data.download.as_ref() else {
        return;
    };
    if dl.done || dl.error.is_some() {
        return;
    }
    let overall_done = dl.done_bytes + dl.downloaded;
    let overall_total = dl.done_total + dl.total.unwrap_or(dl.downloaded);
    let fraction = if overall_total > 0 {
        (overall_done as f32 / overall_total as f32).min(1.0)
    } else {
        0.0
    };
    ui.horizontal(|ui| {
        ui.add(egui::ProgressBar::new(fraction).show_percentage().desired_width(220.0));
        ui.label(format!(
            "{} — file {}/{}: {}{}",
            dl.model,
            dl.file + 1,
            dl.files,
            bytes_to_mb(dl.downloaded),
            dl.total.map(|t| format!(" / {}", bytes_to_mb(t))).unwrap_or_default(),
        ));
        if ui.button("Cancel").clicked() {
            // Cancellation crosses process borders via a flag file.
            crate::ipc::request_cancel(&dirs.root);
        }
    });
    ui.separator();
}

/// Whether the currently selected model can actually be used right now
/// (downloaded, or a custom file/folder that exists).
fn model_selection_ready(data: &Data, dirs: &AppDirs) -> bool {
    model_manager::selection_ready(&dirs.models, &data.config)
}

fn selection_pending(data: &Data) -> bool {
    data.config.backend != data.applied_backend
        || data.config.whisper_model != data.applied_whisper_model
        || data.config.whisper_file != data.applied_whisper_file
        || data.config.onnx_model != data.applied_onnx_model
        || data.config.onnx_folder != data.applied_onnx_folder
}

fn language_ready(data: &Data) -> bool {
    if data.config.language != "auto" {
        return true;
    }
    let (format, model_id) = match data.config.backend {
        Backend::Whisper => (Backend::Whisper, data.config.whisper_model.as_str()),
        Backend::Onnx => (Backend::Onnx, data.config.onnx_model.as_str()),
    };
    model_manager::supports_auto(format, model_id)
}

/// Persist settings. Model selections that are not usable yet (e.g. still
/// downloading) and a language the current model cannot honor stay pending in
/// the UI and are kept out of the saved config until they become applicable.
fn apply_settings(data: &mut Data, dirs: &AppDirs) {
    let mut cfg = data.config.clone();
    if model_selection_ready(data, dirs) {
        data.applied_backend = data.config.backend;
        data.applied_whisper_model = data.config.whisper_model.clone();
        data.applied_whisper_file = data.config.whisper_file.clone();
        data.applied_onnx_model = data.config.onnx_model.clone();
        data.applied_onnx_folder = data.config.onnx_folder.clone();
    } else {
        cfg.backend = data.applied_backend;
        cfg.whisper_model = data.applied_whisper_model.clone();
        cfg.whisper_file = data.applied_whisper_file.clone();
        cfg.onnx_model = data.applied_onnx_model.clone();
        cfg.onnx_folder = data.applied_onnx_folder.clone();
    }
    if language_ready(data) {
        data.applied_language = data.config.language.clone();
    } else {
        cfg.language = data.applied_language.clone();
    }
    match config::save_config(&dirs.root, &cfg) {
        Ok(()) => data.last_error = None,
        Err(e) => {
            crate::log::log(&format!("failed to save config: {e:#}"));
            data.last_error = Some(format!("{e:#}"));
        }
    }
}

fn settings_body(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs) {
    ui.heading("Model format");
    let model_switch_locked = matches!(
        data.status,
        crate::state::Status::Recording | crate::state::Status::Transcribing
    );
    let mut format_changed = false;
    ui.add_enabled_ui(!model_switch_locked, |ui| {
        ui.horizontal(|ui| {
            format_changed |= ui
                .radio_value(
                    &mut data.config.backend,
                    Backend::Whisper,
                    "Whisper (whisper.cpp)",
                )
                .changed();
            format_changed |= ui
                .radio_value(&mut data.config.backend, Backend::Onnx, "ONNX (ONNX Runtime)")
                .changed();
        });
    });
    if model_switch_locked {
        ui.label(
            RichText::new("Model and runtime switching is locked while recording or transcribing.")
                .weak(),
        );
    }
    if format_changed {
        apply_settings(data, dirs);
    }
    ui.separator();
    match data.config.backend {
        Backend::Whisper => whisper_section(ui, data, dirs, model_switch_locked),
        Backend::Onnx => onnx_section(ui, data, dirs, model_switch_locked),
    }
    if selection_pending(data) {
        ui.label(
            RichText::new("The selected model is not downloaded yet — the selection applies once it is ready.")
                .color(egui::Color32::from_rgb(230, 170, 60)),
        );
    }
    ui.separator();
    if ui.button("Open models folder").clicked() {
        let _ = std::process::Command::new("explorer").arg(&dirs.models).spawn();
    }
    ui.separator();
    ui.heading("Sounds");
    let mut sounds_changed = false;
    let mut volume_pct = (data.config.sound_volume * 100.0).round().clamp(0.0, 100.0);
    if ui
        .add(egui::Slider::new(&mut volume_pct, 0.0..=100.0).text("%").fixed_decimals(0))
        .changed()
    {
        data.config.sound_volume = (volume_pct / 100.0).clamp(0.0, 1.0);
        sounds_changed = true;
    }
    sounds_changed |= ui
        .checkbox(
            &mut data.config.play_start_sound,
            "Play sound when recording starts",
        )
        .changed();
    sounds_changed |= ui
        .checkbox(
            &mut data.config.play_end_sound,
            "Play sound when recording ends",
        )
        .changed();
    if sounds_changed {
        apply_settings(data, dirs);
    }
    ui.separator();
    ui.heading("Startup");
    if ui
        .checkbox(&mut data.config.autostart, "Run at Windows startup")
        .changed()
    {
        apply_autostart(data);
        apply_settings(data, dirs);
    }
    if ui
        .checkbox(
            &mut data.config.silent_start,
            "Start silently (no windows when launched at startup)",
        )
        .changed()
    {
        apply_autostart(data);
        apply_settings(data, dirs);
    }
    ui.separator();
    ui.heading("Global hotkey");
    ui.horizontal(|ui| {
        if data.config.hotkey.is_empty() {
            ui.monospace("disabled");
        } else {
            ui.monospace(data.config.hotkey.clone());
        }
        if data.capturing_hotkey {
            ui.label(RichText::new("press Ctrl/Alt/Shift + key (or an F-key), Esc cancels").color(egui::Color32::from_rgb(230, 170, 60)));
        } else if ui.button("Record shortcut").clicked() {
            data.capturing_hotkey = true;
            // Release the global hotkey while capturing, otherwise the very
            // combination being typed would toggle recording instead of
            // reaching this window.
            crate::ipc::set_hotkey_capture(&dirs.root, true);
            let _ = std::fs::remove_file(crate::ipc::hotkey_result_path(&dirs.root));
            crate::log::log("hotkey capture started");
        }
        if ui.button("Clear").clicked() {
            data.config.hotkey.clear();
            apply_settings(data, dirs);
            // Clearing means "pick a new one": start listening right away.
            data.capturing_hotkey = true;
            crate::ipc::set_hotkey_capture(&dirs.root, true);
            let _ = std::fs::remove_file(crate::ipc::hotkey_result_path(&dirs.root));
            crate::log::log("hotkey cleared, capture started");
        }
    });
    if data.capturing_hotkey {
        // The daemon watches the physical keyboard (the window may not hold
        // focus) and reports the captured combination through this file.
        if let Some(text) = crate::ipc::take_hotkey_result(&dirs.root) {
            data.capturing_hotkey = false;
            crate::ipc::set_hotkey_capture(&dirs.root, false);
            if text != "cancel" {
                if let Some(hotkey) = crate::hotkey::parse(&text) {
                    data.config.hotkey = hotkey.label();
                    crate::log::log(&format!("hotkey captured: {}", data.config.hotkey));
                    apply_settings(data, dirs);
                }
            }
        }
    }
    if data.capturing_hotkey {
        let captured = ui.input(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                {
                    crate::log::log(&format!(
                        "hotkey capture saw key {key:?} ctrl={} alt={} shift={}",
                        modifiers.ctrl, modifiers.alt, modifiers.shift
                    ));
                    if *key == egui::Key::Escape {
                        return Some(None);
                    }
                    // A global shortcut needs a modifier (or an F-key); otherwise
                    // a stray Tab or letter would be captured and then stolen from
                    // every application.
                    let has_modifier = modifiers.ctrl || modifiers.alt || modifiers.shift;
                    if let Some(vk) = crate::hotkey::vk_from_egui(*key) {
                        if has_modifier || (0x70..=0x87).contains(&vk) {
                            return Some(Some(crate::hotkey::Hotkey {
                                ctrl: modifiers.ctrl,
                                alt: modifiers.alt,
                                shift: modifiers.shift,
                                win: false,
                                vk,
                            }));
                        }
                    }
                }
            }
            None
        });
        if let Some(captured) = captured {
            data.capturing_hotkey = false;
            crate::ipc::set_hotkey_capture(&dirs.root, false);
            if let Some(hotkey) = captured {
                data.config.hotkey = hotkey.label();
                crate::log::log(&format!("hotkey captured: {}", data.config.hotkey));
                apply_settings(data, dirs);
            }
        }
    }
    ui.separator();
    let (format, model_id) = match data.config.backend {
        Backend::Whisper => (Backend::Whisper, data.config.whisper_model.as_str()),
        Backend::Onnx => (Backend::Onnx, data.config.onnx_model.as_str()),
    };
    let supports_auto = model_manager::supports_auto(format, model_id);
    let mut language_changed = false;
    ui.horizontal(|ui| {
        ui.label("Language code:");
        egui::ComboBox::from_id_source("language_code")
            .selected_text(data.config.language.clone())
            .show_ui(ui, |ui| {
                if supports_auto {
                    language_changed |= ui
                        .selectable_value(
                            &mut data.config.language,
                            "auto".to_string(),
                            "auto (detect automatically)",
                        )
                        .changed();
                }
                for code in [
                    "ru", "en", "de", "fr", "es", "pt", "it", "nl", "pl", "tr", "zh", "ja", "ko",
                ] {
                    language_changed |= ui
                        .selectable_value(&mut data.config.language, code.to_string(), code)
                        .changed();
                }
            });
    });
    if language_changed {
        apply_settings(data, dirs);
    }
    if data.config.language == "auto" && !supports_auto {
        ui.label(
            RichText::new(
                "The current model cannot detect the language automatically — pick a language code.",
            )
            .color(egui::Color32::from_rgb(230, 170, 60)),
        );
    }
    if let Some(err) = &data.last_error {
        ui.label(
            RichText::new(format!("Error: {err}"))
                .color(egui::Color32::from_rgb(230, 90, 90)),
        );
    }
}

fn download_section(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, model_id: &str) {    let active = data
        .download
        .as_ref()
        .map(|d| d.model == model_id && d.error.is_none() && !d.done)
        .unwrap_or(false);
    if let Some(dl) = &data.download {
        if dl.model == model_id && dl.error.is_none() && !dl.done {
            let overall_done = dl.done_bytes + dl.downloaded;
            let overall_total = dl.done_total + dl.total.unwrap_or(dl.downloaded);
            let fraction = if overall_total > 0 {
                (overall_done as f32 / overall_total as f32).min(1.0)
            } else {
                0.0
            };
            ui.add(ProgressBar::new(fraction).show_percentage());
            ui.label(format!(
                "File {}/{}: {}{}",
                dl.file + 1,
                dl.files,
                bytes_to_mb(dl.downloaded),
                dl.total
                    .map(|t| format!(" / {}", bytes_to_mb(t)))
                    .unwrap_or_default()
            ));
            if ui.button("Cancel").clicked() {
                crate::ipc::request_cancel(&dirs.root);
            }
        } else if let Some(err) = &dl.error {
            if dl.model == model_id {
                ui.label(
                    RichText::new(format!("Download error: {err}"))
                        .color(egui::Color32::from_rgb(230, 90, 90)),
                );
            }
        } else if dl.done && dl.model == model_id {
            ui.label("Download complete.");
        }
    }
    let already_downloaded = model_manager::is_ready(&dirs.models, model_id);
    if !active && !already_downloaded && ui.button(format!("Download {model_id}")).clicked() {
        let busy = data
            .download
            .as_ref()
            .map(|d| !d.done && d.error.is_none())
            .unwrap_or(false);
        if !busy {
            // The daemon owns the download; ask it via the request file.
            crate::ipc::write_download_request(&dirs.root, model_id);
        }
    }
}

fn whisper_section(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, locked: bool) {
    let custom = data.config.whisper_model == "custom";
    let mut changed = false;
    ui.add_enabled_ui(!locked, |ui| {
        egui::ComboBox::from_label("Whisper model")
            .selected_text(if custom {
                "Custom file...".to_string()
            } else {
                let marker =
                    if model_manager::is_ready(&dirs.models, &data.config.whisper_model) { "" } else { " (not downloaded)" };
                let size = model_manager::find_model(&data.config.whisper_model)
                    .map(|m| m.size)
                    .unwrap_or("");
                format!("{} ({size}){marker}", data.config.whisper_model)
            })
            .show_ui(ui, |ui| {
                for model in model_manager::MODELS {
                    if model.format != Backend::Whisper {
                        continue;
                    }
                    let marker = if model_manager::is_ready(&dirs.models, model.id) { "" } else { " (not downloaded)" };
                    changed |= ui
                        .selectable_value(
                            &mut data.config.whisper_model,
                            model.id.to_string(),
                            format!("{} ({}){marker}", model.id, model.size),
                        )
                        .changed();
                }
                changed |= ui
                    .selectable_value(&mut data.config.whisper_model, "custom".to_string(), "Custom file...")
                    .changed();
            });
    });
    if changed {
        apply_settings(data, dirs);
    }
    if !custom {
        let id = data.config.whisper_model.clone();
        if model_manager::is_ready(&dirs.models, &id) {
            ui.label(RichText::new("Model is ready.").color(egui::Color32::from_rgb(90, 200, 120)));
        }
        download_section(ui, data, dirs, &id);
    } else {
        match data.config.whisper_file.clone() {
            Some(file) => {
                ui.monospace(&file);
                if !PathBuf::from(&file).exists() {
                    ui.label(
                        RichText::new("File does not exist.")
                            .color(egui::Color32::from_rgb(230, 170, 60)),
                    );
                }
            }
            None => {
                ui.label("No file selected.");
            }
        }
        if ui.button("Choose .ggml model file...").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("whisper.cpp model", &["bin", "ggml"])
                .pick_file()
            {
                data.config.whisper_file = Some(path.to_string_lossy().to_string());
                apply_settings(data, dirs);
            }
        }
    }
}

fn onnx_section(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, locked: bool) {
    let custom = data.config.onnx_model == "custom";
    let mut changed = false;
    ui.add_enabled_ui(!locked, |ui| {
        egui::ComboBox::from_label("ONNX model")
            .selected_text(if custom {
                "Custom folder...".to_string()
            } else {
                let marker =
                    if model_manager::is_ready(&dirs.models, &data.config.onnx_model) { "" } else { " (not downloaded)" };
                let size = model_manager::find_model(&data.config.onnx_model)
                    .map(|m| m.size)
                    .unwrap_or("");
                format!("{} ({size}){marker}", data.config.onnx_model)
            })
            .show_ui(ui, |ui| {
                for model in model_manager::MODELS {
                    if model.format != Backend::Onnx {
                        continue;
                    }
                    let marker = if model_manager::is_ready(&dirs.models, model.id) { "" } else { " (not downloaded)" };
                    changed |= ui
                        .selectable_value(
                            &mut data.config.onnx_model,
                            model.id.to_string(),
                            format!("{} ({}){marker}", model.id, model.size),
                        )
                        .changed();
                }
                changed |= ui
                    .selectable_value(&mut data.config.onnx_model, "custom".to_string(), "Custom folder...")
                    .changed();
            });
    });
    if changed {
        apply_settings(data, dirs);
    }
    if !custom {
        let id = data.config.onnx_model.clone();
        if model_manager::is_ready(&dirs.models, &id) {
            ui.label(RichText::new("Model is ready.").color(egui::Color32::from_rgb(90, 200, 120)));
        }
        download_section(ui, data, dirs, &id);
    } else {
        match data.config.onnx_folder.clone() {
            Some(folder) => {
                ui.monospace(&folder);
                let path = PathBuf::from(&folder);
                let family = model_manager::detect_family(&path);
                let model_ok = model_manager::onnx_folder_ready(&path, family);
                let vocab_ok =
                    path.join("vocab.txt").exists() || path.join("vocab.json").exists();
                let graph_ok = path.join("model.onnx").exists()
                    || (path.join("encoder.onnx").exists()
                        && path.join("decoder.onnx").exists()
                        && path.join("joint.onnx").exists());
                if model_ok {
                    ui.label(RichText::new("Model files found.").color(egui::Color32::from_rgb(90, 200, 120)));
                } else {
                    let missing = [
                        (!vocab_ok).then_some("vocab.txt (or vocab.json)"),
                        (!graph_ok).then_some("model.onnx (CTC) or encoder.onnx + decoder.onnx + joint.onnx (RNN-T)"),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                    ui.label(
                        RichText::new(format!("Missing files: {missing}"))
                            .color(egui::Color32::from_rgb(230, 170, 60)),
                    );
                }
            }
            None => {
                ui.label("No folder selected.");
            }
        }
        if ui.button("Choose folder...").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                data.config.onnx_folder = Some(path.to_string_lossy().to_string());
                apply_settings(data, dirs);
            }
        }
    }
}

fn capture_body(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, expanded: &mut Option<usize>) {
    let status = data.status;
    match status {
        crate::state::Status::Recording => {
            if ui
                .button(RichText::new("Stop recording").color(egui::Color32::from_rgb(200, 60, 60)))
                .clicked()
            {
                crate::ipc::write_record_request(&dirs.root);
            }
            if data.live_started_ms > 0 {
                let elapsed = chrono::Local::now().timestamp_millis() - data.live_started_ms;
                let secs = (elapsed / 1000).max(0);
                ui.label(
                    RichText::new(format!("REC {secs}s"))
                        .color(egui::Color32::from_rgb(220, 60, 60)),
                );
            } else {
                ui.label(
                    RichText::new("REC").color(egui::Color32::from_rgb(220, 60, 60)),
                );
            }
            ui.add_space(4.0);
            // A dedicated read-only field makes the live state visible even
            // before the first words arrive. Its width is bounded, so egui
            // wraps the transcript instead of growing one endless line.
            egui::Frame::group(ui.style()).show(ui, |ui| {
                let size = egui::vec2(ui.available_width(), 140.0);
                ui.allocate_ui(size, |ui| {
                    egui::ScrollArea::vertical()
                        .id_source("live_transcript")
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(if data.live_text.is_empty() {
                                    "Listening…"
                                } else {
                                    &data.live_text
                                })
                                .wrap(true)
                                .selectable(false),
                            );
                        });
                });
            });
        }
        crate::state::Status::Transcribing => {
            ui.label(
                RichText::new("Transcribing…")
                    .color(egui::Color32::from_rgb(230, 150, 40)),
            );
        }
        _ => {
            let download_busy = data
                .download
                .as_ref()
                .map(|d| !d.done && d.error.is_none())
                .unwrap_or(false);
            let can_record =
                status != crate::state::Status::NoModel && !download_busy;
            if !can_record {
                ui.label(
                    RichText::new("Recording is unavailable until a model is ready.")
                        .weak(),
                );
            }
            if ui
                .add_enabled(can_record, egui::Button::new("Start recording"))
                .clicked()
            {
                crate::ipc::write_record_request(&dirs.root);
            }
        }
    }
    ui.separator();
    history_body(ui, data, expanded);
}

fn history_row_offsets(
    ui: &mut egui::Ui,
    history: &state::History,
    expanded: Option<usize>,
) -> Vec<f32> {
    let spacing = ui.spacing().item_spacing.y;
    let base_height = ui.spacing().interact_size.y + 2.0 * spacing + 2.0;
    let wrap_width = ui.available_width().max(32.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let mut offsets = Vec::with_capacity(history.len() + 1);
    offsets.push(0.0);
    for index in 0..history.len() {
        let mut height = base_height;
        if expanded == Some(index) {
            if let Ok(entry) = history.entry(index) {
                let text_height = ui.fonts(|f| {
                    f.layout(entry.text, font.clone(), color, wrap_width)
                        .size()
                        .y
                });
                height += text_height + spacing + 4.0;
            }
        }
        offsets.push(offsets[index] + height);
    }
    offsets
}

fn visible_history_range(
    offsets: &[f32],
    viewport_min: f32,
    viewport_max: f32,
) -> std::ops::Range<usize> {
    let rows = offsets.len().saturating_sub(1);
    if rows == 0 {
        return 0..0;
    }
    let first_visible = offsets.partition_point(|&end| end < viewport_min).min(rows - 1);
    let first = first_visible.saturating_sub(1);
    let mut last = first + 1;
    while last < rows && offsets[last] <= viewport_max {
        last += 1;
    }
    first..(last + 1).min(rows)
}

fn history_body(ui: &mut egui::Ui, data: &mut Data, expanded: &mut Option<usize>) {
    ui.heading("History");
    ui.separator();
    if data.history.is_empty() {
        ui.label("No transcriptions yet.");
        return;
    }

    let mut copied: Option<String> = None;
    let mut toggle: Option<usize> = None;
    let offsets = history_row_offsets(ui, &data.history, *expanded);
    let total_height = offsets.last().copied().unwrap_or(0.0);
    ScrollArea::vertical()
        .id_source("history_rows")
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            ui.set_height(total_height);
            let range = visible_history_range(&offsets, viewport.min.y, viewport.max.y);
            let y_min = ui.max_rect().top() + offsets[range.start];
            let y_max = ui.max_rect().top() + offsets[range.end];
            let rect = egui::Rect::from_x_y_ranges(ui.max_rect().x_range(), y_min..=y_max);
            ui.allocate_ui_at_rect(rect, |ui| {
                for index in range {
                    let Some(row) = data.history.row(index).cloned() else {
                        continue;
                    };
                    let is_open = *expanded == Some(index);
                    let full_text = if is_open {
                        data.history.entry(index).ok().map(|entry| entry.text)
                    } else {
                        None
                    };
                    let height = offsets[index + 1] - offsets[index];
                    let width = ui.available_width();
                    ui.push_id(index, |ui| {
                        ui.allocate_ui(egui::vec2(width, height), |ui| {
                            ui.horizontal(|ui| {
                                if ui.small_button("Copy").clicked() {
                                    match data.history.entry(index) {
                                        Ok(entry) => copied = Some(entry.text),
                                        Err(e) => crate::log::log(&format!(
                                            "failed to read history entry {index}: {e:#}"
                                        )),
                                    }
                                }
                                ui.weak(format!("{} · {}", row.at(), row.model()));
                                if ui.selectable_label(is_open, row.preview()).clicked() {
                                    toggle = Some(index);
                                }
                            });
                            if let Some(text) = full_text {
                                ui.add(egui::Label::new(text).wrap(true));
                                ui.add_space(4.0);
                            }
                            ui.separator();
                        });
                    });
                }
            });
        });
    if let Some(index) = toggle {
        *expanded = if *expanded == Some(index) { None } else { Some(index) };
    }
    if let Some(text) = copied {
        copy_to_clipboard(&text);
    }
}

const ABOUT_LINES: &[&str] = &[
    "Offline voice-to-text tray app.",
    "Records the microphone on tray click,",
    "transcribes locally and copies text to clipboard.",
    "",
    "Backends: whisper.cpp, ONNX Runtime",
    "Models: Hugging Face (ggerganov/whisper.cpp,",
    "istupakov/gigaam-v3-onnx)",
    "",
    "Free and open source under the MIT License.",
];

fn show_about_modal(ctx: &egui::Context, open: &mut bool, dirs: &AppDirs) {
    let screen = ctx.screen_rect();
    let mut close = false;
    let mut close_clicked = false;
    // Dim overlay that swallows clicks, so the UI behind the modal is blocked.
    egui::Area::new(egui::Id::new("about_overlay"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let response = ui.allocate_response(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
            if response.clicked() {
                close = true;
            }
        });
    egui::Area::new(egui::Id::new("about_window"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            egui::Frame::window(&ctx.style()).show(ui, |ui| {
                ui.set_max_width(360.0);
                ui.horizontal(|ui| {
                    ui.heading(format!("Voice Button {}", env!("CARGO_PKG_VERSION")));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("X").clicked() {
                            close_clicked = true;
                        }
                    });
                });
                ui.separator();
                for line in ABOUT_LINES {
                    if line.is_empty() {
                        ui.add_space(4.0);
                    } else {
                        ui.label(*line);
                    }
                }
                ui.hyperlink_to(
                    "GitHub: machine202502/voice-button",
                    "https://github.com/machine202502/voice-button",
                );
                ui.hyperlink_to(
                    "MIT License",
                    "https://github.com/machine202502/voice-button/blob/master/LICENSE",
                );
                ui.separator();
                if ui.button("Open log folder").clicked() {
                    let _ = std::process::Command::new("explorer").arg(&dirs.root).spawn();
                }
            });
        });
    if close || close_clicked {
        *open = false;
    }
}

fn apply_autostart(data: &mut Data) {
    if let Err(e) = autostart::set_enabled(data.config.silent_start, data.config.autostart) {
        data.last_error = Some(format!("failed to update autostart: {e:#}"));
    }
}

fn copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_owned());
    }
}

fn bytes_to_mb(v: u64) -> String {
    format!("{:.1} MB", v as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::{sync_config_from_disk, visible_history_range};
    use crate::config::{Backend, Config};
    use crate::state::Shared;

    #[test]
    fn disk_sync_preserves_a_pending_model_selection() {
        let mut applied = Config::default();
        applied.backend = Backend::Onnx;
        applied.onnx_model = "gigaam-v3-sherpa-int8".to_string();
        let shared = Shared::new(applied.clone(), Vec::new());
        shared.lock().config.onnx_model = "gigaam-v3-rnnt".to_string();

        let mut disk = applied;
        disk.sound_volume = 0.8;
        sync_config_from_disk(&mut shared.lock(), disk);

        let data = shared.lock();
        assert_eq!(data.config.onnx_model, "gigaam-v3-rnnt");
        assert_eq!(data.applied_onnx_model, "gigaam-v3-sherpa-int8");
        assert_eq!(data.config.sound_volume, 0.8);
    }

    #[test]
    fn disk_sync_preserves_a_pending_backend() {
        let shared = Shared::new(Config::default(), Vec::new());
        shared.lock().config.backend = Backend::Onnx;
        sync_config_from_disk(&mut shared.lock(), Config::default());
        assert_eq!(shared.lock().config.backend, Backend::Onnx);
        assert_eq!(shared.lock().applied_backend, Backend::Whisper);
    }

    #[test]
    fn disk_sync_applies_config_when_nothing_is_pending() {
        let shared = Shared::new(Config::default(), Vec::new());
        let mut disk = Config::default();
        disk.backend = Backend::Onnx;
        disk.onnx_model = "gigaam-v3-rnnt".to_string();
        sync_config_from_disk(&mut shared.lock(), disk);
        assert_eq!(shared.lock().config.backend, Backend::Onnx);
        assert_eq!(shared.lock().config.onnx_model, "gigaam-v3-rnnt");
    }

    #[test]
    fn history_viewport_only_materializes_visible_rows() {
        let offsets = (0..=1000).map(|i| i as f32 * 10.0).collect::<Vec<_>>();
        assert_eq!(visible_history_range(&offsets, 450.0, 460.0), 44..48);
        assert_eq!(visible_history_range(&offsets, 0.0, 10.0), 0..3);
        assert_eq!(visible_history_range(&[0.0], 0.0, 100.0), 0..0);
    }
}
