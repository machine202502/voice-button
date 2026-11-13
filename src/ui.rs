use crate::autostart;
use crate::config::{self, AppDirs, Backend, PasteSeparator};
use crate::model_manager;
use crate::i18n::{tr, trf};
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
    History,
    Settings,
    External,
}

/// Page of the "External access" tab: the local server for scripts, or the
/// phone that dictates into monologs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExternalTab {
    LocalServer,
    Phone,
}

/// Which transcriptions the history list shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryFilter {
    All,
    User,
    Server,
    File,
    Phone,
}

impl HistoryFilter {
    fn matches(self, source: state::Source) -> bool {
        match self {
            HistoryFilter::All => true,
            HistoryFilter::User => source == state::Source::User,
            HistoryFilter::Server => source == state::Source::Server,
            HistoryFilter::File => source == state::Source::File,
            HistoryFilter::Phone => source == state::Source::Site,
        }
    }
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
    let snapshot = std::env::var("VB_SNAPSHOT").is_ok();
    // Snapshots keep the English UI so the committed screenshots stay stable.
    let language = if snapshot {
        "en"
    } else {
        config.ui_language.as_str()
    };
    crate::i18n::init(&dirs.root, language);
    if snapshot {
        // Deterministic state for the UI snapshot run.
        let mut cfg = config;
        cfg.ui_language = "en".to_string();
        cfg.backend = Backend::Onnx;
        cfg.onnx_model = "gigaam-v3-sherpa-int8".to_string();
        cfg.language = "auto".to_string();
        history = state::History::from_entries(vec![
            state::HistoryEntry {
                at: "2026-11-05 09:14:32".to_string(),
                text: "Hi! Remind me to call the bank after lunch - I need to check the loan terms and which documents to bring to the meeting with the account manager on Friday, and also ask about early repayment and fees, if any.\nAlso ask whether the meeting can be moved to the morning.\nAnd print two copies of the contract.".to_string(),
                model: "v3_e2e_rnnt".to_string(),
                // Shows how pieces appended with the append hotkey look.
                parts: vec![
                    state::HistoryChunk {
                        at: "2026-11-05 09:16:02".to_string(),
                        text: "Also ask whether the meeting can be moved to the morning.".to_string(),
                    },
                    state::HistoryChunk {
                        at: "2026-11-05 09:17:41".to_string(),
                        text: "And print two copies of the contract.".to_string(),
                    },
                ],
                source: state::Source::User,
                state: crate::history::EntryState::Complete,
                audio: None,
                group: None,
                message: None,
            },
            state::HistoryEntry {
                at: "2026-11-05 11:02:17".to_string(),
                text: "Add milk, bread, eggs and coffee to the shopping list, and do not forget the cat food.".to_string(),
                model: "v3_e2e_ctc".to_string(),
                parts: Vec::new(),
                source: state::Source::User,
                state: crate::history::EntryState::Complete,
                audio: None,
                group: None,
                message: None,
            },
            state::HistoryEntry {
                at: "2026-11-04 18:45:03".to_string(),
                text: "Tomorrow at ten there is a team meeting; prepare the slides for the second section.".to_string(),
                model: "large-v3-turbo-q5_0".to_string(),
                parts: Vec::new(),
                source: state::Source::User,
                state: crate::history::EntryState::Complete,
                audio: None,
                group: None,
                message: None,
            },
            state::HistoryEntry {
                at: "2026-11-04 15:20:44".to_string(),
                text: "Translate the sentence into English: I would be glad to see you at the exhibition on Thursday.".to_string(),
                model: "multi-gigaam-ctc".to_string(),
                parts: Vec::new(),
                source: state::Source::Site,
                state: crate::history::EntryState::Complete,
                audio: None,
                group: Some("Work".to_string()),
                message: Some(42),
            },
        ]);
        // The Phone tab lists what the daemon stores in `site/db.json`. The
        // snapshot has no daemon, so seed that file the way the daemon would:
        // one monolog with two finished takes. The real file poller then picks
        // it up, exactly as it does in the running app.
        let db_path = crate::site::db::Db::path(&dirs.root);
        let _ = std::fs::remove_file(&db_path);
        match crate::site::db::Db::open(&dirs.root) {
            Ok(mut db) => {
                let monolog = db.create_monolog("Work", "2026-11-12T09:30:00+05:00");
                for (clock, seconds, text) in [
                    (
                        "2026-11-12T09:31:00+05:00",
                        4.2,
                        "Remind me to send the invoice to Anna before Friday.",
                    ),
                    (
                        "2026-11-12T09:34:00+05:00",
                        3.1,
                        "Also book a courier for the documents on Thursday morning.",
                    ),
                ] {
                    let message = db.add_message(monolog.id, clock, clock);
                    db.add_seconds(message.id, seconds);
                    db.set_stage(message.id, crate::site::monolog::Stage::Queued);
                    db.set_stage(message.id, crate::site::monolog::Stage::Transcribing);
                    db.set_progress(message.id, text, 1);
                    db.set_stage(message.id, crate::site::monolog::Stage::Done);
                }
                if let Err(e) = db.save() {
                    crate::log::log(&format!("snapshot: cannot seed the phone database: {e:#}"));
                }
            }
            Err(e) => crate::log::log(&format!("snapshot: cannot open the phone database: {e:#}")),
        }
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

/// egui's bundled font covers Latin, Greek and Cyrillic but no CJK glyphs, so a
/// Chinese, Japanese or Korean translation would render as boxes. Add the
/// Windows system fonts for those scripts as fallbacks (no fonts are bundled
/// into the binary).
fn install_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut added = false;
    for (name, candidates) in [
        ("cjk-zh", &["msyh.ttc", "simsun.ttc"][..]),
        ("cjk-ja", &["YuGothM.ttc", "YuGothR.ttc"][..]),
        ("cjk-ko", &["malgun.ttf"][..]),
    ] {
        for file in candidates {
            let path = std::path::Path::new("C:\\Windows\\Fonts").join(file);
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            fonts
                .font_data
                .insert(name.to_string(), egui::FontData::from_owned(bytes));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push(name.to_string());
            }
            added = true;
            break;
        }
    }
    if added {
        ctx.set_fonts(fonts);
    } else {
        crate::log::log("no CJK font found; Chinese/Japanese/Korean may render as boxes");
    }
}

fn run_ui(config: config::Config, history: state::History, dirs: AppDirs, tab: String) {
    let shared = Arc::new(Shared::new(config, history));
    if let Some(pid) = current_parent_pid() {
        spawn_parent_watcher(pid);
    }
    spawn_file_poller(shared.clone(), dirs.clone());
    let initial_tab = if tab == "settings" {
        Tab::Settings
    } else if tab == "history" {
        Tab::History
    } else if tab == "api" {
        Tab::External
    } else {
        Tab::Capture
    };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(WINDOW_TITLE)
            .with_inner_size([560.0, 620.0])
            .with_icon(crate::icons::window_icon()),
        ..Default::default()
    };
    options.viewport = options.viewport.with_active(true);
    let app = MainWindow {
        shared: shared.clone(),
        dirs: dirs.clone(),
        tab: initial_tab,
        external_tab: ExternalTab::LocalServer,
        show_about: false,
        expanded_history: None,
        history_filter: HistoryFilter::All,
        download_done_seen: false,
        api_copied_at: 0.0,
        site_qr: None,
        phone: PhoneState::default(),
        snapshot_dir: std::env::var("VB_SNAPSHOT").ok().map(PathBuf::from),
        snapshot_frame: 0,
    };
    let creator: Box<dyn FnOnce(&eframe::CreationContext<'_>) -> Box<dyn eframe::App>> =
        Box::new(move |cc| {
            install_cjk_fonts(&cc.egui_ctx);
            Box::new(app)
        });
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
    crate::ipc::set_append_hotkey_capture(&dirs.root, false);
    crate::ipc::set_paste_hotkey_capture(&dirs.root, false);
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
        let api_path = dirs.root.join("api.state.json");
        let site_path = dirs.root.join("site.state.json");
        let site_db_path = crate::site::db::Db::path(&dirs.root);
        let network_path = crate::site::network::state_path(&dirs.root);
        let session_path = dirs.root.join("session.json");
        let mut config_meta = meta_key(&std::fs::metadata(&config_path).ok());
        let mut history_meta = meta_key(&std::fs::metadata(&history_path).ok());
        // None forces an initial read when a file already exists. This is
        // essential when the UI is opened after recording/download has begun.
        let mut download_meta = None;
        let mut status_meta = None;
        let mut live_meta = None;
        let mut api_meta = None;
        let mut site_meta = None;
        let mut site_db_meta = None;
        let mut network_meta = None;
        let mut session_meta = None;
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
            let m = meta_key(&std::fs::metadata(&api_path).ok());
            if m != api_meta {
                api_meta = m;
                if let Some(state) = crate::ipc::read_api_state(&dirs.root) {
                    shared.lock().api = Some(state.into());
                }
            }
            let m = meta_key(&std::fs::metadata(&site_path).ok());
            if m != site_meta {
                site_meta = m;
                shared.lock().site = crate::ipc::read_site_state(&dirs.root);
            }
            // The daemon owns `site/db.json`; the window mirrors the pairing
            // record (for the QR code) and the monologs (for the phone list).
            let m = meta_key(&std::fs::metadata(&site_db_path).ok());
            if m != site_db_meta {
                site_db_meta = m;
                let (pairing, monologs) = match crate::site::db::Db::open(&dirs.root) {
                    Ok(db) => {
                        let client = &db.data().client;
                        let pairing = if client.paired || client.token.is_empty() {
                            None
                        } else {
                            Some(state::SitePairing {
                                token: client.token.clone(),
                                until: client.expires.clone(),
                            })
                        };
                        (pairing, Some(state::SiteMonologs::from_db(&db)))
                    }
                    Err(_) => (None, None),
                };
                let mut data = shared.lock();
                data.site_pairing = pairing;
                data.site_monologs = monologs;
            }
            let m = meta_key(&std::fs::metadata(&network_path).ok());
            if m != network_meta {
                network_meta = m;
                shared.lock().site_network = crate::site::network::read(&dirs.root);
            }
            shared.lock().site_network_busy = crate::ipc::site_network_busy(&dirs.root);
            let m = meta_key(&std::fs::metadata(&session_path).ok());
            if m != session_meta {
                session_meta = m;
                let view = crate::ipc::read_session(&dirs.root).map(Into::into);
                shared.lock().session_view = view;
            }
        }
    });
}

/// Which half of the phone list is on screen: active monologs or the archive.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum PhoneView {
    #[default]
    Active,
    Archive,
}

/// Window-side state of the phone list. It does not survive a restart: the
/// daemon owns the monologs, this only remembers what is open and selected.
#[derive(Default)]
struct PhoneState {
    view: PhoneView,
    open: Option<u64>,
    selected: std::collections::BTreeSet<u64>,
    /// Snapshot mode only: scroll the phone page to the monolog list.
    focus_list: bool,
}

/// Messages drawn per frame. A phone can collect thousands of takes; drawing
/// all of them would make the window crawl. The newest ones are shown first.
const PHONE_MESSAGE_CAP: usize = 200;

pub struct MainWindow {
    shared: Arc<Shared>,
    dirs: AppDirs,
    tab: Tab,
    external_tab: ExternalTab,
    show_about: bool,
    expanded_history: Option<usize>,
    history_filter: HistoryFilter,
    download_done_seen: bool,
    /// egui timestamp of the last "Copy" click on the API address; drives the
    /// transient "Copied!" feedback in the API tab.
    api_copied_at: f64,
    /// The QR code of the current pairing link, cached by its text.
    site_qr: Option<(String, egui::TextureHandle)>,
    phone: PhoneState,
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
                ui.selectable_value(&mut self.tab, Tab::Capture, tr("Capture"));
                ui.selectable_value(&mut self.tab, Tab::History, tr("History"));
                ui.selectable_value(&mut self.tab, Tab::Settings, tr("Settings"));
                ui.selectable_value(&mut self.tab, Tab::External, tr("External access"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("About")).clicked() {
                        self.show_about = true;
                    }
                });
            });
            ui.separator();
            match self.tab {
                Tab::Capture => {
                    // The page can outgrow a small window once a file is being
                    // transcribed, so the whole tab scrolls.
                    ScrollArea::vertical()
                        .id_source("capture_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| capture_body(ui, &mut data, &self.dirs));
                }
                Tab::History => history_body(
                    ui,
                    &mut data,
                    &mut self.expanded_history,
                    &mut self.history_filter,
                ),
                Tab::Settings => {
                    // The settings page is taller than the window on small
                    // screens, so keep everything reachable by scrolling.
                    ScrollArea::vertical()
                        .id_source("settings_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| settings_body(ui, &mut data, &self.dirs));
                }
                Tab::External => external_body(
                    ui,
                    &mut data,
                    &self.dirs,
                    ctx,
                    &mut self.api_copied_at,
                    &mut self.external_tab,
                    &mut self.site_qr,
                    &mut self.phone,
                ),
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
            11 => {
                // Illustrative state for the screenshot: the server is on and
                // the address is shown. Nothing is persisted (apply_settings is
                // not called from the snapshot walk).
                let mut data = self.shared.lock();
                data.config.api_enabled = true;
                data.api = Some(state::ApiStatus {
                    running: true,
                    host: "127.0.0.1".to_string(),
                    port: 8931,
                    error: None,
                });
                self.tab = Tab::External;
                self.external_tab = ExternalTab::LocalServer;
            }
            13 => self.capture(&dir.join("api.png")),
            14 => {
                let mut data = self.shared.lock();
                data.config.api_enabled = false;
                data.api = None;
                self.tab = Tab::History;
            }
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
                // The idle "Record" page: the microphone, the live box and the
                // file section that used to live on its own tab.
                self.show_about = false;
                let mut data = self.shared.lock();
                data.status = crate::state::Status::Idle;
                data.live_text.clear();
                data.file = None;
                self.tab = Tab::Capture;
            }
            36 => self.capture(&dir.join("record.png")),
            38 => {
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
            41 => self.capture(&dir.join("download-progress.png")),
            43 => {
                // The phone page with an illustrative paired state: the server
                // is on, an address is shown and a pairing link is waiting.
                self.tab = Tab::External;
                self.external_tab = ExternalTab::Phone;
                // The seeded database holds one monolog with two messages:
                // open it and scroll to the list so the snapshot shows it.
                self.phone.open = Some(1);
                self.phone.selected.insert(2);
                self.phone.focus_list = true;
                let mut data = self.shared.lock();
                data.download = None;
                data.config.site_enabled = true;
                data.site = Some(crate::ipc::SiteSnapshot {
                    running: true,
                    port: 8443,
                    url: "https://192.168.1.10:8443/".to_string(),
                    paired: false,
                });
                data.site_pairing = Some(state::SitePairing {
                    token: "7f3c9a2e5b41d0c8".to_string(),
                    until: (chrono::Local::now() + chrono::Duration::minutes(5)).to_rfc3339(),
                });
                data.site_network = Some(crate::site::network::State {
                    port: 8443,
                    enabled: true,
                    firewall_rule: "Voice Button Site".to_string(),
                    firewall_created: "2026-11-12T10:00:00".to_string(),
                    upnp: None,
                    error: None,
                    updated: "2026-11-12T10:00:01".to_string(),
                });
                data.site_network_busy = false;
            }
            // Several frames let the scroll animation settle before the shot;
            // the target is set on the first frame that draws the list.
            70 => self.capture(&dir.join("phone.png")),
            72 => std::process::exit(0),
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

/// Locate our own window.
///
/// Not by title alone: the installed copy may be running at the same time and
/// `FindWindowW` would return *its* window, belonging to another process. The
/// snapshot then silently captured nothing. The process id is the filter.
fn find_own_window() -> isize {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };

    struct Search {
        pid: u32,
        found: isize,
    }

    unsafe extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        // SAFETY: `param` is the `&mut Search` passed below and outlives the call.
        let search = &mut *(param as *mut Search);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == search.pid && IsWindowVisible(hwnd) != 0 {
            search.found = hwnd as isize;
            return 0;
        }
        1
    }

    let mut search = Search {
        pid: std::process::id(),
        found: 0,
    };
    // SAFETY: `visit` matches the callback signature; the pointer stays valid
    // for the duration of EnumWindows.
    unsafe {
        EnumWindows(Some(visit), &mut search as *mut Search as LPARAM);
    }
    search.found
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
        ui.add(
            egui::ProgressBar::new(fraction)
                .show_percentage()
                .desired_width(220.0),
        );
        ui.label(trf(
            "{model} — file {file}/{files}: {done}{total}",
            &[
                ("model", &dl.model),
                ("file", &(dl.file + 1).to_string()),
                ("files", &dl.files.to_string()),
                ("done", &bytes_to_mb(dl.downloaded)),
                (
                    "total",
                    &dl.total
                        .map(|t| format!(" / {}", bytes_to_mb(t)))
                        .unwrap_or_default(),
                ),
            ],
        ));
        if ui.button(tr("Cancel")).clicked() {
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
    ui.heading(tr("Interface language"));
    let locales = crate::i18n::available();
    let selected = if data.config.ui_language.is_empty() {
        tr("System (automatic)")
    } else {
        locales
            .iter()
            .find(|(code, _)| *code == data.config.ui_language)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| data.config.ui_language.clone())
    };
    let mut language_changed = false;
    ui.horizontal(|ui| {
        ui.label(tr("Language:"));
        egui::ComboBox::from_id_source("ui_language")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                language_changed |= ui
                    .selectable_value(
                        &mut data.config.ui_language,
                        String::new(),
                        tr("System (automatic)"),
                    )
                    .changed();
                for (code, name) in locales {
                    language_changed |= ui
                        .selectable_value(&mut data.config.ui_language, code, name)
                        .changed();
                }
            });
    });
    if language_changed {
        apply_settings(data, dirs);
        // Swap the catalog in place; the rest of this frame already uses it.
        crate::i18n::init(&dirs.root, &data.config.ui_language);
    }
    ui.label(
        RichText::new(tr(
            "Interface translations are JSON files in the locales folder next to the executable. Drop a file there to add a language.",
        ))
        .weak(),
    );
    ui.separator();
    ui.heading(tr("Model format"));
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
                    tr("Whisper (whisper.cpp)"),
                )
                .changed();
            format_changed |= ui
                .radio_value(
                    &mut data.config.backend,
                    Backend::Onnx,
                    tr("ONNX (ONNX Runtime)"),
                )
                .changed();
        });
    });
    if model_switch_locked {
        ui.label(
            RichText::new(tr("Model and runtime switching is locked while recording or transcribing."))
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
            RichText::new(tr("The selected model is not downloaded yet — the selection applies once it is ready."))
                .color(egui::Color32::from_rgb(230, 170, 60)),
        );
    }
    ui.separator();
    if ui.button(tr("Open models folder")).clicked() {
        let _ = std::process::Command::new("explorer")
            .arg(&dirs.models)
            .spawn();
    }
    ui.separator();
    ui.heading(tr("Sounds"));
    let mut sounds_changed = false;
    let mut volume_pct = (data.config.sound_volume * 100.0).round().clamp(0.0, 100.0);
    if ui
        .add(
            egui::Slider::new(&mut volume_pct, 0.0..=100.0)
                .text("%")
                .fixed_decimals(0),
        )
        .changed()
    {
        data.config.sound_volume = (volume_pct / 100.0).clamp(0.0, 1.0);
        sounds_changed = true;
    }
    sounds_changed |= ui
        .checkbox(
            &mut data.config.play_start_sound,
            tr("Play sound when recording starts"),
        )
        .changed();
    ui.horizontal(|ui| {
        if ui.small_button(tr("Play")).clicked() {
            crate::sound::play_start(data.config.sound_volume);
        }
        ui.label(RichText::new(tr("Preview the start cue at the volume above.")).weak());
    });
    sounds_changed |= ui
        .checkbox(
            &mut data.config.play_end_sound,
            tr("Play sound when recording ends"),
        )
        .changed();
    ui.horizontal(|ui| {
        if ui.small_button(tr("Play")).clicked() {
            crate::sound::play_end(data.config.sound_volume);
        }
        ui.label(RichText::new(tr("Preview the end cue at the volume above.")).weak());
    });
    sounds_changed |= ui
        .checkbox(
            &mut data.config.play_error_sound,
            tr("Play sound when the app recovers from a crash"),
        )
        .changed();
    ui.horizontal(|ui| {
        if ui.small_button(tr("Play")).clicked() {
            crate::sound::play_error(data.config.sound_volume);
        }
        ui.label(RichText::new(tr("Preview the crash cue at the volume above.")).weak());
    });
    if sounds_changed {
        apply_settings(data, dirs);
    }
    ui.separator();
    ui.heading(tr("Startup"));
    if ui
        .checkbox(&mut data.config.autostart, tr("Run at Windows startup"))
        .changed()
    {
        apply_autostart(data);
        apply_settings(data, dirs);
    }
    if ui
        .checkbox(
            &mut data.config.silent_start,
            tr("Start silently (no windows when launched at startup)"),
        )
        .changed()
    {
        apply_autostart(data);
        apply_settings(data, dirs);
    }
    ui.separator();
    ui.heading(tr("Microphone"));
    // Device enumeration creates COM objects, so refresh it at most every two
    // seconds instead of on every frame.
    let refresh_devices = data
        .input_devices_at
        .map(|at| at.elapsed() >= Duration::from_secs(2))
        .unwrap_or(true);
    if refresh_devices {
        data.input_devices = crate::recorder::input_device_names();
        data.input_devices_at = Some(std::time::Instant::now());
    }
    let devices = data.input_devices.clone();
    let selected = if data.config.input_device.is_empty() {
        "System default".to_string()
    } else {
        data.config.input_device.clone()
    };
    let mut microphone_changed = false;
    egui::ComboBox::from_id_source("input_device")
        .selected_text(selected)
        .show_ui(ui, |ui| {
            microphone_changed |= ui
                .selectable_value(
                    &mut data.config.input_device,
                    String::new(),
                    tr("System default"),
                )
                .changed();
            for name in &devices {
                microphone_changed |= ui
                    .selectable_value(&mut data.config.input_device, name.clone(), name.as_str())
                    .changed();
            }
        });
    if microphone_changed {
        apply_settings(data, dirs);
    }
    if data.config.input_device.is_empty() {
        ui.label(
            RichText::new(
                tr("Using the Windows default device. Virtual cables (VB-Audio, streaming devices) record silence — pick your real microphone above if transcriptions are empty."),
            )
            .weak(),
        );
    } else {
        ui.label(
            RichText::new(
                tr("Used for new recordings. If the Windows default is a virtual cable, a real microphone is selected automatically."),
            )
            .weak(),
        );
    }
    let mut sensitivity_changed = false;
    sensitivity_changed |= ui
        .add(
            egui::Slider::new(&mut data.config.vad_sensitivity, 0.5..=2.0)
                .text(tr("Microphone sensitivity"))
                .custom_formatter(|value, _| format!("{:.0}%", value * 100.0)),
        )
        .changed();
    if sensitivity_changed {
        data.config.vad_sensitivity = data.config.vad_sensitivity.clamp(0.5, 2.0);
        apply_settings(data, dirs);
    }
    ui.label(
        RichText::new(tr(
            "Raise it if quiet phrases are dropped, lower it in a noisy room.",
        ))
        .weak(),
    );
    ui.separator();
    ui.heading(tr("Mouse"));
    if ui
        .checkbox(
            &mut data.config.swap_mouse_buttons,
            tr("Swap left and middle mouse button actions on the tray icon"),
        )
        .changed()
    {
        apply_settings(data, dirs);
    }
    ui.label(
        RichText::new(if data.config.swap_mouse_buttons {
            tr("Left click records and pastes at the cursor; middle click records a new message.")
        } else {
            tr("Left click records a new message; middle click records and pastes at the cursor.")
        })
        .weak(),
    );
    ui.separator();
    ui.heading(tr("Paste"));
    ui.label(
        RichText::new(
            tr("After the text inserted in record-and-paste mode (paste hotkey or middle click):"),
        )
        .weak(),
    );
    let mut separator_changed = false;
    ui.horizontal(|ui| {
        separator_changed |= ui
            .radio_value(
                &mut data.config.paste_separator,
                PasteSeparator::Space,
                tr("Add a space"),
            )
            .changed();
        separator_changed |= ui
            .radio_value(
                &mut data.config.paste_separator,
                PasteSeparator::Newline,
                tr("Start a new line"),
            )
            .changed();
    });
    if separator_changed {
        apply_settings(data, dirs);
    }
    ui.separator();
    hotkey_section(ui, data, dirs, HotkeyField::Record);
    hotkey_section(ui, data, dirs, HotkeyField::Append);
    hotkey_section(ui, data, dirs, HotkeyField::Paste);
    ui.separator();
    let (format, model_id) = match data.config.backend {
        Backend::Whisper => (Backend::Whisper, data.config.whisper_model.as_str()),
        Backend::Onnx => (Backend::Onnx, data.config.onnx_model.as_str()),
    };
    let supports_auto = model_manager::supports_auto(format, model_id);
    let mut language_changed = false;
    ui.horizontal(|ui| {
        ui.label(tr("Language code:"));
        egui::ComboBox::from_id_source("language_code")
            .selected_text(data.config.language.clone())
            .show_ui(ui, |ui| {
                if supports_auto {
                    language_changed |= ui
                        .selectable_value(
                            &mut data.config.language,
                            "auto".to_string(),
                            tr("auto (detect automatically)"),
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
                tr("The current model cannot detect the language automatically — pick a language code."),
            )
            .color(egui::Color32::from_rgb(230, 170, 60)),
        );
    }
    if let Some(err) = &data.last_error {
        ui.label(
            RichText::new(trf("Error: {error}", &[("error", err)])).color(egui::Color32::from_rgb(230, 90, 90)),
        );
    }
}

/// "External access": the local server for scripts and pages, and the phone
/// that dictates into monologs.
#[allow(clippy::too_many_arguments)]
fn external_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    ctx: &egui::Context,
    copied_at: &mut f64,
    page: &mut ExternalTab,
    qr: &mut Option<(String, egui::TextureHandle)>,
    phone: &mut PhoneState,
) {
    ui.horizontal(|ui| {
        ui.selectable_value(page, ExternalTab::LocalServer, tr("Local server"));
        ui.selectable_value(page, ExternalTab::Phone, tr("Phone"));
    });
    ui.separator();
    match *page {
        ExternalTab::LocalServer => api_body(ui, data, dirs, ctx, copied_at),
        ExternalTab::Phone => phone_body(ui, data, dirs, ctx, copied_at, qr, phone),
    }
}

/// "Phone": switch the HTTPS server on, pair the one phone, show where the QR
/// code points, and list the monologs the phone has dictated.
fn phone_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    ctx: &egui::Context,
    copied_at: &mut f64,
    qr: &mut Option<(String, egui::TextureHandle)>,
    phone: &mut PhoneState,
) {
    ScrollArea::vertical()
        .id_source("phone_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            phone_settings(ui, data, dirs, ctx, copied_at, qr, phone);
        });
}

fn phone_settings(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    ctx: &egui::Context,
    copied_at: &mut f64,
    qr: &mut Option<(String, egui::TextureHandle)>,
    phone: &mut PhoneState,
) {
    ui.heading(tr("Phone"));
    ui.label(
        RichText::new(tr(
            "Dictation from the phone goes into monologs; the text is transcribed here, in the background.",
        ))
        .weak(),
    );
    ui.add_space(6.0);
    let mut commit = ui
        .checkbox(
            &mut data.config.site_enabled,
            tr("Accept connections from the phone"),
        )
        .changed();
    ui.add_enabled_ui(data.config.site_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label(tr("Port:"));
            let port = ui.add(egui::DragValue::new(&mut data.config.site_port).clamp_range(1..=65535));
            commit |= port.drag_stopped() || port.lost_focus();
        });
        let public = crate::site::gate::has_public_ipv4();
        commit |= ui
            .add_enabled(
                !public,
                egui::Checkbox::new(
                    &mut data.config.site_lan_only,
                    tr("Accept only from the local network"),
                ),
            )
            .changed();
        if public {
            ui.label(
                RichText::new(tr(
                    "This computer has a public address, so a phone in the same room looks like any other visitor: the switch does not apply, the token protects the door.",
                ))
                .weak()
                .small(),
            );
        }
    });
    if commit {
        apply_settings(data, dirs);
    }

    let status = data.site.clone();
    match &status {
        Some(site) if site.running => {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&site.url)
                        .monospace()
                        .color(egui::Color32::from_rgb(90, 200, 120)),
                );
                if ui.button(tr("Copy")).clicked() {
                    copy_to_clipboard(&site.url);
                    *copied_at = ctx.input(|i| i.time);
                }
                if site.paired {
                    ui.label(
                        RichText::new(tr("The phone is connected"))
                            .color(egui::Color32::from_rgb(70, 190, 90)),
                    );
                } else {
                    ui.label(RichText::new(tr("No phone is connected")).weak());
                }
            });
            if *copied_at > 0.0 && ctx.input(|i| i.time) < *copied_at + 2.0 {
                ui.label(RichText::new(tr("Copied!")).color(egui::Color32::from_rgb(90, 200, 120)));
            }
            let now = chrono::Local::now().to_rfc3339();
            if let Some(pairing) = data
                .site_pairing
                .clone()
                .filter(|pairing| pairing.alive(&now))
            {
                let link = format!("{}pair?t={}", site.url, pairing.token);
                ui.add_space(6.0);
                ui.label(tr("Open this link on the phone:"));
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(&link).monospace()).wrap(true));
                    if ui.button(tr("Copy")).clicked() {
                        copy_to_clipboard(&link);
                    }
                });
                // The QR is cached by its text: rebuilding a texture every frame
                // would burn the GPU for nothing.
                if qr.as_ref().map(|(text, _)| text != &link).unwrap_or(true) {
                    *qr = qr_texture(ctx, &link).map(|texture| (link.clone(), texture));
                }
                if let Some((_, texture)) = qr.as_ref() {
                    ui.add(egui::Image::new(texture).fit_to_exact_size(egui::vec2(190.0, 190.0)));
                }
                // `2026-11-12T10:05:00+05:00` -> `10:05`
                let clock = pairing
                    .until
                    .get(11..16)
                    .unwrap_or(pairing.until.as_str())
                    .to_string();
                ui.label(
                    RichText::new(trf(
                        "The link lives until {time}, then it stops working.",
                        &[("time", &clock)],
                    ))
                    .color(egui::Color32::from_rgb(230, 170, 60)),
                );
                ui.label(
                    RichText::new(tr(
                        "The link works once. The phone will warn about the certificate: tap through it.",
                    ))
                    .weak(),
                );
            }
            if site.paired {
                if ui.button(tr("Disconnect phone")).clicked() {
                    crate::ipc::request_site_revoke(&dirs.root);
                }
            } else if ui.button(tr("Connect phone")).clicked() {
                crate::ipc::request_site_pair(&dirs.root);
            }

            // Internet access: the script opens the firewall and, where it can,
            // the router. The app only shows what the script reported.
            ui.add_space(6.0);
            let network = data.site_network.clone();
            let busy = data.site_network_busy;
            egui::Frame::group(ui.style()).show(ui, |ui| {
                // The state is the first thing the eye lands on: a coloured dot
                // and one plain sentence.
                let (dot, title) = match (&network, busy) {
                    (_, true) => (
                        egui::Color32::from_rgb(230, 170, 60),
                        tr("Setting up…"),
                    ),
                    (Some(network), _) if network.enabled && network.error.is_none() => (
                        egui::Color32::from_rgb(90, 200, 110),
                        tr("Internet access is on"),
                    ),
                    (Some(network), _) if network.error.is_some() => (
                        egui::Color32::from_rgb(230, 90, 90),
                        tr("Internet access failed"),
                    ),
                    _ => (egui::Color32::from_gray(140), tr("Internet access is off")),
                };
                ui.horizontal(|ui| {
                    let size = egui::vec2(14.0, 14.0);
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter()
                        .circle_filled(rect.center(), 6.0, dot);
                    ui.label(RichText::new(title).strong().color(dot));
                    ui.label(RichText::new(tr("Internet access")).weak().small());
                });
                match &network {
                    Some(network) if network.enabled => {
                        ui.label(RichText::new(crate::site::network::summary(network)).weak().small());
                    }
                    Some(network) if network.error.is_some() => {
                        ui.label(
                            RichText::new(network.error.clone().unwrap_or_default())
                                .color(egui::Color32::from_rgb(230, 90, 90))
                                .small(),
                        );
                    }
                    _ => {
                        ui.label(
                            RichText::new(tr(
                                "The phone is reachable only inside this network until the access is switched on.",
                            ))
                            .weak()
                            .small(),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Enable internet access")))
                        .clicked()
                    {
                        crate::ipc::request_site_network(&dirs.root, true);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Disable and remove")))
                        .clicked()
                    {
                        crate::ipc::request_site_network(&dirs.root, false);
                    }
                });
                ui.label(
                    RichText::new(tr(
                        "A public address on this computer needs no forwarding; otherwise the router has to support UPnP.",
                    ))
                    .weak()
                    .small(),
                );
            });
        }
        Some(_) if data.config.site_enabled => {
            // The server is started and stopped live, so this branch means it
            // was asked for and could not listen: say why, not "restart".
            ui.label(RichText::new(tr("Stopped")).color(egui::Color32::GRAY));
            ui.label(
                RichText::new(tr("The phone server did not start. The log has the reason."))
                .weak()
                .small(),
            );
        }
        _ => {
            ui.label(RichText::new(tr("Stopped")).color(egui::Color32::GRAY));
        }
    }

    // The monologs stay visible whether the server is running or not: they
    // belong to the person, not to the connection.
    ui.separator();
    let list_top = ui.cursor().min;
    phone_monologs(ui, &data.site_monologs, phone, dirs);
    if phone.focus_list {
        // Snapshot mode only: put the list in view for the screenshot.
        ui.scroll_to_rect(
            egui::Rect::from_min_size(list_top, egui::vec2(ui.available_width(), 1.0)),
            Some(egui::Align::TOP),
        );
    }
}

/// The monologs the phone dictated, with the open one's messages below.
fn phone_monologs(
    ui: &mut egui::Ui,
    monologs: &Option<state::SiteMonologs>,
    phone: &mut PhoneState,
    dirs: &AppDirs,
) {
    ui.heading(tr("Monologs"));
    let Some(site) = monologs else {
        ui.label(RichText::new(tr("No monologs yet.")).weak());
        return;
    };
    if site.monologs.is_empty() {
        ui.label(RichText::new(tr("No monologs yet.")).weak());
        return;
    }
    if let Some(open) = phone.open {
        if !site.monologs.iter().any(|monolog| monolog.id == open) {
            // The phone removed it (or the file was replaced): do not keep a
            // selection for something that is no longer there.
            phone.open = None;
            phone.selected.clear();
        }
    }
    // Active and archived are two different jobs: read and archive the first,
    // only unarchive the second. One mixed list made neither clear.
    ui.horizontal(|ui| {
        ui.selectable_value(&mut phone.view, PhoneView::Active, tr("Active"));
        ui.selectable_value(&mut phone.view, PhoneView::Archive, tr("Archive"));
    });
    if phone.view == PhoneView::Archive {
        // The archive is a list, not a reader: nothing there can be opened.
        phone.open = None;
    }
    let showing_archive = phone.view == PhoneView::Archive;
    let mut list: Vec<&state::SiteMonolog> = site
        .monologs
        .iter()
        .filter(|monolog| monolog.archived == showing_archive)
        .collect();
    if list.is_empty() {
        ui.label(
            RichText::new(if showing_archive {
                tr("The archive is empty.")
            } else {
                tr("No monologs yet.")
            })
            .weak(),
        );
        return;
    }
    // Newest monolog first, like the history. The id breaks ties for records
    // written in the same second.
    list.sort_by(|left, right| {
        right
            .created
            .cmp(&left.created)
            .then_with(|| right.id.cmp(&left.id))
    });
    let mut archive: Option<(u64, bool)> = None;
    // One monolog on screen at a time: opening it hides the others, and Back
    // (or clicking it again) returns to the list. The archive stays a list.
    if !showing_archive {
        if let Some(open) = phone.open {
            if let Some(monolog) = list.iter().copied().find(|monolog| monolog.id == open) {
                ui.horizontal(|ui| {
                    if ui.button(tr("Back")).clicked() {
                        phone.open = None;
                        phone.selected.clear();
                    }
                    ui.label(
                        RichText::new(format!("{} ({})", monolog.name, monolog.count())).strong(),
                    );
                    if ui.small_button(tr("Archive")).clicked() {
                        archive = Some((monolog.id, true));
                    }
                });
                ui.indent(("phone_messages", monolog.id), |ui| {
                    phone_messages(ui, monolog, phone);
                });
                if let Some((monolog, archived)) = archive {
                    crate::ipc::request_site_archive(&dirs.root, monolog, archived);
                }
                return;
            }
        }
    }
    for monolog in list {
        if showing_archive {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} ({})", monolog.name, monolog.count())).strong(),
                );
                if ui.small_button(tr("Unarchive")).clicked() {
                    archive = Some((monolog.id, false));
                }
            });
            continue;
        }
        ui.horizontal(|ui| {
            let open = phone.open == Some(monolog.id);
            let title = format!("{} ({})", monolog.name, monolog.count());
            if ui
                .selectable_label(open, RichText::new(title).strong())
                .clicked()
            {
                if open {
                    phone.open = None;
                } else {
                    phone.open = Some(monolog.id);
                    phone.selected.clear();
                }
            }
            if ui.small_button(tr("Archive")).clicked() {
                archive = Some((monolog.id, true));
            }
        });
        if phone.open == Some(monolog.id) {
            ui.indent(("phone_messages", monolog.id), |ui| {
                phone_messages(ui, monolog, phone);
            });
        }
    }
    if let Some((monolog, archived)) = archive {
        crate::ipc::request_site_archive(&dirs.root, monolog, archived);
    }
}

/// Messages of one open monolog: selection, copy, and the takes themselves.
fn phone_messages(ui: &mut egui::Ui, monolog: &state::SiteMonolog, phone: &mut PhoneState) {
    let total = monolog.messages.len();
    if total == 0 {
        ui.label(RichText::new(tr("No messages from the phone yet.")).weak());
        return;
    }
    ui.horizontal(|ui| {
        if ui.small_button(tr("Select all")).clicked() {
            for message in &monolog.messages {
                phone.selected.insert(message.id);
            }
        }
        if ui.small_button(tr("Clear selection")).clicked() {
            phone.selected.clear();
        }
        let count = monolog
            .messages
            .iter()
            .filter(|message| phone.selected.contains(&message.id))
            .count();
        let copy = ui.add_enabled(
            count > 0,
            egui::Button::new(trf(
                "Copy selected ({count})",
                &[("count", &count.to_string())],
            )),
        );
        if copy.clicked() {
            let text = selected_text(&monolog.messages, &phone.selected);
            if !text.is_empty() {
                copy_to_clipboard(&text);
            }
        }
    });
    ui.add_space(2.0);
    // Like the phone's own chat: oldest at the top, newest at the bottom, one
    // header per day. Only the newest `PHONE_MESSAGE_CAP` are drawn, so the
    // window stays fast even when the phone has collected thousands of takes.
    let first = total.saturating_sub(PHONE_MESSAGE_CAP);
    let mut last_day = String::new();
    for message in &monolog.messages[first..] {
        let day = message.recorded.get(0..10).unwrap_or_default().to_string();
        if day != last_day {
            ui.separator();
            ui.label(
                RichText::new(day_header(&message.recorded))
                    .small()
                    .strong()
                    .color(ui.visuals().weak_text_color()),
            );
            last_day = day;
        }
        ui.push_id(message.id, |ui| {
            ui.horizontal(|ui| {
                let mut checked = phone.selected.contains(&message.id);
                if ui.checkbox(&mut checked, "").changed() {
                    if checked {
                        phone.selected.insert(message.id);
                    } else {
                        phone.selected.remove(&message.id);
                    }
                }
                ui.label(RichText::new(message_time(&message.recorded)).small().weak());
                ui.label(RichText::new(stage_label(message.stage)).small().weak());
            });
            if message.text.trim().is_empty() {
                ui.label(RichText::new("…").weak());
            } else {
                ui.add(egui::Label::new(RichText::new(&message.text)).wrap(true));
            }
            ui.add_space(2.0);
        });
    }
    if total > PHONE_MESSAGE_CAP {
        ui.label(
            RichText::new(trf(
                "Showing the newest {shown} of {total} messages.",
                &[
                    ("shown", &PHONE_MESSAGE_CAP.to_string()),
                    ("total", &total.to_string()),
                ],
            ))
            .small()
            .weak(),
        );
    }
}

/// The text "Copy selected" puts on the clipboard: the selected takes in the
/// order they were spoken, separated by a blank line. Empty takes are skipped.
fn selected_text(messages: &[state::SiteMessage], selected: &std::collections::BTreeSet<u64>) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for message in messages {
        if !selected.contains(&message.id) {
            continue;
        }
        let text = message.text.trim();
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join("\n\n")
}

/// `2026-11-12T09:31:00+05:00` -> `2026-11-12 09:31`; an unknown shape is
/// shown unchanged instead of being hidden.
fn message_time(recorded: &str) -> String {
    match recorded.get(0..16) {
        Some(stamp) => stamp.replace('T', " "),
        None => recorded.to_string(),
    }
}

/// The one word the phone list shows for a stage. The labels live in the
/// translations; `no-model` is the one key that does not match `Stage::label`.
fn stage_label(stage: crate::site::monolog::Stage) -> String {
    match stage {
        crate::site::monolog::Stage::NoModel => tr("no_model"),
        stage => tr(stage.label()),
    }
}

/// QR code of the pairing link, as a texture the window can draw.
fn qr_texture(ctx: &egui::Context, text: &str) -> Option<egui::TextureHandle> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let quiet = 2usize;
    let scale = 4usize;
    let side = (width + quiet * 2) * scale;
    let colors = code.to_colors();
    let mut image = egui::ColorImage::new([side, side], egui::Color32::WHITE);
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = (x + quiet) * scale + dx;
                    let py = (y + quiet) * scale + dy;
                    image[(px, py)] = egui::Color32::from_rgb(20, 24, 20);
                }
            }
        }
    }
    Some(ctx.load_texture("site_qr", image, egui::TextureOptions::NEAREST))
}

fn api_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    ctx: &egui::Context,
    copied_at: &mut f64,
) {
    ui.heading(tr("Local server"));
    ui.label(
        RichText::new(
            tr("Accepts sound from a page or a script (PCM16, mono) and returns the transcription. The app's own microphone is not used."),
        )
        .weak(),
    );
    ui.add_space(6.0);
    // Edited in place, but only saved (and the server only restarted) when the
    // field is committed: Enter, leaving a field, or toggling the switch.
    let mut commit = ui
        .checkbox(&mut data.config.api_enabled, tr("Enable the local API server"))
        .changed();
    ui.add_enabled_ui(data.config.api_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.label(tr("Host:"));
            let host = ui.add(
                egui::TextEdit::singleline(&mut data.config.api_host)
                    .desired_width(150.0)
                    .hint_text("127.0.0.1"),
            );
            commit |= host.lost_focus();
            ui.label(tr("Port:"));
            let port = ui.add(
                egui::DragValue::new(&mut data.config.api_port).clamp_range(1..=65535),
            );
            commit |= port.drag_stopped() || port.lost_focus();
        });
    });
    if commit {
        data.config.api_host = data.config.api_host.trim().to_string();
        apply_settings(data, dirs);
    }
    let loopback = matches!(data.config.api_host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if data.config.api_enabled && !loopback {
        ui.label(
            RichText::new(
                tr("This host is reachable from the network: anyone who can open the port can start recordings."),
            )
            .color(egui::Color32::from_rgb(230, 170, 60)),
        );
    }

    ui.separator();
    let running = data.api.as_ref().map(|s| s.running).unwrap_or(false);
    if running {
        let status = data.api.as_ref().expect("checked above");
        let url = format!("http://{}:{}/", browse_host(&status.host), status.port);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&url)
                    .monospace()
                    .color(egui::Color32::from_rgb(90, 200, 120)),
            );
            if ui.button(tr("Copy")).clicked() {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    let _ = clipboard.set_text(&url);
                }
                *copied_at = ctx.input(|i| i.time);
            }
        });
        // Transient "Copied!" feedback right after a copy click.
        if *copied_at > 0.0 && ctx.input(|i| i.time) < *copied_at + 2.0 {
            ui.label(RichText::new(tr("Copied!")).color(egui::Color32::from_rgb(90, 200, 120)));
        }
    } else if data.config.api_enabled {
        match data.api.as_ref().and_then(|s| s.error.clone()) {
            Some(error) => ui.label(
                RichText::new(trf("Not listening: {error}", &[("error", &error)]))
                    .color(egui::Color32::from_rgb(230, 90, 90)),
            ),
            None => ui.label(RichText::new(tr("Starting…")).color(egui::Color32::GRAY)),
        };
    } else {
        ui.label(RichText::new(tr("Stopped")).color(egui::Color32::GRAY));
    }

    ui.separator();
    ui.heading(tr("Endpoints"));
    ui.label(tr("GET  /api/help          — full contract for integrations (JSON)."));
    ui.label(tr("GET  /api/status        — state, live text, received audio."));
    ui.label(tr("GET  /api/models        — models this build can use."));
    ui.label(tr("POST /api/record/start  — open a session (model, sampleRate)."));
    ui.label(tr("POST /api/record/audio  — PCM16 LE mono chunk (?session=N)."));
    ui.label(tr("POST /api/record/stop   — finish and transcribe the buffer."));
    ui.label(tr("GET  /api/record/live   — live text of the session."));
    ui.label(tr("GET  /api/record/text   — transcript of the last session."));
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            tr("The test page is a separate project: api-demo/ (run: node api-demo/server.mjs). The API has no authentication — anyone who can reach the port can send audio."),
        )
        .weak(),
    );
}

/// Host to put in a clickable link: a wildcard bind address is not browsable.
fn browse_host(host: &str) -> &str {
    match host {
        "" | "0.0.0.0" | "::" => "127.0.0.1",
        other => other,
    }
}

/// Which config shortcut a hotkey section edits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HotkeyField {
    /// Plain record start/stop shortcut.
    Record,
    /// "Append to the last message" shortcut.
    Append,
    /// Record a new message and paste it at the caret.
    Paste,
}

impl HotkeyField {
    fn stored(self, data: &Data) -> &str {
        match self {
            HotkeyField::Record => &data.config.hotkey,
            HotkeyField::Append => &data.config.append_hotkey,
            HotkeyField::Paste => &data.config.paste_hotkey,
        }
    }

    fn stored_mut(self, data: &mut Data) -> &mut String {
        match self {
            HotkeyField::Record => &mut data.config.hotkey,
            HotkeyField::Append => &mut data.config.append_hotkey,
            HotkeyField::Paste => &mut data.config.paste_hotkey,
        }
    }

    fn capturing(self, data: &Data) -> bool {
        match self {
            HotkeyField::Record => data.capturing_hotkey,
            HotkeyField::Append => data.capturing_append_hotkey,
            HotkeyField::Paste => data.capturing_paste_hotkey,
        }
    }

    fn set_capturing(self, data: &mut Data, value: bool) {
        match self {
            HotkeyField::Record => data.capturing_hotkey = value,
            HotkeyField::Append => data.capturing_append_hotkey = value,
            HotkeyField::Paste => data.capturing_paste_hotkey = value,
        }
    }
}

/// The Settings window starts capturing one shortcut at a time.
fn start_hotkey_capture(data: &mut Data, dirs: &AppDirs, field: HotkeyField) {
    for other in [HotkeyField::Record, HotkeyField::Append, HotkeyField::Paste] {
        other.set_capturing(data, other == field);
    }
    // Release the global hotkeys while capturing, otherwise the very
    // combination being typed would toggle recording instead of
    // reaching this window.
    crate::ipc::set_hotkey_capture(&dirs.root, field == HotkeyField::Record);
    crate::ipc::set_append_hotkey_capture(&dirs.root, field == HotkeyField::Append);
    crate::ipc::set_paste_hotkey_capture(&dirs.root, field == HotkeyField::Paste);
    let _ = std::fs::remove_file(crate::ipc::hotkey_result_path(&dirs.root));
    crate::log::log("hotkey capture started");
}

fn stop_hotkey_capture(data: &mut Data, dirs: &AppDirs) {
    for field in [HotkeyField::Record, HotkeyField::Append, HotkeyField::Paste] {
        field.set_capturing(data, false);
    }
    crate::ipc::set_hotkey_capture(&dirs.root, false);
    crate::ipc::set_append_hotkey_capture(&dirs.root, false);
    crate::ipc::set_paste_hotkey_capture(&dirs.root, false);
}

fn hotkey_section(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, field: HotkeyField) {
    let heading = tr(match field {
        HotkeyField::Record => "Global hotkey",
        HotkeyField::Append => "Append hotkey (continue the last message)",
        HotkeyField::Paste => "Paste hotkey (type at the cursor)",
    });
    ui.heading(heading);
    ui.label(
        RichText::new(match field {
            HotkeyField::Record => {
                tr("Records a new message; the transcribed text is copied to the clipboard and saved as a new history entry.")
            }
            HotkeyField::Append => {
                tr("Records a piece and appends it to the last message; the full merged text is copied to the clipboard afterwards.")
            }
            HotkeyField::Paste => {
                tr("Records a new message and pastes it at the cursor of the window that was active when recording started; the previous clipboard content is restored afterwards.")
            }
        })
        .weak(),
    );
    let stored = field.stored(data).to_string();
    ui.horizontal(|ui| {
        if stored.is_empty() {
            ui.monospace(tr("disabled"));
        } else {
            ui.monospace(stored);
        }
        if field.capturing(data) {
            ui.label(
                RichText::new(tr("press Ctrl/Alt/Shift + key (or an F-key), Esc cancels"))
                    .color(egui::Color32::from_rgb(230, 170, 60)),
            );
        } else if ui.button(tr("Record shortcut")).clicked() {
            start_hotkey_capture(data, dirs, field);
        }
        if ui.button(tr("Clear")).clicked() {
            field.stored_mut(data).clear();
            apply_settings(data, dirs);
            // Clearing means "pick a new one": start listening right away.
            start_hotkey_capture(data, dirs, field);
        }
    });
    if !field.capturing(data) {
        return;
    }
    let mut apply_captured = |captured: Option<crate::hotkey::Hotkey>| {
        stop_hotkey_capture(data, dirs);
        if let Some(hotkey) = captured {
            let label = hotkey.label();
            *field.stored_mut(data) = label.clone();
            crate::log::log(&format!("hotkey captured: {label}"));
            apply_settings(data, dirs);
        }
    };
    // The daemon watches the physical keyboard (the window may not hold
    // focus) and reports the captured combination through this file.
    if let Some(text) = crate::ipc::take_hotkey_result(&dirs.root) {
        apply_captured(if text == "cancel" {
            None
        } else {
            crate::hotkey::parse(&text)
        });
    }
    // Duplicate path when the window holds focus: read the events directly.
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
        apply_captured(captured);
    }
}

fn download_section(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, model_id: &str) {
    let active = data
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
            ui.label(trf(
                "File {file}/{files}: {done}{total}",
                &[
                    ("file", &(dl.file + 1).to_string()),
                    ("files", &dl.files.to_string()),
                    ("done", &bytes_to_mb(dl.downloaded)),
                    (
                        "total",
                        &dl.total
                            .map(|t| format!(" / {}", bytes_to_mb(t)))
                            .unwrap_or_default(),
                    ),
                ],
            ));
            if ui.button(tr("Cancel")).clicked() {
                crate::ipc::request_cancel(&dirs.root);
            }
        } else if let Some(err) = &dl.error {
            if dl.model == model_id {
                ui.label(
                    RichText::new(trf("Download error: {error}", &[("error", err)]))
                        .color(egui::Color32::from_rgb(230, 90, 90)),
                );
            }
        } else if dl.done && dl.model == model_id {
            ui.label(tr("Download complete."));
        }
    }
    let already_downloaded = model_manager::is_ready(&dirs.models, model_id);
    if !active && !already_downloaded && ui.button(trf("Download {model}", &[("model", model_id)])).clicked() {
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
        egui::ComboBox::from_label(tr("Whisper model"))
            .selected_text(if custom {
                "Custom file...".to_string()
            } else {
                let marker = if model_manager::is_ready(&dirs.models, &data.config.whisper_model) {
                    String::new()
                } else {
                    tr(" (not downloaded)")
                };
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
                    let marker = if model_manager::is_ready(&dirs.models, model.id) {
                        String::new()
                    } else {
                        tr(" (not downloaded)")
                    };
                    changed |= ui
                        .selectable_value(
                            &mut data.config.whisper_model,
                            model.id.to_string(),
                            format!("{} ({}){marker}", model.id, model.size),
                        )
                        .changed();
                }
                changed |= ui
                    .selectable_value(
                        &mut data.config.whisper_model,
                        "custom".to_string(),
                        tr("Custom file..."),
                    )
                    .changed();
            });
    });
    if changed {
        apply_settings(data, dirs);
    }
    if !custom {
        let id = data.config.whisper_model.clone();
        if model_manager::is_ready(&dirs.models, &id) {
            ui.label(RichText::new(tr("Model is ready.")).color(egui::Color32::from_rgb(90, 200, 120)));
        }
        download_section(ui, data, dirs, &id);
    } else {
        match data.config.whisper_file.clone() {
            Some(file) => {
                ui.monospace(&file);
                if !PathBuf::from(&file).exists() {
                    ui.label(
                        RichText::new(tr("File does not exist."))
                            .color(egui::Color32::from_rgb(230, 170, 60)),
                    );
                }
            }
            None => {
                ui.label(tr("No file selected."));
            }
        }
        if ui.button(tr("Choose .ggml model file...")).clicked() {
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
        egui::ComboBox::from_label(tr("ONNX model"))
            .selected_text(if custom {
                "Custom folder...".to_string()
            } else {
                let marker = if model_manager::is_ready(&dirs.models, &data.config.onnx_model) {
                    String::new()
                } else {
                    tr(" (not downloaded)")
                };
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
                    let marker = if model_manager::is_ready(&dirs.models, model.id) {
                        String::new()
                    } else {
                        tr(" (not downloaded)")
                    };
                    changed |= ui
                        .selectable_value(
                            &mut data.config.onnx_model,
                            model.id.to_string(),
                            format!("{} ({}){marker}", model.id, model.size),
                        )
                        .changed();
                }
                changed |= ui
                    .selectable_value(
                        &mut data.config.onnx_model,
                        "custom".to_string(),
                        tr("Custom folder..."),
                    )
                    .changed();
            });
    });
    if changed {
        apply_settings(data, dirs);
    }
    if !custom {
        let id = data.config.onnx_model.clone();
        if model_manager::is_ready(&dirs.models, &id) {
            ui.label(RichText::new(tr("Model is ready.")).color(egui::Color32::from_rgb(90, 200, 120)));
        }
        download_section(ui, data, dirs, &id);
    } else {
        match data.config.onnx_folder.clone() {
            Some(folder) => {
                ui.monospace(&folder);
                let path = PathBuf::from(&folder);
                let family = model_manager::detect_family(&path);
                let model_ok = model_manager::onnx_folder_ready(&path, family);
                let vocab_ok = path.join("vocab.txt").exists() || path.join("vocab.json").exists();
                let graph_ok = path.join("model.onnx").exists()
                    || (path.join("encoder.onnx").exists()
                        && path.join("decoder.onnx").exists()
                        && path.join("joint.onnx").exists());
                if model_ok {
                    ui.label(
                        RichText::new(tr("Model files found."))
                            .color(egui::Color32::from_rgb(90, 200, 120)),
                    );
                } else {
                    let missing = [
                        (!vocab_ok).then(|| "vocab.txt (or vocab.json)".to_string()),
                        (!graph_ok).then(|| {
                            tr("model.onnx (CTC) or encoder.onnx + decoder.onnx + joint.onnx (RNN-T)")
                        }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(", ");
                    ui.label(
                        RichText::new(trf("Missing files: {missing}", &[("missing", &missing)]))
                            .color(egui::Color32::from_rgb(230, 170, 60)),
                    );
                }
            }
            None => {
                ui.label(tr("No folder selected."));
            }
        }
        if ui.button(tr("Choose folder...")).clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                data.config.onnx_folder = Some(path.to_string_lossy().to_string());
                apply_settings(data, dirs);
            }
        }
    }
}

/// Open a file with its default application (explorer <file> does exactly
/// that without pulling in another Win32 dependency).
fn open_path(path: &str) {
    if let Err(e) = std::process::Command::new("explorer").arg(path).spawn() {
        crate::log::log(&format!("cannot open {path}: {e}"));
    }
}

/// Open the folder that contains the file and select it.
fn show_in_folder(path: &str) {
    if let Err(e) = std::process::Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn()
    {
        crate::log::log(&format!("cannot reveal {path}: {e}"));
    }
}

/// Tab "File": pick an audio file and watch its transcription. The file is read
/// where it is; the daemon never copies it.
fn file_body(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs) {
    let refresh = data
        .file_seen
        .map(|at| at.elapsed() >= Duration::from_millis(250))
        .unwrap_or(true);
    if refresh {
        data.file = crate::ipc::read_file_state(&dirs.root);
        data.file_seen = Some(std::time::Instant::now());
    }
    ui.heading(tr("Transcribe a file"));
    ui.label(
        RichText::new(tr(
            "Audio files (WAV, MP3, FLAC, OGG, M4A) and videos (MP4, MOV, MKV) — the audio track is taken from a video. The file stays where it is: nothing is copied into the application, and the text goes to the history.",
        ))
        .weak(),
    );
    let running = data.status == crate::state::Status::File;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new(tr("Choose file…")))
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    tr("Audio and video"),
                    &[
                        "wav", "mp3", "flac", "ogg", "oga", "m4a", "aac", "opus", "mp4", "m4v",
                        "mov", "mkv", "webm",
                    ],
                )
                .pick_file()
            {
                crate::ipc::write_file_request(&dirs.root, &path.to_string_lossy());
            }
        }
        if running && ui.button(tr("Cancel")).clicked() {
            crate::ipc::request_file_cancel(&dirs.root);
        }
    });
    ui.separator();
    let Some(file) = data.file.clone() else {
        ui.label(RichText::new(tr("No file chosen yet.")).weak());
        return;
    };
    ui.label(RichText::new(&file.name).strong());
    if file.total_seconds > 0.0 {
        let progress = (file.done_seconds / file.total_seconds).clamp(0.0, 1.0) as f32;
        ui.add(
            egui::ProgressBar::new(progress).text(trf(
                "{done} / {total} s",
                &[
                    ("done", &format!("{:.0}", file.done_seconds)),
                    ("total", &format!("{:.0}", file.total_seconds)),
                ],
            )),
        );
    } else if file.running {
        ui.add(egui::ProgressBar::new(0.0).text(trf(
            "{done} s",
            &[("done", &format!("{:.0}", file.done_seconds))],
        )));
    }
    if let Some(error) = &file.error {
        ui.label(RichText::new(error).color(egui::Color32::from_rgb(200, 70, 70)));
    }
    if file.cancelled {
        ui.label(RichText::new(tr("Cancelled")).weak());
    }
    if !file.text.is_empty() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_source("file_transcript")
                .max_height(220.0)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(egui::Label::new(file.text.clone()).wrap(true));
                });
        });
        if ui.button(tr("Copy")).clicked() {
            copy_to_clipboard(&file.text);
        }
    }
}

fn capture_body(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs) {
    let status = data.status;
    match status {
        crate::state::Status::Recording => {
            if ui
                .button(RichText::new(tr("Stop recording")).color(egui::Color32::from_rgb(200, 60, 60)))
                .clicked()
            {
                crate::ipc::write_record_request(&dirs.root, r#"{"kind":"user"}"#);
            }
            // The same controls and cues serve both interactors; only the
            // attribution and the audio source differ.
            let server_session = data
                .session_view
                .as_ref()
                .filter(|session| session.owner == crate::state::InteractorKind::Server)
                .map(|session| session.model.clone());
            if let Some(model) = server_session {
                let model = model.map(|model| format!(" · {model}")).unwrap_or_default();
                ui.label(
                    RichText::new(trf("API session: receiving audio{model}", &[("model", &model)]))
                        .color(egui::Color32::from_rgb(230, 170, 60)),
                );
                ui.label(
                    RichText::new(
                        tr("The sound comes from the API client, not from this machine. The text goes back to the caller and is saved in the history; Stop ends the session."),
                    )
                    .weak(),
                );
            } else {
                if data.live_started_ms > 0 {
                    let elapsed = chrono::Local::now().timestamp_millis() - data.live_started_ms;
                    let secs = (elapsed / 1000).max(0);
                    ui.label(
                        RichText::new(trf("REC {secs}s", &[("secs", &secs.to_string())]))
                            .color(egui::Color32::from_rgb(220, 60, 60)),
                    );
                } else {
                    ui.label(RichText::new(tr("REC")).color(egui::Color32::from_rgb(220, 60, 60)));
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
                                        tr("Listening…")
                                    } else {
                                        data.live_text.clone()
                                    })
                                    .wrap(true)
                                    .selectable(false),
                                );
                            });
                    });
                });
            }
        }
        crate::state::Status::Transcribing => {
            ui.label(RichText::new(tr("Transcribing…")).color(egui::Color32::from_rgb(230, 150, 40)));
        }
        crate::state::Status::File => {
            ui.label(
                RichText::new(tr("Transcribing a file…"))
                    .color(egui::Color32::from_rgb(70, 190, 90)),
            );
            ui.label(
                RichText::new(tr(
                    "The microphone stays unavailable until the file is done. Progress is on the File tab.",
                ))
                .weak(),
            );
        }
        crate::state::Status::Recovering => {
            ui.label(
                RichText::new(tr("Recovering a recording…"))
                    .color(egui::Color32::from_rgb(70, 130, 240)),
            );
            ui.label(
                RichText::new(tr(
                    "The previous recording was interrupted by a crash. It is being transcribed into the history; recording stays unavailable until it is done.",
                ))
                .weak(),
            );
        }
        _ => {
            let download_busy = data
                .download
                .as_ref()
                .map(|d| !d.done && d.error.is_none())
                .unwrap_or(false);
            let can_record = status != crate::state::Status::NoModel
                && status != crate::state::Status::Recovering
                && status != crate::state::Status::File
                && !download_busy;
            if !can_record {
                ui.label(RichText::new(tr("Recording is unavailable until a model is ready.")).weak());
            }
            if data.background_active() {
                ui.label(
                    RichText::new(tr("Background task in progress…"))
                        .color(egui::Color32::from_rgb(150, 110, 220)),
                );
            }
            if ui
                .add_enabled(can_record, egui::Button::new(tr("Start recording")))
                .clicked()
            {
                crate::ipc::write_record_request(&dirs.root, r#"{"kind":"user"}"#);
            }
        }
    }
    // A dictation and an imported file are the same intent — "turn this into
    // text" — so they live on one page. While a recording or a file is being
    // processed there is nothing to choose here.
    if matches!(
        status,
        crate::state::Status::Idle | crate::state::Status::NoModel | crate::state::Status::File
    ) {
        ui.separator();
        file_body(ui, data, dirs);
    }
}

/// Take the visible rows of `indices` (newest first) and their heights. Only
/// matching entries are laid out, so filtering keeps the virtualized list exact.
/// Height of a "day" separator in the history list.
const DAY_HEADER_HEIGHT: f32 = 30.0;

/// "Today", "Yesterday" or "Tuesday · 04.11" for a history timestamp.
fn day_header(at: &str) -> String {
    let date = at.get(0..10).unwrap_or(at);
    let today = chrono::Local::now().date_naive();
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok();
    match parsed {
        Some(day) if day == today => tr("Today"),
        Some(day) if Some(day) == today.pred_opt() => tr("Yesterday"),
        Some(day) => format!(
            "{} · {}",
            tr(weekday_name(chrono::Datelike::weekday(&day))),
            day.format("%d.%m")
        ),
        None => at.to_string(),
    }
}

fn weekday_name(day: chrono::Weekday) -> &'static str {
    use chrono::Weekday;
    match day {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

fn history_row_offsets(
    ui: &mut egui::Ui,
    history: &state::History,
    indices: &[usize],
    expanded: Option<usize>,
    starts_day: &[bool],
) -> Vec<f32> {
    let spacing = ui.spacing().item_spacing.y;
    let base_height = ui.spacing().interact_size.y + 2.0 * spacing + 2.0;
    let wrap_width = ui.available_width().max(32.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let mut offsets = Vec::with_capacity(indices.len() + 1);
    offsets.push(0.0);
    for (position, &index) in indices.iter().enumerate() {
        let mut height = base_height;
        if starts_day.get(position).copied().unwrap_or(false) {
            height += DAY_HEADER_HEIGHT;
        }
        if expanded == Some(index) {
            if let Ok(entry) = history.entry(index) {
                let text_height = ui.fonts(|f| {
                    f.layout(entry.text, font.clone(), color, wrap_width)
                        .size()
                        .y
                });
                height += text_height + spacing + 4.0;
                // Every appended piece adds its own row (copy button + time)
                // and its wrapped text below the full message.
                for part in &entry.parts {
                    let part_height = ui.fonts(|f| {
                        f.layout(part.text.clone(), font.clone(), color, wrap_width)
                            .size()
                            .y
                    });
                    height += ui.spacing().interact_size.y + part_height + 2.0 * spacing;
                }
                if !entry.parts.is_empty() {
                    height += spacing;
                }
            }
        }
        offsets.push(offsets[position] + height);
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
    let first_visible = offsets
        .partition_point(|&end| end < viewport_min)
        .min(rows - 1);
    let first = first_visible.saturating_sub(1);
    let mut last = first + 1;
    while last < rows && offsets[last] <= viewport_max {
        last += 1;
    }
    first..(last + 1).min(rows)
}

fn history_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    expanded: &mut Option<usize>,
    filter: &mut HistoryFilter,
) {
    ui.horizontal(|ui| {
        ui.heading(tr("History"));
        ui.add_space(10.0);
        ui.label(RichText::new(tr("show:")).weak());
        let mut next = *filter;
        ui.selectable_value(&mut next, HistoryFilter::All, tr("All"));
        ui.selectable_value(&mut next, HistoryFilter::User, tr("User"));
        ui.selectable_value(&mut next, HistoryFilter::Server, tr("Server"));
        ui.selectable_value(&mut next, HistoryFilter::File, tr("File"));
        ui.selectable_value(&mut next, HistoryFilter::Phone, tr("Phone"));
        *filter = next;
    });
    ui.separator();
    let mut indices: Vec<usize> = (0..data.history.len())
        .filter(|&index| {
            data.history
                .row(index)
                .map(|row| filter.matches(row.source()))
                .unwrap_or(false)
        })
        .collect();
    // History is a log, not a chat: the newest message is always the first
    // row. The order is by the time of the recording itself: the file is in the
    // order things were saved, but a phone message may arrive hours later and
    // must still sit in its own place.
    indices.sort_by(|left, right| {
        let left = data.history.row(*left).map(|row| row.at().to_string());
        let right = data.history.row(*right).map(|row| row.at().to_string());
        right.unwrap_or_default().cmp(&left.unwrap_or_default())
    });
    if indices.is_empty() {
        ui.label(match filter {
            HistoryFilter::All => "No transcriptions yet.",
            HistoryFilter::User => "No transcriptions dictated by the user yet.",
            HistoryFilter::Server => "No transcriptions started by the API server yet.",
            HistoryFilter::File => "No imported files yet.",
            HistoryFilter::Phone => "No messages from the phone yet.",
        });
        return;
    }

    let mut copied: Option<String> = None;
    let mut toggle: Option<usize> = None;
    // One header per day, newest day first, like the log it is.
    let dates: Vec<String> = indices
        .iter()
        .map(|&index| {
            data.history
                .row(index)
                .map(|row| row.at().to_string())
                .unwrap_or_default()
        })
        .collect();
    let starts_day: Vec<bool> = dates
        .iter()
        .enumerate()
        .map(|(position, date)| {
            position == 0 || dates[position - 1].get(0..10) != date.get(0..10)
        })
        .collect();
    let offsets = history_row_offsets(ui, &data.history, &indices, *expanded, &starts_day);
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
                for position in range {
                    let index = indices[position];
                    let Some(row) = data.history.row(index).cloned() else {
                        continue;
                    };
                    let is_open = *expanded == Some(index);
                    let full_entry = if is_open {
                        data.history.entry(index).ok()
                    } else {
                        None
                    };
                    let height = offsets[position + 1] - offsets[position];
                    let width = ui.available_width();
                    ui.push_id(index, |ui| {
                        ui.allocate_ui(egui::vec2(width, height), |ui| {
                            if starts_day.get(position).copied().unwrap_or(false) {
                                ui.separator();
                                ui.label(
                                    RichText::new(day_header(&dates[position]))
                                        .small()
                                        .strong()
                                        .color(ui.visuals().weak_text_color()),
                                );
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button(tr("Copy")).clicked() {
                                    match data.history.entry(index) {
                                        Ok(entry) => copied = Some(entry.text),
                                        Err(e) => crate::log::log(&format!(
                                            "failed to read history entry {index}: {e:#}"
                                        )),
                                    }
                                }
                                let pieces = if row.pieces() > 0 {
                                    trf(
                                        " · +{count} piece(s)",
                                        &[("count", &row.pieces().to_string())],
                                    )
                                } else {
                                    String::new()
                                };
                                ui.weak(trf(
                                    "{at} · {model}{pieces}",
                                    &[("at", row.at()), ("model", row.model()), ("pieces", &pieces)],
                                ));
                                if row.source() == state::Source::Server {
                                    ui.label(
                                        RichText::new(tr("server"))
                                            .small()
                                            .color(egui::Color32::from_rgb(230, 170, 60)),
                                    );
                                }
                                if row.source() == state::Source::File {
                                    ui.label(
                                        RichText::new(tr("file"))
                                            .small()
                                            .color(egui::Color32::from_rgb(70, 190, 90)),
                                    );
                                }
                                if row.source() == state::Source::Site {
                                    let text = match row.group() {
                                        Some(group) => format!("{} · {group}", tr("phone")),
                                        None => tr("phone").to_string(),
                                    };
                                    ui.label(
                                        RichText::new(text)
                                            .small()
                                            .color(egui::Color32::from_rgb(150, 110, 220)),
                                    );
                                }
                                if row.state().is_partial() {
                                    ui.label(
                                        RichText::new(tr("incomplete"))
                                            .small()
                                            .color(egui::Color32::from_rgb(90, 150, 250)),
                                    );
                                }
                                if ui.selectable_label(is_open, row.preview()).clicked() {
                                    toggle = Some(index);
                                }
                            });
                            if let Some(entry) = full_entry {
                                if let Some(audio) = entry.audio.as_deref() {
                                    let exists = std::path::Path::new(audio).exists();
                                    ui.horizontal(|ui| {
                                        if ui
                                            .add_enabled(exists, egui::Button::new(tr("Open recording")))
                                            .on_hover_text(audio)
                                            .clicked()
                                        {
                                            open_path(audio);
                                        }
                                        if ui
                                            .add_enabled(exists, egui::Button::new(tr("Show in folder")))
                                            .clicked()
                                        {
                                            show_in_folder(audio);
                                        }
                                    });
                                }
                                ui.add(egui::Label::new(entry.text).wrap(true));
                                ui.add_space(4.0);
                                // Pieces appended with the append hotkey are
                                // listed under the full message, each with its
                                // own time and copy button.
                                for (part_index, part) in entry.parts.iter().enumerate() {
                                    ui.push_id(part_index, |ui| {
                                        ui.add_space(2.0);
                                        ui.horizontal(|ui| {
                                            if ui.small_button(tr("Copy")).clicked() {
                                                copied = Some(part.text.clone());
                                            }
                                            ui.weak(trf("appended {at}", &[("at", &part.at)]));
                                        });
                                        ui.label(RichText::new(&part.text).weak());
                                    });
                                }
                            }
                            ui.separator();
                        });
                    });
                }
            });
        });
    if let Some(index) = toggle {
        *expanded = if *expanded == Some(index) {
            None
        } else {
            Some(index)
        };
    }
    if let Some(text) = copied {
        copy_to_clipboard(&text);
    }
}

/// About text: English lines are the keys, translated on each render so a
/// language switch applies without restarting.
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

fn about_lines() -> Vec<String> {
    ABOUT_LINES
        .iter()
        .map(|line| if line.is_empty() { String::new() } else { tr(line) })
        .collect()
}

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
                        if ui.button(tr("X")).clicked() {
                            close_clicked = true;
                        }
                    });
                });
                ui.separator();
                for line in about_lines() {
                    if line.is_empty() {
                        ui.add_space(4.0);
                    } else {
                        ui.label(line);
                    }
                }
                ui.hyperlink_to(
                    tr("GitHub: machine202502/voice-button"),
                    "https://github.com/machine202502/voice-button",
                );
                ui.hyperlink_to(
                    tr("MIT License"),
                    "https://github.com/machine202502/voice-button/blob/master/LICENSE",
                );
                ui.separator();
                if ui.button(tr("Open log folder")).clicked() {
                    let _ = std::process::Command::new("explorer")
                        .arg(&dirs.root)
                        .spawn();
                }
            });
        });
    if close || close_clicked {
        *open = false;
    }
}

fn apply_autostart(data: &mut Data) {
    if let Err(e) = autostart::set_enabled(data.config.silent_start, data.config.autostart) {
        data.last_error = Some(trf(
                "failed to update autostart: {error}",
                &[("error", &format!("{e:#}"))],
            ));
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
    use super::{day_header, message_time, selected_text, sync_config_from_disk, visible_history_range, weekday_name};
    use crate::config::{Backend, Config};
    use crate::state::{Shared, SiteMessage};
    use crate::site::monolog::Stage;
    use std::collections::BTreeSet;

    #[test]
    fn day_headers_read_like_a_chat() {
        let today = chrono::Local::now().date_naive();
        assert_eq!(
            day_header(&format!("{} 10:00:00", today.format("%Y-%m-%d"))),
            crate::i18n::tr("Today")
        );
        let yesterday = today.pred_opt().unwrap();
        assert_eq!(
            day_header(&format!("{} 10:00:00", yesterday.format("%Y-%m-%d"))),
            crate::i18n::tr("Yesterday")
        );
        let older = today - chrono::Days::new(10);
        let text = day_header(&format!("{} 09:00:00", older.format("%Y-%m-%d")));
        assert!(
            text.contains(&older.format("%d.%m").to_string()),
            "the date is there: {text}"
        );
        assert!(
            text.contains(&crate::i18n::tr(weekday_name(chrono::Datelike::weekday(&older)))),
            "and the weekday: {text}"
        );
        assert_eq!(day_header("nonsense"), "nonsense");
    }

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

    fn message(id: u64, text: &str) -> SiteMessage {
        SiteMessage {
            id,
            recorded: "2026-11-12T09:31:00+05:00".to_string(),
            stage: Stage::Done,
            text: text.to_string(),
        }
    }

    #[test]
    fn copied_selection_keeps_the_spoken_order_and_skips_empty_takes() {
        let messages = vec![
            message(1, "Первая мысль."),
            message(2, "   "),
            message(3, "Вторая мысль."),
            message(4, "Не выбрана."),
        ];
        let selected: BTreeSet<u64> = [1, 2, 3].into_iter().collect();
        assert_eq!(
            selected_text(&messages, &selected),
            "Первая мысль.\n\nВторая мысль."
        );
        assert_eq!(selected_text(&messages, &BTreeSet::new()), "");
    }

    #[test]
    fn a_recorded_stamp_becomes_a_readable_time() {
        assert_eq!(
            message_time("2026-11-12T09:31:00+05:00"),
            "2026-11-12 09:31"
        );
        assert_eq!(message_time(""), "");
    }
}
