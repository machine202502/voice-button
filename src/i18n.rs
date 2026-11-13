//! Interface translations.
//!
//! The binary ships no translated strings: catalogs are plain JSON files in a
//! `locales/` folder next to the executable (and optionally in the data
//! directory), so a translation can be fixed or added without rebuilding.
//!
//! English source strings are the keys and the fallback, so a missing or
//! incomplete file degrades to English instead of breaking the window.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Catalog {
    pub code: String,
    /// Native name shown in Settings, e.g. "Русский".
    pub name: String,
    #[serde(default)]
    pub strings: BTreeMap<String, String>,
}

impl Catalog {
    fn english() -> Self {
        Self {
            code: "en".to_string(),
            name: "English".to_string(),
            strings: BTreeMap::new(),
        }
    }
}

fn active() -> &'static RwLock<Catalog> {
    static ACTIVE: OnceLock<RwLock<Catalog>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(Catalog::english()))
}

fn catalogs() -> &'static RwLock<Vec<Catalog>> {
    static CATALOGS: OnceLock<RwLock<Vec<Catalog>>> = OnceLock::new();
    CATALOGS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Translate one English source string.
pub fn tr(key: &str) -> String {
    let guard = active().read().unwrap();
    match guard.strings.get(key) {
        Some(text) => text.clone(),
        None => key.to_string(),
    }
}

/// Translate one key in a named catalog, regardless of the active language.
/// Used for Telegram command descriptions, which are registered for every
/// supported language at once.
pub fn tr_lang(code: &str, key: &str) -> String {
    let guard = catalogs().read().unwrap();
    guard
        .iter()
        .find(|catalog| catalog.code == code)
        .and_then(|catalog| catalog.strings.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// Translate and substitute `{name}` placeholders.
pub fn trf(key: &str, args: &[(&str, &str)]) -> String {
    let mut text = tr(key);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// Folders searched for catalogs, in order of increasing priority: the app's
/// own `locales/` next to the executable, then a user-editable copy in the data
/// directory (so a translation can be dropped in without touching Program Files).
pub fn search_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("locales"));
        }
    }
    dirs.push(root.join("locales"));
    dirs
}

/// Read every catalog found in `dirs`; a later folder overrides the same code.
fn load_catalogs(dirs: &[PathBuf]) -> Vec<Catalog> {
    let mut by_code: BTreeMap<String, Catalog> = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match serde_json::from_str::<Catalog>(&text) {
                Ok(mut catalog) => {
                    if catalog.code.is_empty() {
                        catalog.code = path
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or("en")
                            .to_string();
                    }
                    if catalog.name.is_empty() {
                        catalog.name = catalog.code.clone();
                    }
                    by_code.insert(catalog.code.clone(), catalog);
                }
                Err(e) => crate::log::log(&format!(
                    "locale {} is not valid JSON: {e:#}",
                    path.display()
                )),
            }
        }
    }
    by_code.into_values().collect()
}

/// Pick the catalog for `preferred` (empty or "system" = the Windows language)
/// and make it active. Returns the applied code.
pub fn init(root: &Path, preferred: &str) -> String {
    let wanted = if preferred.is_empty() || preferred == "system" {
        crate::config::system_language()
    } else {
        preferred.to_string()
    };
    let found = load_catalogs(&search_dirs(root));
    let chosen = found
        .iter()
        .find(|catalog| catalog.code == wanted)
        .or_else(|| found.iter().find(|catalog| catalog.code == "en"))
        .cloned()
        .unwrap_or_else(Catalog::english);
    let code = chosen.code.clone();
    *active().write().unwrap() = chosen;
    *catalogs().write().unwrap() = found;
    code
}

/// Catalogs available for the Settings picker, English always included.
pub fn available() -> Vec<(String, String)> {
    let guard = catalogs().read().unwrap();
    let mut list: Vec<(String, String)> = guard
        .iter()
        .map(|catalog| (catalog.code.clone(), catalog.name.clone()))
        .collect();
    if !list.iter().any(|(code, _)| code == "en") {
        list.push(("en".to_string(), "English".to_string()));
    }
    list
}

/// Name of the active catalog's language, for the settings label.
pub fn active_name() -> String {
    active().read().unwrap().name.clone()
}

/// Code of the active catalog ("ru", "en", "zh"). The phone app takes its
/// language from here: one setting, no second picker.
pub fn active_code() -> String {
    active().read().unwrap().code.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_catalog_can_be_read_without_changing_the_active_one() {
        let dir = std::env::temp_dir().join(format!("vb-i18n-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ru.json"),
            r#"{"code":"ru","name":"Русский","strings":{"Start the bot":"Запустить бота"}}"#,
        )
        .unwrap();
        *catalogs().write().unwrap() = load_catalogs(&[dir.clone()]);
        assert_eq!(tr_lang("ru", "Start the bot"), "Запустить бота");
        assert_eq!(tr_lang("en", "Start the bot"), "Start the bot");
        assert_eq!(tr_lang("ru", "missing"), "missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_translations_fall_back_to_the_english_key() {
        *active().write().unwrap() = Catalog::english();
        assert_eq!(tr("Settings"), "Settings");
        assert_eq!(trf("REC {secs}s", &[("secs", "7")]), "REC 7s");
    }

    #[test]
    fn a_catalog_overrides_known_keys_only() {
        let dir = std::env::temp_dir().join(format!("vb-i18n-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ru.json"),
            r#"{"code":"ru","name":"Русский","strings":{"Settings":"Настройки"}}"#,
        )
        .unwrap();
        let found = load_catalogs(&[dir.clone()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].code, "ru");
        assert_eq!(found[0].name, "Русский");
        assert_eq!(found[0].strings.get("Settings").map(String::as_str), Some("Настройки"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
