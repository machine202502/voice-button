use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Backend {
    Whisper,
    #[serde(alias = "GigaAm")]
    Onnx,
}

impl Backend {
    /// Engine name used by the public API, e.g. in `Onnx/gigaam-v3-rnnt`.
    pub fn label(self) -> &'static str {
        match self {
            Backend::Whisper => "Whisper",
            Backend::Onnx => "Onnx",
        }
    }
}

/// What record-and-paste appends after the inserted text so the next dictation
/// does not run into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PasteSeparator {
    /// A single space (the default).
    #[default]
    Space,
    /// A line break, so the next dictation starts on a new line.
    Newline,
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
    /// Multiplier for the voice-activity threshold. 1.0 is the default; higher
    /// values treat quieter speech as speech, which helps a quiet microphone.
    #[serde(default = "default_vad_sensitivity")]
    pub vad_sensitivity: f32,
    /// Swap the tray icon's left and middle mouse actions.
    #[serde(default)]
    pub swap_mouse_buttons: bool,
    /// Draw the holiday microphone in the tray and on the site. On by default;
    /// turning it off leaves the plain icon all year.
    #[serde(default = "default_true")]
    pub holiday_icons: bool,
    /// Debug: pin one holiday microphone regardless of the date, e.g.
    /// `"halloween"`. It works even when `holiday_icons` is off, so every icon
    /// can be looked at on the same machine. There is no field for it in
    /// Settings — it is set by hand in `config.json` and empty by default.
    #[serde(default)]
    pub debug_holiday: String,
    /// Window colours: follow Windows (the default), dark, or light.
    #[serde(default)]
    pub theme: Theme,
    /// Separator added after text inserted by record-and-paste.
    #[serde(default)]
    pub paste_separator: PasteSeparator,
    #[serde(default = "default_true")]
    pub autostart: bool,
    #[serde(default = "default_true")]
    pub silent_start: bool,
    #[serde(default = "default_sound_volume")]
    pub sound_volume: f32,
    #[serde(default = "default_true")]
    pub play_start_sound: bool,
    /// Crash cue: played when the daemon died and its recording is recovered.
    #[serde(default = "default_true")]
    pub play_error_sound: bool,
    #[serde(default = "default_true")]
    pub play_end_sound: bool,
    /// How long the recognizer stays in memory after the last dictation, in
    /// minutes. Five by default: a usual day keeps it hot, and a forgotten app
    /// gives the memory back. Zero means "never unload".
    #[serde(default = "default_model_idle_minutes")]
    pub model_idle_minutes: u32,
    /// What the window remembers between runs: the source and date filters of
    /// the history, the monologs and the Telegram log. The window process is
    /// short-lived, so the filters cannot live in it.
    #[serde(default = "sources_all")]
    pub history_sources: String,
    #[serde(default = "range_all")]
    pub history_range: SavedRange,
    #[serde(default = "range_all")]
    pub monolog_range: SavedRange,
    #[serde(default = "range_today")]
    pub telegram_range: SavedRange,
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
    /// Whether the local API server listens at all.
    #[serde(default)]
    pub api_enabled: bool,
    /// Host the local API server binds to; localhost by default.
    #[serde(default = "default_api_host")]
    pub api_host: String,
    /// Port the local API server listens on.
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// Real-time seconds the app may spend finishing the tail of an exclusive
    /// API session after the caller stopped, before the microphone is handed
    /// back. Zero finishes the whole tail, however long it takes.
    #[serde(default = "default_api_tail_seconds")]
    pub api_tail_seconds: u32,
    /// Interface language: a locale code from `locales/`, empty = the Windows
    /// language (or English when there is no matching catalog).
    #[serde(default)]
    pub ui_language: String,
    /// Whether the phone server listens at all.
    #[serde(default)]
    pub site_enabled: bool,
    /// Port the phone connects to.
    #[serde(default = "default_site_port")]
    pub site_port: u16,
    /// Address written into the QR code for use outside the home network; empty
    /// means "local network only".
    #[serde(default)]
    pub site_public_url: String,
    /// Refuse connections that do not come from the local network.
    #[serde(default = "default_true")]
    pub site_lan_only: bool,
    /// Longest segment the phone's audio is cut into. Shorter segments make the
    /// microphone answer faster when the owner takes it back.
    #[serde(default = "default_site_segment_seconds")]
    pub site_segment_seconds: f32,
    /// Telegram bot: off until the person turns it on in the settings.
    #[serde(default)]
    pub telegram_enabled: bool,
    /// Proxy for api.telegram.org, e.g. http://127.0.0.1:10408. Empty = direct.
    #[serde(default)]
    pub telegram_proxy: String,
    /// Test/override base URL; empty uses the real Bot API.
    #[serde(default)]
    pub telegram_api_base: String,
    /// Recognizer for the phone; empty uses the model selected in Settings.
    #[serde(default)]
    pub phone_model: String,
    /// Custom folder/file for the phone when `phone_model` is `custom`.
    #[serde(default)]
    pub phone_model_folder: Option<String>,
    /// Recognizer for Telegram; empty uses the model selected in Settings.
    #[serde(default)]
    pub telegram_model: String,
    /// Custom folder/file for Telegram when `telegram_model` is `custom`.
    #[serde(default)]
    pub telegram_model_folder: Option<String>,
}

/// Shortcuts start unbound: the user picks them in Settings.
fn default_hotkey() -> String {
    String::new()
}

fn default_sound_volume() -> f32 {
    0.5
}

/// Five minutes of silence is "the person is done dictating for now".
fn default_model_idle_minutes() -> u32 {
    5
}

/// A date filter as the settings file keeps it: the name of the preset and, for
/// a hand-picked period, its two days.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedRange {
    #[serde(default = "range_preset_today")]
    pub preset: String,
    /// `YYYY-MM-DD`; empty unless `preset` is `period`.
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
}

impl Default for SavedRange {
    fn default() -> Self {
        Self {
            preset: "today".to_string(),
            from: String::new(),
            to: String::new(),
        }
    }
}

impl SavedRange {
    pub fn from_filter(filter: crate::dates::DateFilter) -> Self {
        let period = filter.preset == crate::dates::Preset::Period;
        Self {
            preset: filter.preset.key().to_string(),
            from: if period {
                filter.from.format("%Y-%m-%d").to_string()
            } else {
                String::new()
            },
            to: if period {
                filter.to.format("%Y-%m-%d").to_string()
            } else {
                String::new()
            },
        }
    }

    /// The filter to start the window with. A hand-edited or outdated value must
    /// not break the list, so anything unreadable falls back to the defaults.
    pub fn to_filter(&self) -> crate::dates::DateFilter {
        let preset = crate::dates::Preset::parse(&self.preset).unwrap_or_default();
        let mut filter = crate::dates::DateFilter::preset(preset);
        if preset == crate::dates::Preset::Period {
            let today = chrono::Local::now().date_naive();
            let day = |text: &str| chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok();
            let from = day(&self.from).unwrap_or(today);
            let to = day(&self.to).unwrap_or(from).max(from);
            filter.from = from;
            filter.to = to;
        }
        filter
    }
}

fn range_preset_today() -> String {
    "today".to_string()
}

fn sources_all() -> String {
    "all".to_string()
}

/// Every list but Telegram starts on "all time": that is what the window showed
/// before the filters were remembered.
fn range_all() -> SavedRange {
    SavedRange {
        preset: "all".to_string(),
        from: String::new(),
        to: String::new(),
    }
}

fn range_today() -> SavedRange {
    SavedRange::default()
}

fn default_vad_sensitivity() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

fn default_site_port() -> u16 {
    crate::site::gate::DEFAULT_SITE_PORT
}

fn default_site_segment_seconds() -> f32 {
    15.0
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
            holiday_icons: true,
            debug_holiday: String::new(),
            theme: Theme::System,
            paste_separator: PasteSeparator::Space,
            autostart: true,
            silent_start: true,
            play_start_sound: true,
            play_error_sound: true,
            play_end_sound: true,
            sound_volume: 0.5,
            model_idle_minutes: 5,
            history_sources: "all".to_string(),
            history_range: range_all(),
            monolog_range: range_all(),
            telegram_range: range_today(),
            vad_sensitivity: 1.0,
            hotkey: String::new(),
            append_hotkey: String::new(),
            paste_hotkey: String::new(),
            api_enabled: false,
            api_host: default_api_host(),
            api_port: default_api_port(),
            api_tail_seconds: default_api_tail_seconds(),
            ui_language: String::new(),
            site_enabled: false,
            site_port: default_site_port(),
            site_public_url: String::new(),
            site_lan_only: true,
            site_segment_seconds: default_site_segment_seconds(),
            telegram_enabled: false,
            telegram_proxy: String::new(),
            telegram_api_base: String::new(),
            phone_model: String::new(),
            phone_model_folder: None,
            telegram_model: String::new(),
            telegram_model_folder: None,
        }
    }
}

/// Windows UI language, lowercased and without the region: "ru", "en", "zh".
pub fn system_language() -> String {
    use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buffer = [0u16; 85];
    // SAFETY: the buffer is valid for its stated length and the call only
    // writes up to that length.
    let len = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    if len <= 0 {
        return String::new();
    }
    let locale = String::from_utf16_lossy(&buffer[..(len as usize).saturating_sub(1)]);
    locale
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// First-run defaults: a model that fits the language of the system.
///
/// Russian speakers get the compact GigaAM RNN-T with punctuation, which handles
/// Russian better than a small Whisper. English, Chinese, Japanese and Korean
/// are covered by SenseVoice, which is multilingual and much lighter than a
/// large Whisper. Everything else starts on the multilingual Whisper model.
/// After the first run the choice belongs to the user.
pub fn defaults_for_language(language: &str) -> Config {
    let mut config = Config::default();
    config.ui_language = String::new();
    match language {
        "ru" => {
            config.backend = Backend::Onnx;
            config.onnx_model = "gigaam-v3-rnnt-punct".to_string();
            config.language = "ru".to_string();
        }
        "en" | "zh" | "ja" | "ko" => {
            config.backend = Backend::Onnx;
            config.onnx_model = "sense-voice".to_string();
            // SenseVoice detects the language; it also covers English.
            config.language = "auto".to_string();
        }
        _ => {
            config.backend = Backend::Whisper;
            config.whisper_model = "large-v3-turbo-q5_0".to_string();
            config.language = "auto".to_string();
        }
    }
    config
}

/// Config used when there is no file yet.
pub fn default_for_system() -> Config {
    defaults_for_language(&system_language())
}

fn default_api_host() -> String {
    "127.0.0.1".to_string()
}

fn default_api_port() -> u16 {
    8931
}

/// Ten seconds is enough for the tail of a normal API take and short enough
/// that the person at the computer gets the microphone back quickly.
fn default_api_tail_seconds() -> u32 {
    10
}

#[derive(Debug, Clone)]
pub struct AppDirs {
    pub root: PathBuf,
    pub models: PathBuf,
}

pub fn app_dirs() -> AppDirs {
    // Snapshot test runs must not touch the real data directory.
    // Tests and the e2e suite point the whole app at a throwaway directory.
    let root = if let Some(dir) = std::env::var_os("VB_DATA_DIR") {
        PathBuf::from(dir)
    } else if std::env::var("VB_SNAPSHOT").is_ok() {
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
    let mut config = match fs::read_to_string(root.join("config.json")) {
        // No settings yet: the first launch suggests a model for the system
        // language instead of a generic default.
        Err(_) => default_for_system(),
        Ok(text) => serde_json::from_str::<Config>(&text).unwrap_or_else(|_| default_for_system()),
    };
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

#[cfg(test)]
mod tests {
    use super::{defaults_for_language, Backend, Config, SavedRange};

    #[test]
    fn the_chosen_filters_survive_a_restart() {
        // The window keeps the filters in the settings file; a hand-picked
        // period must come back with the same two days.
        use crate::dates::{DateFilter, Preset};
        let mut period = DateFilter::preset(Preset::Period);
        period.from = chrono::NaiveDate::from_ymd_opt(2026, 2, 10).unwrap();
        period.to = chrono::NaiveDate::from_ymd_opt(2026, 2, 14).unwrap();
        let saved = SavedRange::from_filter(period);
        let text = serde_json::to_string(&saved).expect("serializes");
        let back: SavedRange = serde_json::from_str(&text).expect("parses");
        assert_eq!(back.to_filter(), period);
        assert_eq!(SavedRange::from_filter(DateFilter::everything()).to_filter(), DateFilter::everything());
        assert_eq!(SavedRange::from_filter(DateFilter::today()).to_filter(), DateFilter::today());
    }

    #[test]
    fn an_old_config_opens_the_lists_where_they_opened_before() {
        // Before the filters were remembered the history and the monologs opened
        // on everything and the Telegram log on today.
        let mut value = serde_json::to_value(Config::default()).expect("a config serializes");
        let object = value.as_object_mut().expect("a config is an object");
        for key in ["history_sources", "history_range", "monolog_range", "telegram_range"] {
            object.remove(key);
        }
        let old: Config = serde_json::from_value(value).expect("the old config loads");
        assert_eq!(old.history_sources, "all");
        assert_eq!(old.history_range.to_filter(), crate::dates::DateFilter::everything());
        assert_eq!(old.monolog_range.to_filter(), crate::dates::DateFilter::everything());
        assert_eq!(old.telegram_range.to_filter(), crate::dates::DateFilter::today());
    }

    #[test]
    fn the_model_is_unloaded_after_five_idle_minutes_by_default() {
        // A config written before the setting existed must still get the default,
        // and five minutes is the value the window shows.
        let mut value = serde_json::to_value(Config::default()).expect("a config serializes");
        value
            .as_object_mut()
            .expect("a config is an object")
            .remove("model_idle_minutes");
        let old: Config = serde_json::from_value(value).expect("the old config loads");
        assert_eq!(old.model_idle_minutes, 5);
        assert_eq!(Config::default().model_idle_minutes, 5);
    }

    #[test]
    fn russian_systems_get_the_gigaam_rnnt_model() {
        let cfg = defaults_for_language("ru");
        assert_eq!(cfg.backend, Backend::Onnx);
        assert_eq!(cfg.onnx_model, "gigaam-v3-rnnt-punct");
        assert_eq!(cfg.language, "ru");
        // The interface follows the system language, not a fixed one.
        assert!(cfg.ui_language.is_empty());
    }

    #[test]
    fn english_chinese_japanese_and_korean_get_sense_voice() {
        for language in ["en", "zh", "ja", "ko"] {
            let cfg = defaults_for_language(language);
            assert_eq!(cfg.backend, Backend::Onnx, "{language}");
            assert_eq!(cfg.onnx_model, "sense-voice", "{language}");
            assert_eq!(cfg.language, "auto", "{language}");
        }
    }

    #[test]
    fn other_languages_get_the_multilingual_whisper_model() {
        let cfg = defaults_for_language("de");
        assert_eq!(cfg.backend, Backend::Whisper);
        assert_eq!(cfg.whisper_model, "large-v3-turbo-q5_0");
    }
}
