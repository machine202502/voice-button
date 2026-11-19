//! App updates through Velopack.
//!
//! The checker and downloader run on their own thread. The egui window polls
//! `state()` every frame, so it needs no channel or repaint handle.

use crate::log;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use velopack::sources::HttpSource;
use velopack::{UpdateCheck, UpdateInfo, UpdateManager, VelopackApp};

const UPDATE_FEED: &str = "https://machine202502.github.io/voice-button/updates";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading(u8),
    Installing,
    UpToDate,
    Error,
    Unsupported,
}

fn state_cell() -> &'static Mutex<UpdateState> {
    static STATE: OnceLock<Mutex<UpdateState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(UpdateState::Idle))
}

fn available() -> &'static Mutex<Option<UpdateInfo>> {
    static AVAILABLE: OnceLock<Mutex<Option<UpdateInfo>>> = OnceLock::new();
    AVAILABLE.get_or_init(|| Mutex::new(None))
}

fn busy() -> &'static AtomicBool {
    static BUSY: AtomicBool = AtomicBool::new(false);
    &BUSY
}

fn test_mode() -> bool {
    std::env::var_os("VB_SNAPSHOT").is_some() || std::env::var_os("VB_DATA_DIR").is_some()
}

fn feed() -> String {
    std::env::var("VOICE_BUTTON_UPDATE_FEED")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| UPDATE_FEED.to_string())
}

fn manager() -> Result<UpdateManager, velopack::Error> {
    UpdateManager::new(HttpSource::new(feed()), None, None)
}

fn set_state(state: UpdateState) {
    *state_cell()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = state;
}

pub fn state() -> UpdateState {
    *state_cell()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn take_available() -> Option<UpdateInfo> {
    available()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

fn set_available(info: Option<UpdateInfo>) {
    *available()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = info;
}

enum Checked {
    Update(UpdateInfo),
    Current,
    Unsupported,
}

/// Called when About opens. A running check or a found update is left alone.
pub fn check() {
    if test_mode() {
        set_state(UpdateState::Idle);
        return;
    }
    if busy().swap(true, Ordering::SeqCst) {
        return;
    }
    if take_available().is_some() {
        busy().store(false, Ordering::SeqCst);
        return;
    }

    set_state(UpdateState::Checking);
    std::thread::spawn(move || {
        let result = (|| -> Result<Checked, String> {
            let manager = match manager() {
                Ok(manager) => manager,
                Err(velopack::Error::NotInstalled(_)) | Err(velopack::Error::NotSupported(_)) => {
                    return Ok(Checked::Unsupported);
                }
                Err(error) => return Err(error.to_string()),
            };
            if manager.get_is_portable() {
                return Ok(Checked::Unsupported);
            }
            match manager
                .check_for_updates()
                .map_err(|error| error.to_string())?
            {
                UpdateCheck::UpdateAvailable(info) => Ok(Checked::Update(*info)),
                UpdateCheck::NoUpdateAvailable | UpdateCheck::RemoteIsEmpty => Ok(Checked::Current),
            }
        })();

        busy().store(false, Ordering::SeqCst);
        match result {
            Ok(Checked::Update(info)) => {
                let version = info.TargetFullRelease.Version.clone();
                log::log(&format!("update available: {version}"));
                set_available(Some(info));
                set_state(UpdateState::Available);
            }
            Ok(Checked::Current) => {
                log::log("no update available");
                set_available(None);
                set_state(UpdateState::UpToDate);
            }
            Ok(Checked::Unsupported) => {
                log::log("updates are not supported for this installation");
                set_available(None);
                set_state(UpdateState::Unsupported);
            }
            Err(error) => {
                set_available(None);
                log::log(&format!("update check failed: {error}"));
                set_state(UpdateState::Error);
            }
        }
    });
}

/// Called by the green About button. Downloads the check's update, then lets
/// Velopack replace the installation and restart the daemon.
pub fn install() {
    if busy().swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(info) = take_available() else {
        busy().store(false, Ordering::SeqCst);
        check();
        return;
    };

    let version = info.TargetFullRelease.Version.clone();
    set_state(UpdateState::Downloading(0));
    std::thread::spawn(move || {
        let result = (|| -> Result<UpdateManager, String> {
            let manager = manager().map_err(|error| error.to_string())?;
            let (progress_tx, progress_rx) = mpsc::channel::<i16>();
            let progress_thread = std::thread::spawn(move || {
                while let Ok(percent) = progress_rx.recv() {
                    set_state(UpdateState::Downloading(percent.clamp(0, 100) as u8));
                }
            });

            let download = manager
                .download_updates(&info, Some(progress_tx))
                .map_err(|error| error.to_string());
            let _ = progress_thread.join();
            download?;
            Ok(manager)
        })();

        match result {
            Ok(manager) => {
                log::log(&format!("update {version} downloaded; restarting"));
                set_state(UpdateState::Installing);
                if let Err(error) = manager.apply_updates_and_restart(&info) {
                    log::log(&format!("update install failed: {error}"));
                    busy().store(false, Ordering::SeqCst);
                    set_state(UpdateState::Error);
                }
            }
            Err(error) => {
                log::log(&format!("update download failed: {error}"));
                busy().store(false, Ordering::SeqCst);
                set_state(UpdateState::Error);
            }
        }
    });
}

/// Velopack handles uninstall/update arguments before the daemon starts.
pub fn init() {
    if !test_mode() {
        VelopackApp::build().run();
    }
}
