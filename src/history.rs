use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::store::Store;

/// One appended piece inside a message that was built with the append
/// shortcut. The full merged text stays in `history.text`; `parts` only
/// records when each additional piece was added.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryChunk {
    pub at: String,
    pub text: String,
}

/// Who started the recording that produced an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Dictated by a human through the window, a shortcut or the tray icon.
    #[default]
    User,
    /// Started by the local API server.
    Server,
    /// An audio file imported by the user.
    File,
    /// Dictated from the phone: the original text never changes here, the
    /// phone keeps its own editable copy.
    Site,
    /// A voice/file sent to the Telegram bot.
    Telegram,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::User => "user",
            Source::Server => "server",
            Source::File => "file",
            Source::Site => "site",
            Source::Telegram => "telegram",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "server" => Source::Server,
            "file" => Source::File,
            "site" => Source::Site,
            "telegram" => Source::Telegram,
            _ => Source::User,
        }
    }
}

/// Whether the recording behind an entry was transcribed completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EntryState {
    #[default]
    Complete,
    /// Recovered after a crash, or one segment could not be transcribed: the
    /// text is in the history but may be missing a part.
    Partial,
}

impl EntryState {
    pub fn is_partial(self) -> bool {
        self == EntryState::Partial
    }

    fn as_str(self) -> &'static str {
        match self {
            EntryState::Complete => "complete",
            EntryState::Partial => "partial",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "partial" => EntryState::Partial,
            _ => EntryState::Complete,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub at: String,
    pub text: String,
    #[serde(default)]
    pub model: String,
    /// Appended pieces in chronological order; empty for plain recordings.
    #[serde(default)]
    pub parts: Vec<HistoryChunk>,
    /// Interactor that started the recording (old entries have no field and
    /// read back as the user).
    #[serde(default)]
    pub source: Source,
    /// Complete unless the recording was recovered after a crash.
    #[serde(default)]
    pub state: EntryState,
    /// Audio kept for the user because the transcription is not guaranteed.
    /// An absolute path to the file that belongs to this entry.
    #[serde(default)]
    pub audio: Option<String>,
    /// Group of a message dictated from the phone, as it was named then.
    #[serde(default)]
    pub group: Option<String>,
    /// Id of the phone message this entry came from.
    #[serde(default)]
    pub message: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct HistoryRow {
    id: i64,
    at: String,
    preview: String,
    model: String,
    pieces: usize,
    source: Source,
    state: EntryState,
    audio: Option<String>,
    group: Option<String>,
}

impl HistoryRow {
    /// One-line preview: line breaks are flattened so the fixed row height in
    /// the virtualized list stays correct.
    fn preview_of(text: &str) -> String {
        text.chars()
            .take(60)
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect()
    }

    fn display_at(at: &str) -> String {
        chrono::DateTime::parse_from_rfc3339(at)
            .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|_| at.to_string())
    }

    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            at: Self::display_at(&row.get::<_, String>(1)?),
            preview: Self::preview_of(&row.get::<_, String>(2)?),
            model: row.get(3)?,
            pieces: row.get::<_, i64>(4)? as usize,
            source: Source::from_str(&row.get::<_, String>(5)?),
            state: EntryState::from_str(&row.get::<_, String>(6)?),
            audio: row.get(7)?,
            group: row.get(8)?,
        })
    }

    fn new(entry: &HistoryEntry, id: i64) -> Self {
        Self {
            id,
            at: Self::display_at(&entry.at),
            preview: Self::preview_of(&entry.text),
            model: entry.model.clone(),
            pieces: entry.parts.len(),
            source: entry.source,
            state: entry.state,
            audio: entry.audio.clone(),
            group: entry.group.clone(),
        }
    }

    pub fn id(&self) -> i64 {
        self.id
    }

    pub fn at(&self) -> &str {
        &self.at
    }

    pub fn preview(&self) -> &str {
        &self.preview
    }

    pub fn model(&self) -> &str {
        if self.model.is_empty() {
            "unknown model"
        } else {
            &self.model
        }
    }

    /// Number of pieces appended to this message after it was created.
    pub fn pieces(&self) -> usize {
        self.pieces
    }

    /// Who started the recording behind this entry.
    pub fn source(&self) -> Source {
        self.source
    }

    /// Complete, or recovered/partial after a crash.
    pub fn state(&self) -> EntryState {
        self.state
    }

    /// Audio the user may open because the text is not guaranteed.
    pub fn audio(&self) -> Option<&str> {
        self.audio.as_deref()
    }

    /// Group of a message dictated from the phone.
    pub fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }
}

/// Handle to the history table. Rows are read by page, so a window never holds
/// the whole table; a plain vector is kept only for tests and the snapshot.
pub struct History {
    store: Option<Arc<Store>>,
    inline: Vec<HistoryEntry>,
    len: usize,
}

impl History {
    pub fn load(root: &Path) -> Self {
        match Self::open(root) {
            Ok(history) => history,
            Err(e) => {
                crate::log::log(&format!("history database failed: {e:#}"));
                Self::empty()
            }
        }
    }

    fn open(root: &Path) -> Result<Self> {
        let store = Arc::new(Store::open(root)?);
        let len: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM history", [], |row| row.get(0))?;
        Ok(Self {
            store: Some(store),
            inline: Vec::new(),
            len: len as usize,
        })
    }

    pub fn from_entries(entries: Vec<HistoryEntry>) -> Self {
        Self {
            store: None,
            len: entries.len(),
            inline: entries,
        }
    }

    fn empty() -> Self {
        Self {
            store: None,
            inline: Vec::new(),
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// A full entry by its page index (newest first). Only the row that is
    /// actually opened is read.
    pub fn entry(&self, index: usize) -> Result<HistoryEntry> {
        if let Some(store) = &self.store {
            let id: i64 = store
                .conn()
                .query_row(
                    "SELECT id FROM history ORDER BY at DESC, id DESC LIMIT 1 OFFSET ?1",
                    [index as i64],
                    |row| row.get(0),
                )
                .optional()?
                .context("history row is out of range")?;
            return self.entry_by_id(id);
        }
        self.inline
            .get(index)
            .cloned()
            .context("history row is out of range")
    }

    pub fn entry_by_id(&self, id: i64) -> Result<HistoryEntry> {
        let store = self.store.as_ref().context("history has no backing database")?;
        read_entry(&store.conn(), id)?.context("history row disappeared from the database")
    }

    pub fn row(&self, index: usize) -> Option<HistoryRow> {
        if let Some(store) = &self.store {
            let conn = store.conn();
            return conn
                .query_row(
                    &format!("{ROW_SELECT} ORDER BY h.at DESC, h.id DESC LIMIT 1 OFFSET ?1"),
                    [index as i64],
                    HistoryRow::from_row,
                )
                .optional()
                .ok()
                .flatten();
        }
        self.inline
            .get(index)
            .map(|entry| HistoryRow::new(entry, 0))
    }

    /// One page of index rows, newest first. `sources` empty means all.
    pub fn page(
        &self,
        sources: &[Source],
        needle: &str,
        offset: usize,
        limit: usize,
    ) -> Vec<HistoryRow> {
        if let Some(store) = &self.store {
            let conn = store.conn();
            let (filter, params) = source_filter(sources, needle);
            let sql = format!("{ROW_SELECT}{filter} ORDER BY h.at DESC, h.id DESC LIMIT ? OFFSET ?");
            let mut statement = match conn.prepare(&sql) {
                Ok(statement) => statement,
                Err(_) => return Vec::new(),
            };
            let mut values: Vec<Box<dyn rusqlite::ToSql>> = params
                .iter()
                .map(|value| Box::new(value.clone()) as Box<dyn rusqlite::ToSql>)
                .collect();
            values.push(Box::new(limit as i64));
            values.push(Box::new(offset as i64));
            let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|value| value.as_ref()).collect();
            return statement
                .query_map(refs.as_slice(), HistoryRow::from_row)
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default();
        }
        let needle = needle.to_lowercase();
        self.inline
            .iter()
            .filter(|entry| sources.is_empty() || sources.contains(&entry.source))
            .filter(|entry| needle.is_empty() || entry.text.to_lowercase().contains(&needle))
            .skip(offset)
            .take(limit)
            .map(|entry| HistoryRow::new(entry, 0))
            .collect()
    }

    pub fn count(&self, sources: &[Source], needle: &str) -> usize {
        if self.store.is_none() {
            let needle = needle.to_lowercase();
            return self
                .inline
                .iter()
                .filter(|entry| sources.is_empty() || sources.contains(&entry.source))
                .filter(|entry| needle.is_empty() || entry.text.to_lowercase().contains(&needle))
                .count();
        }
        let store = self.store.as_ref().unwrap();
        let conn = store.conn();
        let (filter, params) = source_filter(sources, needle);
        let sql = format!("SELECT COUNT(*) FROM history h{filter}");
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|value| value as &dyn rusqlite::ToSql).collect();
        conn.query_row(&sql, refs.as_slice(), |row| row.get::<_, i64>(0))
            .map(|count| count as usize)
            .unwrap_or(0)
    }

    pub fn push_row(&mut self, _row: HistoryRow) {
        self.len += 1;
    }

    /// Rewrite the newest entry in place. Used to append a piece to a message.
    pub fn update_last(&mut self, _root: &Path, entry: &HistoryEntry) -> Result<HistoryRow> {
        let store = self.store.as_ref().context("history has no backing database")?;
        let mut conn = store.conn();
        let id: i64 = conn
            .query_row("SELECT id FROM history ORDER BY id DESC LIMIT 1", [], |row| row.get(0))
            .optional()?
            .context("history is empty")?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "UPDATE history SET at = ?1, text = ?2, model = ?3, source = ?4, state = ?5,
                audio = ?6, group_name = ?7, message_id = ?8 WHERE id = ?9",
            params![
                entry.at,
                entry.text,
                entry.model,
                entry.source.as_str(),
                entry.state.as_str(),
                entry.audio,
                entry.group,
                entry.message.map(|value| value as i64),
                id
            ],
        )?;
        transaction.execute("DELETE FROM history_parts WHERE history_id = ?1", [id])?;
        for (position, part) in entry.parts.iter().enumerate() {
            transaction.execute(
                "INSERT INTO history_parts(history_id, position, at, text)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, position as i64, part.at, part.text],
            )?;
        }
        transaction.commit()?;
        let _ = conn.execute(
            "INSERT INTO kv(key, value) VALUES ('rev.history', '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [],
        );
        Ok(HistoryRow::new(entry, id))
    }

    pub fn clear_local(&mut self) {
        self.inline.clear();
        self.len = 0;
    }
}

const ROW_SELECT: &str = "SELECT h.id, h.at, substr(h.text, 1, 240), h.model,
    (SELECT COUNT(*) FROM history_parts p WHERE p.history_id = h.id),
    h.source, h.state, h.audio, h.group_name FROM history h";

fn source_filter(sources: &[Source], needle: &str) -> (String, Vec<String>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();
    if !sources.is_empty() {
        let marks: Vec<&str> = sources.iter().map(|_| "?").collect();
        clauses.push(format!("h.source IN ({})", marks.join(",")));
        params.extend(sources.iter().map(|source| source.as_str().to_string()));
    }
    if !needle.trim().is_empty() {
        clauses.push("vb_lower(h.text) LIKE vb_lower(?) ESCAPE '\\'".to_string());
        params.push(format!(
            "%{}%",
            needle
                .trim()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        ));
    }
    let filter = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (filter, params)
}

fn read_entry(conn: &Connection, id: i64) -> Result<Option<HistoryEntry>> {
    let mut entry = conn
        .query_row(
            "SELECT id, at, text, model, source, state, audio, group_name, message_id
             FROM history WHERE id = ?1",
            [id],
            |row| {
                Ok(HistoryEntry {
                    at: row.get(1)?,
                    text: row.get(2)?,
                    model: row.get(3)?,
                    parts: Vec::new(),
                    source: Source::from_str(&row.get::<_, String>(4)?),
                    state: EntryState::from_str(&row.get::<_, String>(5)?),
                    audio: row.get(6)?,
                    group: row.get(7)?,
                    message: row.get::<_, Option<i64>>(8)?.map(|value| value as u64),
                })
            },
        )
        .optional()?;
    let Some(entry) = entry.as_mut() else {
        return Ok(None);
    };
    let mut statement = conn.prepare(
        "SELECT at, text FROM history_parts WHERE history_id = ?1 ORDER BY position",
    )?;
    entry.parts = statement
        .query_map([id], |row| {
            Ok(HistoryChunk {
                at: row.get(0)?,
                text: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(entry.clone()))
}

impl From<Vec<HistoryEntry>> for History {
    fn from(entries: Vec<HistoryEntry>) -> Self {
        Self::from_entries(entries)
    }
}

pub fn history_path(root: &Path) -> PathBuf {
    root.join("voice-button.sqlite3")
}

pub fn append(root: &Path, entry: &HistoryEntry) -> Result<HistoryRow> {
    let store = Store::open(root)?;
    let mut conn = store.conn();
    let transaction = conn.transaction()?;
    transaction.execute(
        "INSERT INTO history(at, text, model, source, state, audio, group_name, message_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            entry.at,
            entry.text,
            entry.model,
            entry.source.as_str(),
            entry.state.as_str(),
            entry.audio,
            entry.group,
            entry.message.map(|value| value as i64)
        ],
    )?;
    let id = transaction.last_insert_rowid();
    for (position, part) in entry.parts.iter().enumerate() {
        transaction.execute(
            "INSERT INTO history_parts(history_id, position, at, text)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, position as i64, part.at, part.text],
        )?;
    }
    transaction.commit()?;
    let _ = conn.execute(
        "INSERT INTO kv(key, value) VALUES ('rev.history', '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
        [],
    );
    Ok(HistoryRow::new(entry, id))
}

pub fn clear(root: &Path) {
    if let Ok(store) = Store::open(root) {
        let conn = store.conn();
        let _ = conn.execute("DELETE FROM history_parts", []);
        let _ = conn.execute("DELETE FROM history", []);
        let _ = conn.execute(
            "INSERT INTO kv(key, value) VALUES ('rev.history', '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_rfc_timestamp_is_shown_like_the_other_history_rows() {
        let shown = HistoryRow::display_at("2026-11-13T13:54:10.530631300+05:00");
        assert_eq!(shown.len(), 19, "YYYY-MM-DD HH:MM:SS");
        assert!(shown.starts_with("2026-11-13 "));
        assert!(!shown.contains('T'));
    }

    fn test_root(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "voice-button-{name}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn entry(index: usize) -> HistoryEntry {
        HistoryEntry {
            at: format!("2026-11-06 00:00:{index:04}"),
            text: format!("Сообщение номер {index}, которое должно сохраниться полностью"),
            model: format!("model-{index}"),
            parts: Vec::new(),
            source: Source::User,
            state: EntryState::Complete,
            audio: None,
            group: None,
            message: None,
        }
    }

    #[test]
    fn a_phone_message_keeps_its_group_and_original_text() {
        let root = test_root("history-phone");
        let mut site = entry(1);
        site.source = Source::Site;
        site.group = Some("Работа".to_string());
        site.message = Some(7);
        append(&root, &site).unwrap();
        let loaded = History::load(&root);
        let row = loaded.row(0).expect("one row");
        assert_eq!(row.source(), Source::Site);
        assert_eq!(row.group(), Some("Работа"));
        let stored = loaded.entry(0).unwrap();
        assert_eq!(stored.message, Some(7));
        assert_eq!(stored.text, site.text);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn sqlite_round_trip_preserves_every_entry() {
        let root = test_root("history-roundtrip");
        for index in 0..1000 {
            append(&root, &entry(index)).unwrap();
        }
        let loaded = History::load(&root);
        assert_eq!(loaded.len(), 1000);
        assert_eq!(loaded.entry(0).unwrap().text, entry(999).text);
        assert_eq!(loaded.entry(999).unwrap().text, entry(0).text);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn update_last_merges_into_the_newest_row_only() {
        let root = test_root("history-append");
        append(&root, &entry(1)).unwrap();
        append(&root, &entry(2)).unwrap();
        let mut history = History::load(&root);
        let mut merged = history.entry(0).unwrap();
        merged.text = format!("{} Продолжение мысли", merged.text);
        merged.parts.push(HistoryChunk {
            at: "2026-11-06 00:01:00".to_string(),
            text: "Продолжение мысли".to_string(),
        });
        history.update_last(&root, &merged).unwrap();
        let loaded = History::load(&root);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.entry(1).unwrap().text, entry(1).text);
        let newest = loaded.entry(0).unwrap();
        assert_eq!(newest.text, format!("{} Продолжение мысли", entry(2).text));
        assert_eq!(newest.parts.len(), 1);
        assert_eq!(newest.parts[0].text, "Продолжение мысли");
        let mut loaded = History::load(&root);
        let mut again = loaded.entry(0).unwrap();
        again.text.push_str(" И ещё кусочек");
        again.parts.push(HistoryChunk {
            at: "2026-11-06 00:02:00".to_string(),
            text: "И ещё кусочек".to_string(),
        });
        loaded.update_last(&root, &again).unwrap();
        let loaded = History::load(&root);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.entry(0).unwrap().parts.len(), 2);
        assert_eq!(loaded.entry(1).unwrap().text, entry(1).text);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn entries_with_parts_round_trip() {
        let root = test_root("history-parts");
        let mut big = entry(1);
        big.text = "Первый кусочек. Второй кусочек.".to_string();
        big.parts.push(HistoryChunk {
            at: "2026-11-06 00:03:00".to_string(),
            text: "Второй кусочек.".to_string(),
        });
        append(&root, &big).unwrap();
        let loaded = History::load(&root);
        assert_eq!(loaded.entry(0).unwrap(), big);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pages_do_not_read_full_entry_text() {
        let root = test_root("history-lazy");
        for index in 0..20 {
            append(&root, &entry(index)).unwrap();
        }
        let loaded = History::load(&root);
        assert_eq!(loaded.len(), 20);
        assert_eq!(loaded.row(0).unwrap().preview(), entry(19).text);
        assert_eq!(loaded.page(&[], "", 0, 5).len(), 5);
        loaded.entry(10).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_and_source_filters_run_over_the_whole_table() {
        let root = test_root("history-search");
        let mut server = entry(1);
        server.source = Source::Server;
        append(&root, &server).unwrap();
        append(&root, &entry(2)).unwrap();
        append(&root, &entry(3)).unwrap();
        let loaded = History::load(&root);
        assert_eq!(loaded.count(&[Source::Server], ""), 1);
        assert_eq!(loaded.count(&[], "номер 2"), 1);
        assert_eq!(loaded.count(&[Source::User], "номер 2"), 1);
        assert_eq!(loaded.page(&[], "номер 2", 0, 10)[0].preview(), entry(2).text);
        assert_eq!(loaded.page(&[Source::Server], "", 0, 10)[0].source(), Source::Server);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn entries_read_in_recording_time_order() {
        let root = test_root("history-order");
        let mut late = entry(1);
        late.at = "2026-11-06 12:00:00".to_string();
        append(&root, &late).unwrap();
        let mut early = entry(2);
        early.at = "2026-11-06 09:00:00".to_string();
        append(&root, &early).unwrap();
        let loaded = History::load(&root);
        assert_eq!(loaded.entry(0).unwrap().at, "2026-11-06 12:00:00");
        assert_eq!(loaded.entry(1).unwrap().at, "2026-11-06 09:00:00");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clear_empties_the_history() {
        let root = test_root("history-clear");
        append(&root, &entry(1)).unwrap();
        clear(&root);
        assert!(History::load(&root).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn server_entries_keep_their_source_through_the_index() {
        let root = test_root("history-source");
        let mut recorded = entry(1);
        recorded.source = Source::Server;
        append(&root, &recorded).unwrap();
        append(&root, &entry(2)).unwrap();
        let history = History::load(&root);
        assert_eq!(history.row(0).unwrap().source(), Source::User);
        assert_eq!(history.row(1).unwrap().source(), Source::Server);
        assert_eq!(history.entry(1).unwrap().source, Source::Server);
        let _ = std::fs::remove_dir_all(root);
    }
}
