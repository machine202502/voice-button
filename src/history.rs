use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// One appended piece inside a message that was built with the append
/// shortcut. The full merged text stays in `HistoryEntry::text`; `parts` only
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
}

#[derive(Debug, Clone)]
pub struct HistoryRow {
    at: String,
    preview: String,
    model: String,
    offset: u64,
    len: usize,
    pieces: usize,
    source: Source,
    state: EntryState,
    audio: Option<String>,
    inline_entry: Option<HistoryEntry>,
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

    fn new(entry: &HistoryEntry, offset: u64, len: usize) -> Self {
        Self {
            at: entry.at.clone(),
            preview: Self::preview_of(&entry.text),
            model: entry.model.clone(),
            offset,
            len,
            pieces: entry.parts.len(),
            source: entry.source,
            state: entry.state,
            audio: entry.audio.clone(),
            inline_entry: None,
        }
    }

    fn inline(entry: HistoryEntry) -> Self {
        Self {
            preview: Self::preview_of(&entry.text),
            at: entry.at.clone(),
            model: entry.model.clone(),
            offset: 0,
            len: 0,
            pieces: entry.parts.len(),
            source: entry.source,
            state: entry.state,
            audio: entry.audio.clone(),
            inline_entry: Some(entry),
        }
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
}

/// Newest-first index of a JSONL file. Full texts stay on disk and are read
/// individually by byte offset; only timestamps and short previews are kept.
pub struct History {
    root: Option<PathBuf>,
    rows: Vec<HistoryRow>,
    #[cfg(test)]
    disk_reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl History {
    pub fn load(root: &Path) -> Self {
        if let Err(e) = migrate_legacy(root) {
            crate::log::log(&format!("history migration failed: {e:#}"));
        }
        match Self::index(root) {
            Ok(history) => history,
            Err(e) => {
                crate::log::log(&format!("history indexing failed: {e:#}"));
                Self::empty(Some(root.to_path_buf()))
            }
        }
    }

    pub fn from_entries(entries: Vec<HistoryEntry>) -> Self {
        let mut history = Self::empty(None);
        history.rows = entries.into_iter().map(HistoryRow::inline).collect();
        history
    }

    fn empty(root: Option<PathBuf>) -> Self {
        Self {
            root,
            rows: Vec::new(),
            #[cfg(test)]
            disk_reads: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn index(root: &Path) -> Result<Self> {
        let path = history_path(root);
        if !path.exists() {
            return Ok(Self::empty(Some(root.to_path_buf())));
        }
        let file = std::fs::File::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let mut reader = BufReader::new(file);
        let mut rows = Vec::new();
        let mut offset = 0u64;
        loop {
            let mut line = String::new();
            let bytes = reader.read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            let line_len = line.trim_end_matches(['\r', '\n']).len();
            let line_offset = offset;
            offset += bytes as u64;
            if line_len == 0 {
                continue;
            }
            match serde_json::from_str::<HistoryEntry>(line.trim_end_matches(['\r', '\n'])) {
                Ok(entry) => rows.push(HistoryRow::new(&entry, line_offset, line_len)),
                Err(e) => crate::log::log(&format!(
                    "skipping invalid history line at byte {line_offset}: {e}"
                )),
            }
        }
        rows.reverse();
        let mut history = Self::empty(Some(root.to_path_buf()));
        history.rows = rows;
        Ok(history)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, index: usize) -> Option<&HistoryRow> {
        self.rows.get(index)
    }

    pub fn entry(&self, index: usize) -> Result<HistoryEntry> {
        let row = self
            .rows
            .get(index)
            .context("history row is out of range")?;
        if let Some(entry) = &row.inline_entry {
            return Ok(entry.clone());
        }
        let root = self.root.as_ref().context("history has no backing file")?;
        let path = history_path(root);
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.seek(SeekFrom::Start(row.offset))?;
        let mut bytes = vec![0; row.len];
        file.read_exact(&mut bytes)?;
        #[cfg(test)]
        self.disk_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        serde_json::from_slice(&bytes).context("failed to decode history entry")
    }

    pub fn push_row(&mut self, row: HistoryRow) {
        self.rows.insert(0, row);
    }

    /// Rewrite the newest entry in place. The replaced line is the last line
    /// of the file, so truncating at its start offset and rewriting keeps every
    /// earlier entry's byte offset intact. Used to append a piece to a message.
    pub fn update_last(&mut self, root: &Path, entry: &HistoryEntry) -> Result<HistoryRow> {
        let row = self.rows.first().context("history is empty")?;
        if row.inline_entry.is_some() {
            // In-memory rows (UI snapshot mode) have no position on disk.
            anyhow::bail!("history row has no backing file");
        }
        let offset = row.offset;
        let path = history_path(root);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        let mut line = serde_json::to_vec(entry)?;
        let len = line.len();
        line.push(b'\n');
        file.seek(SeekFrom::Start(offset))?;
        file.set_len(offset)?;
        file.write_all(&line)?;
        file.flush()?;
        let new_row = HistoryRow::new(entry, offset, len);
        self.rows[0] = new_row.clone();
        Ok(new_row)
    }

    pub fn clear_local(&mut self) {
        self.rows.clear();
    }

    #[cfg(test)]
    fn disk_reads(&self) -> usize {
        self.disk_reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl From<Vec<HistoryEntry>> for History {
    fn from(entries: Vec<HistoryEntry>) -> Self {
        Self::from_entries(entries)
    }
}

pub fn history_path(root: &Path) -> PathBuf {
    root.join("history.jsonl")
}

fn legacy_history_path(root: &Path) -> PathBuf {
    root.join("history.json")
}

fn migrate_legacy(root: &Path) -> Result<()> {
    let target = history_path(root);
    let legacy = legacy_history_path(root);
    if target.exists() || !legacy.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&legacy)
        .with_context(|| format!("failed to read {}", legacy.display()))?;
    let entries: Vec<HistoryEntry> = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", legacy.display()))?;
    let mut jsonl = String::new();
    for entry in entries.iter().rev() {
        jsonl.push_str(&serde_json::to_string(entry)?);
        jsonl.push('\n');
    }
    crate::ipc::atomic_write(target, jsonl.as_bytes());
    Ok(())
}

pub fn append(root: &Path, entry: &HistoryEntry) -> Result<HistoryRow> {
    std::fs::create_dir_all(root)?;
    let path = history_path(root);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let offset = file.metadata()?.len();
    let mut line = serde_json::to_vec(entry)?;
    let len = line.len();
    line.push(b'\n');
    file.write_all(&line)?;
    file.flush()?;
    Ok(HistoryRow::new(entry, offset, len))
}

pub fn clear(root: &Path) {
    crate::ipc::atomic_write(history_path(root), &[]);
}

#[cfg(test)]
mod tests {
    use super::{append, clear, history_path, History, HistoryEntry};
    use std::path::PathBuf;

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
            source: super::Source::User,
            state: super::EntryState::Complete,
            audio: None,
        }
    }

    #[test]
    fn jsonl_round_trip_preserves_every_entry() {
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
    fn update_last_merges_into_the_newest_line_only() {
        let root = test_root("history-append");
        append(&root, &entry(1)).unwrap();
        append(&root, &entry(2)).unwrap();
        let mut history = History::load(&root);
        let mut merged = history.entry(0).unwrap();
        merged.text = format!("{} Продолжение мысли", merged.text);
        merged.parts.push(super::HistoryChunk {
            at: "2026-11-06 00:01:00".to_string(),
            text: "Продолжение мысли".to_string(),
        });
        history.update_last(&root, &merged).unwrap();
        // Both entries survive, the newest one carries the merged text.
        let loaded = History::load(&root);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.entry(1).unwrap().text, entry(1).text);
        let newest = loaded.entry(0).unwrap();
        assert_eq!(newest.text, format!("{} Продолжение мысли", entry(2).text));
        assert_eq!(newest.parts.len(), 1);
        assert_eq!(newest.parts[0].text, "Продолжение мысли");
        // A second append keeps working (offsets stay valid).
        let mut loaded = History::load(&root);
        let mut again = loaded.entry(0).unwrap();
        again.text.push_str(" И ещё кусочек");
        again.parts.push(super::HistoryChunk {
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
        big.parts.push(super::HistoryChunk {
            at: "2026-11-06 00:03:00".to_string(),
            text: "Второй кусочек.".to_string(),
        });
        append(&root, &big).unwrap();
        let loaded = History::load(&root);
        assert_eq!(loaded.entry(0).unwrap(), big);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn indexing_does_not_read_full_entry_text() {
        let root = test_root("history-lazy");
        for index in 0..20 {
            append(&root, &entry(index)).unwrap();
        }
        let loaded = History::load(&root);
        assert_eq!(loaded.len(), 20);
        assert_eq!(loaded.disk_reads(), 0);
        assert_eq!(loaded.row(0).unwrap().preview(), entry(19).text);
        loaded.entry(10).unwrap();
        assert_eq!(loaded.disk_reads(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_json_is_migrated_without_reordering() {
        let root = test_root("history-migration");
        let entries = vec![entry(2), entry(1), entry(0)];
        std::fs::write(
            root.join("history.json"),
            serde_json::to_string_pretty(&entries).unwrap(),
        )
        .unwrap();
        let loaded = History::load(&root);
        assert!(history_path(&root).exists());
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.entry(0).unwrap().text, entry(2).text);
        assert_eq!(loaded.entry(2).unwrap().text, entry(0).text);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn clear_empties_the_jsonl_history() {
        let root = test_root("history-clear");
        append(&root, &entry(1)).unwrap();
        clear(&root);
        assert!(History::load(&root).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn entries_without_model_remain_readable() {
        let root = test_root("history-no-model");
        std::fs::write(
            history_path(&root),
            "{\"at\":\"2026-11-06 00:00:00\",\"text\":\"Старая запись\"}\n",
        )
        .unwrap();
        let history = History::load(&root);
        assert_eq!(history.row(0).unwrap().model(), "unknown model");
        assert_eq!(history.row(0).unwrap().source(), super::Source::User);
        assert_eq!(history.entry(0).unwrap().text, "Старая запись");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn server_entries_keep_their_source_through_the_index() {
        let root = test_root("history-source");
        let mut recorded = entry(1);
        recorded.source = super::Source::Server;
        append(&root, &recorded).unwrap();
        append(&root, &entry(2)).unwrap();
        let history = History::load(&root);
        assert_eq!(history.row(0).unwrap().source(), super::Source::User);
        assert_eq!(history.row(1).unwrap().source(), super::Source::Server);
        assert_eq!(history.entry(1).unwrap().source, super::Source::Server);
        let _ = std::fs::remove_dir_all(root);
    }
}
