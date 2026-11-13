use crate::autostart;
use crate::config::{self, AppDirs, Backend, PasteSeparator};
use crate::model_manager;
use crate::paging::Paged;
use crate::site::db::{MessageRow, MonologRow};
use crate::store::Store;
use crate::i18n::{tr, trf};
use crate::state::{self, Data, Shared};
use crate::theme::{self, Palette, Theme};
use arboard;
use eframe::egui;
use egui::{ProgressBar, RichText, ScrollArea};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const WINDOW_TITLE: &str = "Voice Button";

/// Tell Windows which mode the frame is in.
///
/// The title bar is drawn by the system, not by us, so switching the palette
/// alone leaves the caption in the old mode: dark mode paints white text, which
/// on the light theme is unreadable. DWMWA_USE_IMMERSIVE_DARK_MODE is the switch
/// the desktop honours.
fn apply_caption_theme(dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
    };
    let hwnd = find_own_window();
    if hwnd == 0 {
        return;
    }
    let value: i32 = i32::from(dark);
    // SAFETY: the handle belongs to this process (find_own_window filters by pid)
    // and the attribute expects a DWORD of this size.
    unsafe {
        DwmSetWindowAttribute(
            hwnd as *mut core::ffi::c_void,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            &value as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

/// The palette of the theme the window is painted with right now. `theme::apply`
/// has already put the theme into the visuals, so pages do not thread it around.
fn pal(ui: &egui::Ui) -> Palette {
    theme::palette(theme::active(ui.ctx()))
}

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
    Telegram,
}

/// Which transcriptions the history list shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HistoryFilter {
    All,
    User,
    Server,
    File,
    Phone,
    Telegram,
}

impl HistoryFilter {
    /// Sources to ask SQL for. Empty means every row.
    fn sources(self) -> Vec<state::Source> {
        match self {
            HistoryFilter::All => Vec::new(),
            HistoryFilter::User => vec![state::Source::User],
            HistoryFilter::Server => vec![state::Source::Server],
            HistoryFilter::File => vec![state::Source::File],
            HistoryFilter::Phone => vec![state::Source::Site],
            HistoryFilter::Telegram => vec![state::Source::Telegram],
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
        // GigaAM cannot detect the language itself, so the snapshot uses a real
        // code: "auto" here painted an amber warning across the Settings shot.
        cfg.language = "ru".to_string();
        // Committed snapshots must not depend on the config or the Windows
        // theme: the walk below forces dark and switches to light only for the
        // two review frames.
        cfg.theme = Theme::Dark;
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
        // The Phone tab lists what the daemon stores in the phone database. The
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
    // The window owns its colours now; the OS theme must not repaint it, and
    // the palette starts from the saved theme (dark when the file has none).
    let initial_theme = config.theme;
    let shared = Arc::new(Shared::new(config, history));
    let store = match Store::open(&dirs.root) {
        Ok(store) => Arc::new(store),
        Err(error) => {
            crate::log::log(&format!("ui: cannot open the database: {error:#}"));
            return;
        }
    };
    if let Some(pid) = current_parent_pid() {
        spawn_parent_watcher(pid);
    }
    spawn_file_poller(shared.clone(), dirs.clone(), store.clone());
    let initial_tab = if tab == "settings" {
        Tab::Settings
    } else if tab == "history" {
        Tab::History
    } else if tab == "api" {
        Tab::External
    } else if tab == "telegram" || tab == "phone" {
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
    // eframe follows the Windows theme by default and would overwrite the
    // palette on a system theme change; the app's own setting wins here.
    options.follow_system_theme = false;
    let snapshot_theme = std::env::var("VB_SNAPSHOT")
        .ok()
        .map(|_| Theme::Dark);
    let app = MainWindow {
        shared: shared.clone(),
        dirs: dirs.clone(),
        store,
        tab: initial_tab,
        external_tab: if tab == "telegram" {
            ExternalTab::Telegram
        } else if tab == "phone" {
            ExternalTab::Phone
        } else {
            ExternalTab::LocalServer
        },
        show_about: false,
        expanded_history: None,
        history_filter: HistoryFilter::All,
        history_search: String::new(),
        history_paged: Paged::new(),
        history_heights: crate::lazy_list::LazyList::new(0.0, 20.0),
        history_key: None,
        history_query: None,
        download_done_seen: false,
        api_copied_at: 0.0,
        site_qr: None,
        phone: PhoneState::default(),
        telegram_chat: Paged::new(),
        telegram_heights: crate::lazy_list::LazyList::new(8.0, 120.0),
        telegram_key: None,
        telegram_form: TelegramForm::default(),
        telegram_search: String::new(),
        telegram_range: crate::telegram::ChatRange::default(),
        snapshot_dir: std::env::var("VB_SNAPSHOT").ok().map(PathBuf::from),
        snapshot_frame: 0,
        applied_theme: Some(theme::resolve(initial_theme)),
        applied_setting: Some(initial_theme),
        theme_checked_at: None,
        snapshot_theme,
    };
    let creator: Box<dyn FnOnce(&eframe::CreationContext<'_>) -> Box<dyn eframe::App>> =
        Box::new(move |cc| {
            install_cjk_fonts(&cc.egui_ctx);
            let theme = theme::resolve(initial_theme);
            theme::apply(&cc.egui_ctx, theme);
            apply_caption_theme(matches!(theme, crate::theme::Theme::Dark));
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

/// Keep the in-memory state in sync with the daemon's SQLite runtime rows.
/// Config is also written by us, so it keeps its timestamp check; everything
/// else is a cheap key/value read.
fn spawn_file_poller(shared: Arc<Shared>, dirs: AppDirs, store: Arc<crate::store::Store>) {
    std::thread::spawn(move || {
        let config_path = dirs.root.join("config.json");
        let mut config_meta = meta_key(&std::fs::metadata(&config_path).ok());
        // A snapshot run paints illustrative states on purpose. After its first
        // read, a later daemon-file poll must not overwrite them, otherwise the
        // committed image depends on where the 300 ms poll happens to land.
        let snapshot = std::env::var("VB_SNAPSHOT").is_ok();
        let mut first_poll = true;
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let m = meta_key(&std::fs::metadata(&config_path).ok());
            if m != config_meta {
                config_meta = m;
                let cfg = config::load_config(&dirs.root);
                sync_config_from_disk(&mut shared.lock(), cfg);
            }
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
            {
                let mut data = shared.lock();
                if !snapshot || first_poll {
                    data.download = download;
                    if let Some(status) = crate::ipc::read_status(&dirs.root) {
                        data.status = status;
                    }
                    match crate::ipc::read_live(&dirs.root) {
                        Some(live) => {
                            data.live_text = live.text;
                            data.live_started_ms = live.started;
                        }
                        None => {
                            data.live_text.clear();
                            data.live_started_ms = 0;
                        }
                    }
                    data.api = crate::ipc::read_api_state(&dirs.root).map(Into::into);
                    data.site = crate::ipc::read_site_state(&dirs.root);
                    data.session_view = crate::ipc::read_session(&dirs.root).map(Into::into);
                    data.site_pairing = crate::site::db::client(&store).and_then(|client| {
                        if client.paired || client.token.is_empty() {
                            None
                        } else {
                            Some(state::SitePairing {
                                token: client.token,
                                until: client.expires,
                            })
                        }
                    });
                    data.site_network = crate::site::network::read(&dirs.root);
                    data.site_network_busy = crate::ipc::site_network_busy(&dirs.root);
                }
            }
            first_poll = false;
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
    search: String,
    selected: std::collections::BTreeSet<u64>,
    /// One page window over `monologs`, keyed by (revision, archive, search).
    monologs: Paged<MonologRow>,
    monolog_key: Option<(u64, bool, String)>,
    monolog_query: Option<(bool, String)>,
    /// One page window over the open monolog's messages, keyed by
    /// (revision, monolog id).
    messages: Paged<MessageRow>,
    message_key: Option<(u64, u64)>,
    /// Measured heights: a message is as tall as its text, a monolog as tall as
    /// its preview. Both lists are lazy with per-row heights.
    monolog_heights: crate::lazy_list::LazyList,
    message_heights: crate::lazy_list::LazyList,
}

/// Messages drawn per frame. A phone can collect thousands of takes; drawing
/// all of them would make the window crawl. The newest ones are shown first.

pub struct MainWindow {
    shared: Arc<Shared>,
    dirs: AppDirs,
    store: Arc<Store>,
    tab: Tab,
    external_tab: ExternalTab,
    show_about: bool,
    expanded_history: Option<usize>,
    history_filter: HistoryFilter,
    history_search: String,
    history_paged: Paged<crate::history::HistoryRow>,
    /// Measured heights of the history rows: an opened row is as tall as its text.
    history_heights: crate::lazy_list::LazyList,
    history_key: Option<(u64, HistoryFilter, String)>,
    history_query: Option<(HistoryFilter, String)>,
    download_done_seen: bool,
    /// egui timestamp of the last "Copy" click on the API address; drives the
    /// transient "Copied!" feedback in the API tab.
    api_copied_at: f64,
    /// The QR code of the current pairing link, cached by its text.
    site_qr: Option<(String, egui::TextureHandle)>,
    phone: PhoneState,
    telegram_chat: Paged<crate::telegram::ChatEntry>,
    /// Measured heights of the Telegram rows: they differ by message length.
    telegram_heights: crate::lazy_list::LazyList,
    telegram_key: Option<(u64, String, crate::telegram::ChatRange)>,
    telegram_form: TelegramForm,
    telegram_search: String,
    /// Today / week / everything: the log opens on today.
    telegram_range: crate::telegram::ChatRange,
    snapshot_dir: Option<PathBuf>,
    snapshot_frame: u32,
    /// Theme the visuals currently carry. A mismatch means Settings (or the
    /// config file) changed it and the palette must be applied again.
    applied_theme: Option<Theme>,
    /// The setting `applied_theme` was resolved from, `System` included.
    applied_setting: Option<Theme>,
    /// When the Windows setting was last read for `System`, so a live Windows
    /// light/dark switch is picked up without a registry read every frame.
    theme_checked_at: Option<Instant>,
    /// Snapshot mode only: overrides the config so committed images do not
    /// depend on it. `None` outside snapshot runs.
    snapshot_theme: Option<Theme>,
}

impl eframe::App for MainWindow {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the UI fresh (e.g. download progress) between events.
        ctx.request_repaint_after(Duration::from_millis(250));
        self.refresh_pages();
        if self.tab == Tab::External
            && self.external_tab == ExternalTab::Telegram
            && self
                .telegram_form
                .snapshot_at
                .map(|at| at.elapsed() > Duration::from_secs(1))
                .unwrap_or(true)
        {
            self.telegram_form.snapshot = Some(crate::telegram::snapshot(&self.dirs.root));
            self.telegram_form.snapshot_at = Some(Instant::now());
        }

        let mut data = self.shared.lock();
        // Settings, the config file and the snapshot walk all write the theme
        // into `config`/`snapshot_theme`. `System` is resolved here: right away
        // when the setting changes, and at most once a second otherwise, so a
        // Windows light/dark switch is followed without a WM_SETTINGCHANGE hook.
        let setting = self.snapshot_theme.unwrap_or(data.config.theme);
        let recheck = self.applied_setting != Some(setting)
            || setting != Theme::System
            || self.applied_theme.is_none()
            || self
                .theme_checked_at
                .map(|at| at.elapsed() >= Duration::from_secs(1))
                .unwrap_or(true);
        if recheck {
            self.applied_setting = Some(setting);
            self.theme_checked_at = Some(Instant::now());
            let wanted = theme::resolve(setting);
            if self.applied_theme != Some(wanted) {
                theme::apply(ctx, wanted);
                apply_caption_theme(matches!(wanted, crate::theme::Theme::Dark));
                self.applied_theme = Some(wanted);
            }
        }
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
                    &mut self.history_search,
                    &mut self.history_paged,
                    &mut self.history_heights,
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
                    &self.store,
                    ctx,
                    &mut self.api_copied_at,
                    &mut self.external_tab,
                    &mut self.site_qr,
                    &mut self.phone,
                    &mut self.telegram_chat,
                    &mut self.telegram_heights,
                    &mut self.telegram_range,
                    &mut self.telegram_form,
                    &mut self.telegram_search,
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
        let holiday_icons = data.config.holiday_icons;
        drop(data);

        if self.show_about {
            show_about_modal(ctx, &mut self.show_about, &self.dirs, holiday_icons);
        }

        self.drive_snapshot();
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // The window background behind the panels follows the theme, so a
        // resize never shows a dark strip under the light palette.
        theme::palette(self.applied_theme.unwrap_or(Theme::Dark))
            .bg
            .to_normalized_gamma_f32()
    }
}

impl MainWindow {
    /// Decide which pages the visible lists need. Runs every frame and is
    /// cheap: two revision reads and a count only when a query or the table
    /// actually changed.
    fn refresh_pages(&mut self) {
        let history_key = (
            self.store.revision("rev.history"),
            self.history_filter,
            self.history_search.trim().to_string(),
        );
        let history_query = (history_key.1, history_key.2.clone());
        if self.history_query.as_ref() != Some(&history_query) {
            self.expanded_history = None;
            self.history_query = Some(history_query);
        }
        if self.history_key.as_ref() != Some(&history_key) {
            let sources = self.history_filter.sources();
            let total = self.shared.lock().history.count(&sources, &history_key.2);
            self.history_paged.reset(total);
            self.history_key = Some(history_key);
        }

        let telegram_key = (
            self.store.revision("rev.telegram"),
            self.telegram_search.trim().to_string(),
            self.telegram_range,
        );
        if self.telegram_key.as_ref() != Some(&telegram_key) {
            let total = if telegram_key.1.is_empty() {
                crate::telegram::chat_count(&self.dirs, telegram_key.2)
            } else {
                crate::telegram::chat_search_count(&self.dirs, telegram_key.2, &telegram_key.1)
            };
            self.telegram_chat.reset(total);
            self.telegram_heights.reset();
            self.telegram_key = Some(telegram_key);
        }

        let monolog_key = (
            self.store.revision("rev.site"),
            self.phone.view == PhoneView::Archive,
            self.phone.search.trim().to_string(),
        );
        let monolog_query = (monolog_key.1, monolog_key.2.clone());
        if self.phone.monolog_query.as_ref() != Some(&monolog_query) {
            self.phone.monolog_query = Some(monolog_query);
            self.phone.open = None;
            self.phone.selected.clear();
        }
        if self.phone.monolog_key.as_ref() != Some(&monolog_key) {
            let total = crate::site::db::monolog_count(
                &self.store,
                monolog_key.1,
                &monolog_key.2,
            );
            self.phone.monologs.reset(total);
            self.phone.monolog_key = Some(monolog_key);
        }
    }

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
                // open it so the snapshot shows both the QR and the list.
                self.phone.open = Some(1);
                self.phone.selected.insert(2);
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
            // The seeded database holds one monolog with two messages; it is
            // open, so the shot shows the QR and the list.
            70 => {
                self.capture(&dir.join("phone.png"));
                // The dark set is complete. The rest is the same states in the
                // light theme: switch the palette and replay them, so both sets
                // come from the same deterministic data.
                self.snapshot_theme = Some(Theme::Light);
                // The picker in the light Settings shot must agree with the
                // paint. Nothing is saved: the walk never calls apply_settings.
                let mut data = self.shared.lock();
                data.config.theme = Theme::Light;
                data.download = None;
                data.status = crate::state::Status::Recording;
                data.live_text =
                    "Book a table for two at the Italian place on Saturday evening.".to_string();
                data.live_started_ms = chrono::Local::now().timestamp_millis() - 42_000;
                data.file = None;
                self.tab = Tab::Capture;
            }
            74 => self.capture(&dir.join("capture-recording-light.png")),
            75 => {
                let mut data = self.shared.lock();
                data.status = crate::state::Status::Idle;
                data.live_text.clear();
                data.live_started_ms = 0;
                self.tab = Tab::Settings;
            }
            77 => self.capture(&dir.join("settings-light.png")),
            78 => {
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
            80 => self.capture(&dir.join("api-light.png")),
            81 => {
                let mut data = self.shared.lock();
                data.config.api_enabled = false;
                data.api = None;
                self.tab = Tab::History;
            }
            84 => self.capture(&dir.join("history-light.png")),
            85 => self.expanded_history = Some(0),
            88 => self.capture(&dir.join("history-expanded-light.png")),
            89 => {
                self.expanded_history = None;
                self.tab = Tab::Settings;
                self.show_about = true;
            }
            92 => self.capture(&dir.join("about-light.png")),
            93 => {
                self.show_about = false;
                let mut data = self.shared.lock();
                data.status = crate::state::Status::Idle;
                data.live_text.clear();
                data.live_started_ms = 0;
                data.file = None;
                self.tab = Tab::Capture;
            }
            95 => self.capture(&dir.join("record-light.png")),
            96 => {
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
            99 => self.capture(&dir.join("download-progress-light.png")),
            100 => {
                let mut data = self.shared.lock();
                data.download = None;
                self.tab = Tab::External;
                self.external_tab = ExternalTab::Phone;
                self.phone.open = Some(1);
                self.phone.selected.clear();
                self.phone.selected.insert(2);
            }
            103 => self.capture(&dir.join("phone-light.png")),
            105 => std::process::exit(0),
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
    let p = pal(ui);
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
                .color(p.amber),
        );
    }
    ui.separator();
    // The model's input language belongs with the model, not at the bottom of
    // the page next to the shortcuts.
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
            .color(p.amber),
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
    }    ui.label(
        RichText::new(if data.config.swap_mouse_buttons {
            tr("Left click records and pastes at the cursor; middle click records a new message.")
        } else {
            tr("Left click records a new message; middle click records and pastes at the cursor.")
        })
        .weak(),
    );
    ui.separator();
    ui.heading(tr("Appearance"));
    // `accent` is the fill/icon colour and is too light for text; labels and
    // links use `accent_ink` through the style, never the palette's accent.
    ui.horizontal(|ui| {
        ui.label(tr("Theme:"));
        let mut theme = data.config.theme;
        let mut theme_changed = false;
        theme_changed |= ui
            .radio_value(&mut theme, Theme::Dark, tr(Theme::Dark.label()))
            .changed();
        theme_changed |= ui
            .radio_value(&mut theme, Theme::Light, tr(Theme::Light.label()))
            .changed();
        theme_changed |= ui
            .radio_value(&mut theme, Theme::System, tr(Theme::System.label()))
            .changed();
        if theme_changed {
            data.config.theme = theme;
            // Saved right away; the update loop resolves System and repaints
            // with the new palette on the next frame — nothing to restart.
            apply_settings(data, dirs);
        }
    });
    if ui
        .checkbox(&mut data.config.holiday_icons, tr("Holiday icons"))
        .changed()
    {
        apply_settings(data, dirs);
    }
    ui.label(
        RichText::new(tr(
            "Show the holiday microphone in the tray and on the site: a tree around New Year, the clock on the night itself, and the symbol of each holiday on its day. Recording and file processing keep their own colour.",
        ))
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
    if let Some(err) = &data.last_error {
        ui.label(
            RichText::new(trf("Error: {error}", &[("error", err)])).color(p.red),
        );
    }
}

#[derive(Default)]
struct TelegramForm {
    token: String,
    proxy: String,
    snapshot: Option<crate::telegram::Snapshot>,
    snapshot_at: Option<Instant>,
}

/// The job stage is stored as an English word; it is translated when drawn, so
/// the database keeps one stable value whatever the interface language is.
fn telegram_stage_label(stage: &str) -> String {
    match stage {
        "" | "queued" => tr("queued"),
        "downloading" => tr("downloading"),
        "transcribing" => tr("transcribing"),
        "engine busy" => tr("engine busy"),
        "done" => tr("done"),
        // A value this build does not know stays as it is instead of vanishing.
        other => other.to_string(),
    }
}

/// The media kind of a Telegram message, translated at display time only.
fn telegram_kind_label(kind: &str) -> String {
    match kind {
        "voice" => tr("Voice note"),
        "video_note" => tr("Video note"),
        "audio" => tr("Audio"),
        "video" => tr("Video"),
        "document" => tr("File"),
        other => other.to_string(),
    }
}

fn telegram_settings(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, form: &mut TelegramForm) {
    let p = pal(ui);
    ui.heading(tr("Telegram bot"));
    let mut commit = ui
        .checkbox(&mut data.config.telegram_enabled, tr("Receive files from Telegram"))
        .changed();
    ui.horizontal(|ui| {
        ui.label(tr("Proxy:"));
        let proxy = ui.add(
            egui::TextEdit::singleline(&mut data.config.telegram_proxy)
                .hint_text("http://127.0.0.1:10408"),
        );
        commit |= proxy.lost_focus();
    });
    if commit {
        apply_settings(data, dirs);
    }
    if form.proxy.is_empty() {
        form.proxy = data.config.telegram_proxy.clone();
    }
    ui.label(RichText::new(tr(
        "The token is stored for this Windows user only; it is never written to config.json.",
    )).weak().small());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut form.token)
                .password(true)
                .hint_text(tr("Bot token from @BotFather")),
        );
        if ui.button(tr("Save token")).clicked() {
            // The proxy field is exactly what is used; empty means direct.
            let proxy = data.config.telegram_proxy.clone();
            crate::ipc::request_telegram_token(&dirs.root, &form.token, &proxy);
            form.token.clear();
        }
    });
    if let Some(snapshot) = &form.snapshot {
        let owner = snapshot
            .owner
            .map(|id| id.to_string())
            .unwrap_or_else(|| tr("nobody yet — send /start to the bot").to_string());
        ui.label(trf("Owner: {owner}", &[("owner", &owner)]));
        ui.label(trf("In queue: {count}", &[("count", &snapshot.queued.to_string())]));
        if let Some(current) = &snapshot.current {
            let stage = telegram_stage_label(&current.stage);
            ui.label(trf(
                "Now: {file} · {stage}",
                &[("file", &current.file_name), ("stage", &stage)],
            ));
        }
        if let Some(error) = &snapshot.last_error {
            ui.label(RichText::new(error).color(p.red).small());
        }
        if ui.button(tr("Reset the bot owner")).clicked() {
            crate::ipc::request_telegram_reset(&dirs.root);
        }
    }
}

fn telegram_date_label(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("[%d.%m.%y %H:%M]")
                .to_string()
        })
        .unwrap_or_else(|_| value.get(11..16).unwrap_or("").to_string())
}

fn telegram_day_label(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%d.%m.%Y")
                .to_string()
        })
        .unwrap_or_else(|_| value.get(..10).unwrap_or(value).to_string())
}

fn telegram_body(
    ui: &mut egui::Ui,
    dirs: &AppDirs,
    chat: &mut Paged<crate::telegram::ChatEntry>,
    heights: &mut crate::lazy_list::LazyList,
    range: &mut crate::telegram::ChatRange,
    search: &mut String,
) {
    let p = pal(ui);
    ui.heading(tr("Telegram"));
    ui.label(
        RichText::new(tr(
            "Files sent to the bot and their transcripts. Read-only: Telegram keeps the originals.",
        ))
        .weak()
        .small(),
    );
    // The order is the bot's own: the first message it finished is at the top, the
    // last at the bottom. The date under a card is the date of the *original* file,
    // which can be years old for a forwarded note, so it is metadata only and never
    // reorders anything.
    // The filter row is always here, even when the list is empty: otherwise the
    // screen hid the only control that could explain the emptiness, and a person
    // whose messages are a week old saw "nothing from Telegram yet" with no way
    // to reach them.
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(search)
                .hint_text(tr("Search"))
                .desired_width(220.0),
        );
        // The log opens on today: the bot handled those messages now, and the
        // date on a card is the date of the original file, which may be older.
        ui.selectable_value(range, crate::telegram::ChatRange::Today, tr("Today"));
        ui.selectable_value(range, crate::telegram::ChatRange::Week, tr("Week"));
        ui.selectable_value(range, crate::telegram::ChatRange::All, tr("All time"));
        ui.label(
            RichText::new(trf("Messages: {count}", &[("count", &chat.total().to_string())]))
                .small()
                .weak(),
        );
    });
    if chat.total() == 0 {
        let searching = !search.trim().is_empty();
        ui.label(
            RichText::new(if searching {
                tr("No matching messages.")
            } else if *range == crate::telegram::ChatRange::Today {
                tr("Nothing today")
            } else if *range == crate::telegram::ChatRange::Week {
                tr("Nothing this week")
            } else {
                tr("Nothing from Telegram yet.")
            })
            .weak(),
        );
        if !searching && *range != crate::telegram::ChatRange::All
            && ui.button(tr("Show everything")).clicked()
        {
            *range = crate::telegram::ChatRange::All;
        }
        ui.separator();
        return;
    }
    ui.separator();
    // Proper lazy list: each row is drawn at its natural height and measured, so a
    // three-line message and a thirty-line one both fit, and nothing has to be
    // guessed. Only the rows inside the viewport (plus a couple as a margin) are
    // built; the pages they need come from SQLite on demand.
    let needle = search.trim().to_string();
    let total = chat.total();
    if heights.len() != total {
        heights.reset();
        heights.ensure(total);
    }
    let spacing = 8.0_f32;
    let mut changed = false;
    ScrollArea::vertical()
        .id_source("telegram_scroll")
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            let content_top = ui.max_rect().top();
            let row_width = ui.max_rect().width();
            ui.set_height(heights.total());
            let first = heights.index_at(viewport.min.y).saturating_sub(2);
            let last = (heights.index_at(viewport.max.y) + 3).min(total);
            if first >= last {
                return;
            }
            chat.ensure(first..last, |offset, limit| {
                if needle.is_empty() {
                    crate::telegram::chat_page(dirs, *range, limit, offset)
                } else {
                    crate::telegram::chat_search_page(dirs, *range, &needle, limit, offset)
                }
            });
            for index in first..last {
                let Some(entry) = chat.get(index) else {
                    continue;
                };
                let previous_day = index
                    .checked_sub(1)
                    .and_then(|previous| chat.get(previous))
                    .map(|previous| telegram_day_label(&previous.at));
                let top = content_top + heights.offset(index);
                // The cell is as wide as the list and as tall as the content wants
                // to be: the bottom is left open so nothing is squeezed.
                let rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left(), top),
                    egui::vec2(row_width, heights.total().max(top) + 4000.0),
                );
                let drawn = ui
                    .allocate_ui_at_rect(rect, |ui| {
                        if previous_day.as_deref() != Some(entry_day(entry).as_str()) {
                            ui.label(RichText::new(telegram_day_label(&entry.at)).small().weak());
                        }
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            telegram_card(ui, entry, p);
                        });
                    })
                    .response
                    .rect
                    .height();
                changed |= heights.set(index, drawn + spacing);
            }
        });
    if changed {
        ui.ctx().request_repaint();
    }
}

/// Characters shown before a message is folded. Chosen so a preview stays
/// readable in the narrow list and the button is worth pressing.
const LONG_TEXT_CHARS: usize = 320;

/// A message body that folds when it is long.
///
/// Tall rows are fine — the list measures them — but a telegram with five
/// paragraphs should not push every other message off the screen. The open/closed
/// state lives in the widget memory keyed by the message id, so it survives
/// scrolling and repaints but not a restart.
fn long_text(ui: &mut egui::Ui, id: egui::Id, body: &str) {
    let chars = body.chars().count();
    if chars <= LONG_TEXT_CHARS {
        ui.add(egui::Label::new(body).wrap(true).sense(egui::Sense::hover()));
        return;
    }
    let key = id.with("expanded");
    let expanded = ui.data(|data| data.get_temp::<bool>(key).unwrap_or(false));
    if expanded {
        ui.add(egui::Label::new(body).wrap(true).sense(egui::Sense::hover()));
        if ui.small_button(tr("Show less")).clicked() {
            ui.data_mut(|data| data.insert_temp(key, false));
        }
    } else {
        let mut preview: String = body.chars().take(LONG_TEXT_CHARS).collect();
        preview.push('…');
        ui.add(egui::Label::new(preview).wrap(true).sense(egui::Sense::hover()));
        if ui.small_button(tr("Show more")).clicked() {
            ui.data_mut(|data| data.insert_temp(key, true));
        }
    }
}

/// The day heading a row needs above it, if the previous row was another day.
fn entry_day(entry: &crate::telegram::ChatEntry) -> String {
    telegram_day_label(&entry.at)
}

/// One message card, at whatever height its text needs.
fn telegram_card(ui: &mut egui::Ui, entry: &crate::telegram::ChatEntry, p: Palette) {
    ui.set_width(ui.available_width());
    let clock = telegram_date_label(&entry.at);
    let sender = if entry.sender.trim().is_empty() {
        tr("User")
    } else {
        entry.sender.clone()
    };
    let seconds = if entry.seconds > 0.0 {
        format!(" · {:.0} {}", entry.seconds, tr("s"))
    } else {
        String::new()
    };
    ui.label(
        RichText::new(format!(
            "{} · {} · {}{}{}",
            clock,
            sender,
            telegram_kind_label(&entry.kind),
            if entry.file_name.is_empty() { "" } else { " · " },
            entry.file_name
        ))
        .size(12.5)
        .color(ui.visuals().text_color()),
    );
    let body = if entry.text.is_empty() { "—" } else { entry.text.as_str() };
    long_text(ui, egui::Id::new(("telegram_body", entry.message_id)), body);
    if let Some(error) = &entry.error {
        ui.label(RichText::new(error).color(p.red).small());
    }
    ui.horizontal(|ui| {
        if !entry.text.is_empty() && ui.small_button(tr("Copy")).clicked() {
            copy_to_clipboard(&entry.text);
        }
        ui.label(RichText::new(seconds).small().weak());
    });
}
/// "External access": the local server for scripts and pages, and the phone
/// that dictates into monologs.
#[allow(clippy::too_many_arguments)]
fn external_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    store: &Store,
    ctx: &egui::Context,
    copied_at: &mut f64,
    page: &mut ExternalTab,
    qr: &mut Option<(String, egui::TextureHandle)>,
    phone: &mut PhoneState,
    telegram_chat: &mut Paged<crate::telegram::ChatEntry>,
    telegram_heights: &mut crate::lazy_list::LazyList,
    telegram_range: &mut crate::telegram::ChatRange,
    telegram: &mut TelegramForm,
    telegram_search: &mut String,
) {
    ui.horizontal(|ui| {
        ui.selectable_value(page, ExternalTab::LocalServer, tr("Local server"));
        ui.selectable_value(page, ExternalTab::Phone, tr("Phone"));
        ui.selectable_value(page, ExternalTab::Telegram, tr("Telegram"));
    });
    ui.separator();
    match *page {
        ExternalTab::LocalServer => api_body(ui, data, dirs, ctx, copied_at),
        ExternalTab::Phone => phone_body(ui, data, dirs, store, ctx, copied_at, qr, phone),
        ExternalTab::Telegram => {
            source_model_picker(ui, data, dirs, SourceModel::Telegram);
            ui.separator();
            telegram_settings(ui, data, dirs, telegram);
            ui.separator();
            telegram_body(ui, dirs, telegram_chat, telegram_heights, telegram_range, telegram_search);
        }
    }
}

#[derive(Clone, Copy)]
enum SourceModel {
    Phone,
    Telegram,
}

fn source_model_picker(ui: &mut egui::Ui, data: &mut Data, dirs: &AppDirs, source: SourceModel) {
    let (label, id, current, folder) = match source {
        SourceModel::Phone => (
            tr("Phone model"),
            "phone_model",
            data.config.phone_model.clone(),
            data.config.phone_model_folder.clone(),
        ),
        SourceModel::Telegram => (
            tr("Telegram model"),
            "telegram_model",
            data.config.telegram_model.clone(),
            data.config.telegram_model_folder.clone(),
        ),
    };
    let mut chosen = current.clone();
    let selected = if current.is_empty() {
        tr("Same as Settings")
    } else if current == "custom" {
        folder
            .as_deref()
            .map(|path| format!("{}: {path}", tr("Custom folder...")))
            .unwrap_or_else(|| tr("Custom folder..."))
    } else {
        current.clone()
    };
    ui.label(RichText::new(label).small().weak());
    egui::ComboBox::from_id_source(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut chosen, String::new(), tr("Same as Settings"));
            for model in model_manager::MODELS {
                let marker = if model_manager::is_ready(&dirs.models, model.id) {
                    String::new()
                } else {
                    tr(" (not downloaded)")
                };
                ui.selectable_value(
                    &mut chosen,
                    model.id.to_string(),
                    format!("{} ({}){marker}", model.id, model.size),
                );
            }
            ui.selectable_value(&mut chosen, "custom".to_string(), tr("Custom folder..."));
        });
    if chosen != current || (chosen == "custom" && folder.is_none()) {
        let next_folder = if chosen == "custom" {
            let picked = rfd::FileDialog::new()
                .set_title(tr("Choose the model folder"))
                .pick_folder();
            let Some(path) = picked else {
                return;
            };
            Some(path.to_string_lossy().to_string())
        } else {
            None
        };
        match source {
            SourceModel::Phone => {
                data.config.phone_model = chosen;
                data.config.phone_model_folder = next_folder;
            }
            SourceModel::Telegram => {
                data.config.telegram_model = chosen;
                data.config.telegram_model_folder = next_folder;
            }
        }
        if let Err(error) = config::save_config(&dirs.root, &data.config) {
            crate::log::log(&format!("cannot save the source model: {error:#}"));
        }
    }
}

/// "Phone": switch the HTTPS server on, pair the one phone, show where the QR
/// code points, and list the monologs the phone has dictated.
fn phone_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    store: &Store,
    ctx: &egui::Context,
    copied_at: &mut f64,
    qr: &mut Option<(String, egui::TextureHandle)>,
    phone: &mut PhoneState,
) {
    ScrollArea::vertical()
        .id_source("phone_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            phone_settings(ui, data, dirs, store, ctx, copied_at, qr, phone);
        });
}

fn phone_settings(
    ui: &mut egui::Ui,
    data: &mut Data,
    dirs: &AppDirs,
    store: &Store,
    ctx: &egui::Context,
    copied_at: &mut f64,
    qr: &mut Option<(String, egui::TextureHandle)>,
    phone: &mut PhoneState,
) {
    let p = pal(ui);
    source_model_picker(ui, data, dirs, SourceModel::Phone);
    ui.separator();
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
                        .color(p.success),
                );
                if ui.button(tr("Copy")).clicked() {
                    copy_to_clipboard(&site.url);
                    *copied_at = ctx.input(|i| i.time);
                }
                if site.paired {
                    ui.label(
                        RichText::new(tr("The phone is connected"))
                            .color(p.success),
                    );
                } else {
                    ui.label(RichText::new(tr("No phone is connected")).weak());
                }
            });
            if *copied_at > 0.0 && ctx.input(|i| i.time) < *copied_at + 2.0 {
                ui.label(RichText::new(tr("Copied!")).color(p.success));
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
                    .color(p.amber),
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
                        p.amber,
                        tr("Setting up…"),
                    ),
                    (Some(network), _) if network.enabled && network.error.is_none() => (
                        p.success,
                        tr("Internet access is on"),
                    ),
                    (Some(network), _) if network.error.is_some() => (
                        p.red,
                        tr("Internet access failed"),
                    ),
                    _ => (p.muted, tr("Internet access is off")),
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
                                .color(p.red)
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
            ui.label(RichText::new(tr("Stopped")).color(p.muted));
            ui.label(
                RichText::new(tr("The phone server did not start. The log has the reason."))
                .weak()
                .small(),
            );
        }
        _ => {
            ui.label(RichText::new(tr("Stopped")).color(p.muted));
        }
    }

    // The monologs stay visible whether the server is running or not: they
    // belong to the person, not to the connection.
    ui.separator();
    phone_monologs(ui, store, phone, dirs);
}

/// The monologs the phone dictated, with the open one's messages below.
/// The preview line of a monolog: the newest message, nothing more.
fn phone_preview(monolog: &MonologRow) -> String {
    monolog.preview.chars().take(110).collect()
}

/// The newest message's clock, or the monolog's creation time.
fn phone_when(monolog: &MonologRow) -> String {
    let at = if monolog.last_at.is_empty() {
        &monolog.created
    } else {
        &monolog.last_at
    };
    at.replace('T', " ").chars().take(16).collect()
}

fn phone_monologs(
    ui: &mut egui::Ui,
    store: &Store,
    phone: &mut PhoneState,
    dirs: &AppDirs,
) {
    ui.horizontal(|ui| {
        ui.heading(tr("Monologs"));
        ui.add(
            egui::TextEdit::singleline(&mut phone.search)
                .hint_text(tr("Search"))
                .desired_width(180.0),
        );
    });
    let showing_archive = phone.view == PhoneView::Archive;
    let query = phone.search.trim().to_string();
    let total = phone.monologs.total();
    // Active and archived are two different jobs: read and archive the first,
    // only unarchive the second.
    ui.horizontal(|ui| {
        let mut view = phone.view;
        ui.selectable_value(&mut view, PhoneView::Active, tr("Active"));
        ui.selectable_value(&mut view, PhoneView::Archive, tr("Archive section"));
        if view != phone.view {
            phone.view = view;
            phone.open = None;
            phone.selected.clear();
        }
    });
    if total == 0 {
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
    // Open the newest monolog when nothing is open yet. The row itself is on
    // the first page, so this costs one query.
    if phone.open.is_none() {
        phone
            .monologs
            .ensure(0..1, |offset, limit| {
                crate::site::db::monolog_page(store, showing_archive, &query, offset, limit)
            });
        phone.open = phone.monologs.get(0).map(|monolog| monolog.id);
        phone.selected.clear();
    }
    let mut archive: Option<(u64, bool)> = None;
    let list_height = ui.available_height().max(320.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(350.0, list_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                // Lazy rows with measured heights: a two-line monolog name and a
                // long preview do not share a size, and forcing them to would
                // leave holes. Only the rows near the viewport are built, and only
                // the pages they need are read.
                if phone.monolog_heights.len() != total {
                    phone.monolog_heights.reset();
                    phone.monolog_heights.ensure(total);
                }
                let mut monolog_changed = false;
                ScrollArea::vertical()
                    .id_source("phone_monolog_list")
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, viewport| {
                        let content_top = ui.max_rect().top();
                        let row_width = ui.max_rect().width();
                        ui.set_height(phone.monolog_heights.total());
                        let first = phone.monolog_heights.index_at(viewport.min.y).saturating_sub(2);
                        let last = (phone.monolog_heights.index_at(viewport.max.y) + 3).min(total);
                        if first >= last {
                            return;
                        }
                        phone.monologs.ensure(first..last, |offset, limit| {
                            crate::site::db::monolog_page(
                                store,
                                showing_archive,
                                &query,
                                offset,
                                limit,
                            )
                        });
                        for index in first..last {
                            let Some(monolog) = phone.monologs.get(index) else {
                                continue;
                            };
                            let selected = phone.open == Some(monolog.id);
                            let preview = phone_preview(monolog);
                            let when = phone_when(monolog);
                            let text = format!(
                                "{}\n{} · {}\n{}",
                                monolog.name,
                                when,
                                trf("Messages: {count}", &[("count", &monolog.count.to_string())]),
                                preview
                            );
                            let top = content_top + phone.monolog_heights.offset(index);
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(ui.max_rect().left(), top),
                                egui::vec2(
                                    row_width,
                                    phone.monolog_heights.total().max(top) + 4000.0,
                                ),
                            );
                            let drawn = ui.allocate_ui_at_rect(rect, |ui| {
                                ui.add(
                                    egui::SelectableLabel::new(selected, RichText::new(text).size(14.0)),
                                )
                                .clicked()
                            });
                            monolog_changed |= phone.monolog_heights.set(index, drawn.response.rect.height() + 6.0);
                            if drawn.inner {
                                phone.open = Some(monolog.id);
                                phone.selected.clear();
                            }
                        }
                    });
                if monolog_changed {
                    ui.ctx().request_repaint();
                }
            },
        );
        ui.separator();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), list_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width(ui.available_width());
                let Some(selected) = phone
                    .open
                    .and_then(|id| crate::site::db::monolog_row(store, id))
                else {
                    ui.label(RichText::new(tr("No monologs yet.")).weak());
                    return;
                };
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} ({})", selected.name, selected.count)).strong(),
                    );
                    if ui
                        .small_button(if showing_archive {
                            tr("Unarchive")
                        } else {
                            tr("Archive")
                        })
                        .clicked()
                    {
                        archive = Some((selected.id, !showing_archive));
                    }
                });
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        phone_when(&selected),
                        phone_preview(&selected)
                    ))
                    .small()
                    .weak(),
                );
                ui.separator();
                phone_messages(ui, store, &selected, phone);
            },
        );
    });
    if let Some((monolog, archived)) = archive {
        crate::ipc::request_site_archive(&dirs.root, monolog, archived);
    }
}

/// Messages of one open monolog: selection, copy, and the takes themselves.
/// The list is one endless scroll; only the pages around the viewport are read.
fn phone_messages(
    ui: &mut egui::Ui,
    store: &Store,
    monolog: &MonologRow,
    phone: &mut PhoneState,
) {
    let total = crate::site::db::message_count(store, monolog.id);
    if total == 0 {
        ui.label(RichText::new(tr("No messages from the phone yet.")).weak());
        return;
    }
    let key = (store.revision("rev.site"), monolog.id);
    if phone.message_key.as_ref() != Some(&key) {
        phone.messages.reset(total);
        phone.message_key = Some(key);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.small_button(tr("Select all")).clicked() {
            for id in crate::site::db::message_ids(store, monolog.id) {
                phone.selected.insert(id);
            }
        }
        if ui.small_button(tr("Clear selection")).clicked() {
            phone.selected.clear();
        }
        let count = phone.selected.len();
        let copy = ui.add_enabled(
            count > 0,
            egui::Button::new(trf(
                "Copy selected ({count})",
                &[("count", &count.to_string())],
            )),
        );
        if copy.clicked() {
            let ids: Vec<u64> = phone.selected.iter().copied().collect();
            let text = crate::site::db::message_texts(store, &ids)
                .into_iter()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !text.is_empty() {
                copy_to_clipboard(&text);
            }
        }
    });
    ui.add_space(2.0);
    // Like the phone's own chat: oldest at the top, newest at the bottom, one
    // header per day. Every row keeps its own height — a message is as tall as
    // its text — and the list builds only the rows inside the viewport.
    if phone.message_heights.len() != total {
        phone.message_heights.reset();
        phone.message_heights.ensure(total);
    }
    let mut changed = false;
    ScrollArea::vertical()
        .id_source(("phone_messages_scroll", monolog.id))
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            let content_top = ui.max_rect().top();
            let row_width = ui.max_rect().width();
            ui.set_height(phone.message_heights.total());
            let first = phone.message_heights.index_at(viewport.min.y).saturating_sub(2);
            let last = (phone.message_heights.index_at(viewport.max.y) + 3).min(total);
            if first >= last {
                return;
            }
            phone.messages.ensure(first..last, |offset, limit| {
                crate::site::db::message_page(store, monolog.id, offset, limit)
            });
            for index in first..last {
                let Some(message) = phone.messages.get(index) else {
                    continue;
                };
                let day = message.recorded.get(0..10).unwrap_or_default().to_string();
                let previous_day = index
                    .checked_sub(1)
                    .and_then(|previous| phone.messages.get(previous))
                    .and_then(|previous| previous.recorded.get(0..10));
                let top = content_top + phone.message_heights.offset(index);
                let rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left(), top),
                    egui::vec2(
                        row_width,
                        phone.message_heights.total().max(top) + 4000.0,
                    ),
                );
                let drawn = ui
                    .allocate_ui_at_rect(rect, |ui| {
                        if previous_day != Some(day.as_str()) {
                            ui.label(
                                RichText::new(day_header(&message.recorded))
                                    .small()
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
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
                                ui.label(
                                    RichText::new(message_time(&message.recorded))
                                        .small()
                                        .weak(),
                                );
                                ui.label(
                                    RichText::new(stage_label(message.stage))
                                        .small()
                                        .weak(),
                                );
                            });
                            if message.text.trim().is_empty() {
                                ui.label(RichText::new("…").weak());
                            } else {
                                long_text(ui, egui::Id::new(("phone_body", message.id)), &message.text);
                            }
                        });
                    })
                    .response
                    .rect
                    .height();
                changed |= phone.message_heights.set(index, drawn + 6.0);
            }
        });
    if changed {
        ui.ctx().request_repaint();
    }
}

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
    let p = theme::palette(theme::active(ctx));
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let quiet = 2usize;
    let scale = 4usize;
    let side = (width + quiet * 2) * scale;
    let colors = code.to_colors();
    let mut image = egui::ColorImage::new([side, side], p.qr_bg);
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] != qrcode::Color::Dark {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = (x + quiet) * scale + dx;
                    let py = (y + quiet) * scale + dy;
                    image[(px, py)] = p.qr_fg;
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
    let p = pal(ui);
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
            .color(p.amber),
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
                    .color(p.success),
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
            ui.label(RichText::new(tr("Copied!")).color(p.success));
        }
    } else if data.config.api_enabled {
        match data.api.as_ref().and_then(|s| s.error.clone()) {
            Some(error) => ui.label(
                RichText::new(trf("Not listening: {error}", &[("error", &error)]))
                    .color(p.red),
            ),
            None => ui.label(RichText::new(tr("Starting…")).color(p.muted)),
        };
    } else {
        ui.label(RichText::new(tr("Stopped")).color(p.muted));
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
    let _ = crate::ipc::clear_hotkey_result(&dirs.root);
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
    let p = pal(ui);
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
                    .color(p.amber),
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
    let p = pal(ui);
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
                        .color(p.red),
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
    let p = pal(ui);
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
            ui.label(RichText::new(tr("Model is ready.")).color(p.success));
        }
        download_section(ui, data, dirs, &id);
    } else {
        match data.config.whisper_file.clone() {
            Some(file) => {
                ui.monospace(&file);
                if !PathBuf::from(&file).exists() {
                    ui.label(
                        RichText::new(tr("File does not exist."))
                            .color(p.amber),
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
    let p = pal(ui);
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
            ui.label(RichText::new(tr("Model is ready.")).color(p.success));
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
                            .color(p.success),
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
                            .color(p.amber),
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
    let p = pal(ui);
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
        ui.label(RichText::new(error).color(p.red));
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
    let p = pal(ui);
    let status = data.status;
    match status {
        crate::state::Status::Recording => {
            if ui
                .button(RichText::new(tr("Stop recording")).color(p.red))
                .clicked()
            {
                crate::ipc::write_record_request(&dirs.root, "user");
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
                        .color(p.amber),
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
                            .color(p.red),
                    );
                } else {
                    ui.label(RichText::new(tr("REC")).color(p.red));
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
            ui.label(RichText::new(tr("Transcribing…")).color(p.amber));
        }
        crate::state::Status::File => {
            ui.label(
                RichText::new(tr("Transcribing a file…"))
                    .color(p.success),
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
                    .color(p.info),
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
                        .color(p.violet),
                );
            }
            if ui
                .add_enabled(can_record, egui::Button::new(tr("Start recording")))
                .clicked()
            {
                crate::ipc::write_record_request(&dirs.root, "user");
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

fn history_entry_for(
    history: &state::History,
    row: &crate::history::HistoryRow,
    index: usize,
) -> Option<state::HistoryEntry> {
    if row.id() == 0 {
        history.entry(index).ok()
    } else {
        history.entry_by_id(row.id()).ok()
    }
}

fn history_body(
    ui: &mut egui::Ui,
    data: &mut Data,
    expanded: &mut Option<usize>,
    filter: &mut HistoryFilter,
    search: &mut String,
    paged: &mut Paged<crate::history::HistoryRow>,
    heights: &mut crate::lazy_list::LazyList,
) {
    let p = pal(ui);
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
        ui.selectable_value(&mut next, HistoryFilter::Telegram, tr("Telegram"));
        if next != *filter {
            *filter = next;
            *expanded = None;
        }
        ui.add(
            egui::TextEdit::singleline(search)
                .hint_text(tr("Search"))
                .desired_width(200.0),
        );
    });
    ui.separator();
    if paged.is_empty() {
        ui.label(match *filter {
            HistoryFilter::All => tr("No transcriptions yet."),
            HistoryFilter::User => tr("No transcriptions dictated by the user yet."),
            HistoryFilter::Server => tr("No transcriptions started by the API server yet."),
            HistoryFilter::File => tr("No imported files yet."),
            HistoryFilter::Phone => tr("No messages from the phone yet."),
            HistoryFilter::Telegram => tr("No Telegram messages yet."),
        });
        return;
    }

    let sources = filter.sources();
    let needle = search.trim().to_string();
    // The open entry is measured once; its extra height shifts the rows below
    // it. Everything else has the same height, so no row is ever measured just
    // to draw the scrollbar.
    if let Some(open) = *expanded {
        paged.ensure(open..(open + 1).min(paged.total()), |offset, limit| {
            data.history.page(&sources, &needle, offset, limit)
        });
    }
    let total = paged.total();
    // Rows are measured like every other lazy list here: a collapsed row takes one
    // line, an opened one takes as much as its transcript needs, and nothing is
    // pre-computed with a guessed height.
    if heights.len() != total {
        heights.reset();
        heights.ensure(total);
    }
    let mut changed = false;

    let mut copied: Option<String> = None;
    let mut toggle: Option<usize> = None;
    ScrollArea::vertical()
        .id_source("history_rows")
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            ui.set_height(heights.total());
            let first = heights.index_at(viewport.min.y).saturating_sub(2);
            let last = (heights.index_at(viewport.max.y) + 3).min(total);
            if first >= last {
                return;
            }
            paged.ensure(first..last, |offset, limit| {
                data.history.page(&sources, &needle, offset, limit)
            });
            let content_top = ui.max_rect().top();
            for index in first..last {
                let Some(row) = paged.get(index) else {
                    continue;
                };
                let is_open = *expanded == Some(index);
                let top = content_top + heights.offset(index);
                let rect = egui::Rect::from_min_size(
                    egui::pos2(ui.max_rect().left(), top),
                    egui::vec2(ui.max_rect().width(), heights.total().max(top) + 4000.0),
                );
                let starts_day = index == 0
                    || paged
                        .get(index - 1)
                        .map(|previous| previous.at().get(0..10) != row.at().get(0..10))
                        .unwrap_or(true);
                let full_entry = if is_open {
                    history_entry_for(&data.history, row, index)
                } else {
                    None
                };
                let drawn = ui
                    .allocate_ui_at_rect(rect, |ui| {
                    ui.set_width(ui.available_width());
                    ui.push_id(index, |ui| {
                        ui.horizontal(|ui| {
                            if ui.small_button(tr("Copy")).clicked() {
                                match history_entry_for(&data.history, row, index) {
                                    Some(entry) => copied = Some(entry.text),
                                    None => crate::log::log(&format!(
                                        "failed to read history entry {index}"
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
                            // The day divider used to sit above the line and
                            // cost a whole block per entry; it reads better on
                            // the line itself, next to the time.
                            let at = if starts_day {
                                format!("{} · {}", day_header(row.at()), row.at())
                            } else {
                                row.at().to_string()
                            };
                            ui.weak(trf(
                                "{at} · {model}{pieces}",
                                &[
                                    ("at", &at),
                                    // An empty model column reads as "unknown
                                    // model"; that is a phrase, not an id.
                                    ("model", &tr(row.model())),
                                    ("pieces", &pieces),
                                ],
                            ));
                            if row.source() == state::Source::Server {
                                ui.label(
                                    RichText::new(tr("server"))
                                        .small()
                                        .color(p.amber),
                                );
                            }
                            if row.source() == state::Source::File {
                                ui.label(
                                    RichText::new(tr("file"))
                                        .small()
                                        .color(p.success),
                                );
                            }
                            if row.source() == state::Source::Telegram {
                                ui.label(
                                    RichText::new(tr("Telegram"))
                                        .small()
                                        .color(p.info),
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
                                        .color(p.violet),
                                );
                            }
                            if row.state().is_partial() {
                                ui.label(
                                    RichText::new(tr("incomplete"))
                                        .small()
                                        .color(p.info),
                                );
                            }
                            if ui.selectable_label(is_open, row.preview()).clicked() {
                                toggle = Some(index);
                            }
                        });
                        if let Some(entry) = &full_entry {
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
                            ui.add(egui::Label::new(entry.text.clone()).wrap(true));
                            ui.add_space(4.0);
                            // Pieces appended with the append hotkey are listed
                            // under the full message, each with its own time and
                            // copy button.
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
                        // A hairline keeps the rows readable without the
                        // vertical padding a separator widget adds.
                        let bottom = ui.max_rect().bottom() - 1.0;
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            bottom,
                            egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color().gamma_multiply(0.25)),
                        );
                    });
                    })
                    .response
                    .rect
                    .height();
                changed |= heights.set(index, drawn);
            }
        });
    if changed {
        ui.ctx().request_repaint();
    }
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

/// The microphone in the About title: the holiday head when one is showing, the
/// brand green on the tile and the stand, exactly like the tray's idle icon.
/// Its colour comes from the palette, so the light theme does not show a lime
/// glyph on white.
fn about_mic(ctx: &egui::Context, holiday_icons: bool) -> Option<egui::TextureHandle> {
    type Cached = (Option<String>, Theme, egui::TextureHandle);
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<Cached>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(None));
    let theme = theme::active(ctx);
    let accent = theme::palette(theme).accent;
    // The window runs in its own process, so the daemon's published holiday is
    // not visible here: it is worked out from the same date and language. UI
    // snapshots are committed files and must not carry a holiday, so the
    // snapshot mode always shows the plain microphone.
    let holiday = if holiday_icons && std::env::var("VB_SNAPSHOT").is_err() {
        crate::holidays::active(
            chrono::Local::now().date_naive(),
            &crate::i18n::active_code(),
        )
        .map(|id| id.to_string())
    } else {
        None
    };
    let mut guard = cache.lock().unwrap();
    if let Some((cached, cached_theme, texture)) = guard.as_ref() {
        if *cached == holiday && *cached_theme == theme {
            return Some(texture.clone());
        }
    }
    let svg = crate::icons::tray_svg(holiday.as_deref(), [accent.r(), accent.g(), accent.b()]);
    let (rgba, width, height) = crate::icons::render_rgba(&svg, 64)?;
    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
    let texture = ctx.load_texture("about-mic", image, egui::TextureOptions::LINEAR);
    *guard = Some((holiday, theme, texture.clone()));
    Some(texture)
}

fn show_about_modal(
    ctx: &egui::Context,
    open: &mut bool,
    dirs: &AppDirs,
    holiday_icons: bool,
) {
    let screen = ctx.screen_rect();
    let p = theme::palette(theme::active(ctx));
    let mut close = false;
    let mut close_clicked = false;
    // Dim overlay that swallows clicks, so the UI behind the modal is blocked.
    egui::Area::new(egui::Id::new("about_overlay"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let response = ui.allocate_response(screen.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, p.overlay);
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
                    // The microphone sits in front of the name, as tall as the
                    // heading's letters, and follows the holiday.
                    if let Some(texture) = about_mic(ctx, holiday_icons) {
                        let side = ui.text_style_height(&egui::TextStyle::Heading);
                        ui.add(egui::Image::new((texture.id(), egui::vec2(side, side))));
                    }
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
    use super::{day_header, message_time, sync_config_from_disk, weekday_name};
    use crate::config::{Backend, Config};
    use crate::state::Shared;
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
    fn a_recorded_stamp_becomes_a_readable_time() {
        assert_eq!(
            message_time("2026-11-12T09:31:00+05:00"),
            "2026-11-12 09:31"
        );
        assert_eq!(message_time(""), "");
    }
}
