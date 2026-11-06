use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Whisper,
    #[serde(alias = "GigaAm")]
    Onnx,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub backend: Backend,
    #[serde(default)]
    pub whisper_model: String,
    #[serde(default)]
    pub whisper_file: Option<String>,
    #[serde(default)]
    pub onnx_model: String,
    #[serde(default)]
    pub onnx_folder: Option<String>,
    pub language: String,
    /// Preferred microphone name; empty uses the Windows default device.
    /// Virtual cables (VB-Audio, streaming devices) record silence unless
    /// something is routed into them, so the microphone is selectable.
    #[serde(default)]
    pub input_device: String,
    /// Swap the tray icon's left and middle mouse actions.
    #[serde(default)]
    pub swap_mouse_buttons: bool,
    #[serde(default = "default_true")]
    pub autostart: bool,
    #[serde(default = "default_true")]
    pub silent_start: bool,
    #[serde(default = "default_sound_volume")]
    pub sound_volume: f32,
    #[serde(default = "default_true")]
    pub play_start_sound: bool,
    #[serde(default = "default_true")]
    pub play_end_sound: bool,
    /// Global start/stop shortcut, e.g. "Ctrl+Alt+R". Empty (the default)
    /// means the shortcut is not bound.
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    /// Global append shortcut: transcribes a piece and appends it to the last
    /// history entry, then copies the full merged message. Empty disables it.
    #[serde(default)]
    pub append_hotkey: String,
    /// Global shortcut that records a new message, saves it to history and
    /// pastes the text at the caret of the window that was active when the
    /// shortcut was pressed. The previous clipboard content is restored after
    /// the paste. Empty disables it.
    #[serde(default)]
    pub paste_hotkey: String,
}

/// Shortcuts start unbound: the user picks them in Settings.
fn default_hotkey() -> String {
    String::new()
}

fn default_sound_volume() -> f32 {
    0.5
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            backend: Backend::Whisper,
            whisper_model: "large-v3-turbo-q5_0".to_string(),
            whisper_file: None,
            onnx_model: "gigaam-v3-sherpa-int8".to_string(),
            onnx_folder: None,
            language: "auto".to_string(),
            input_device: String::new(),
            swap_mouse_buttons: false,
            autostart: true,
            silent_start: true,
            play_start_sound: true,
            play_end_sound: true,
            sound_volume: 0.5,
            hotkey: String::new(),
            append_hotkey: String::new(),
            paste_hotkey: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppDirs {
    pub root: PathBuf,
    pub models: PathBuf,
}

pub fn app_dirs() -> AppDirs {
    // Snapshot test runs must not touch the real data directory.
    let root = if std::env::var("VB_SNAPSHOT").is_ok() {
        std::env::temp_dir().join("voice-button-snapshot")
    } else {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("voice-button")
    };
    let models = root.join("models");
    let _ = fs::create_dir_all(&models);
    // Snapshot runs render the Settings page; give the default ONNX entry the
    // marker files so the screenshots show a downloaded model instead of a
    // "download" button. Nothing is read from them.
    if std::env::var("VB_SNAPSHOT").is_ok() {
        let folder = models.join("gigaam-v3-sherpa-int8");
        let _ = fs::create_dir_all(&folder);
        for name in ["model.onnx", "vocab.txt"] {
            let _ = fs::write(folder.join(name), b"");
        }
    }
    AppDirs { root, models }
}

pub fn save_config(root: &std::path::Path, cfg: &Config) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(cfg)?;
    crate::ipc::atomic_write(root.join("config.json"), text.as_bytes());
    Ok(())
}

pub fn load_config(root: &std::path::Path) -> Config {
    let mut config = fs::read_to_string(root.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Config>(&s).ok())
        .unwrap_or_default();
    // Retired catalog ids (istupakov exports and older folders) map to the
    // sherpa-onnx conversions this build actually ships.
    config.onnx_model = match config.onnx_model.as_str() {
        "gigaam-v3-ctc-int8"
        | "gigaam-v3-ctc"
        | "v3_ctc"
        | "multi-gigaam-ctc"
        | "multi-gigaam-large-ctc" => "gigaam-v3-sherpa-int8",
        "gigaam-v3-e2e-rnnt-int8" | "v3_e2e_rnnt" => "gigaam-v3-rnnt-punct",
        "v3_e2e_ctc" => "gigaam-v3-ctc-punct",
        "v3_rnnt" => "gigaam-v3-rnnt",
        other => other,
    }
    .to_string();
    config
}
