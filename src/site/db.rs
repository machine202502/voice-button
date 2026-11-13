//! Storage for monologs and their messages.
//!
//! One file (`site/db.json`) holds everything the phone owns: the paired
//! client, the monologs, the messages, and the stage each message is in. The
//! file is small, so it is written whole on every change; that keeps the code
//! obvious and makes a torn write impossible to observe as a partial state.
//!
//! Stages are not free-form: `set_stage` refuses a move the state machine does
//! not allow, so a message can never jump back into "transcribing" after it is
//! done, and can never sit in a state that lies about the work left.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::monolog::{Monolog, Stage};
use std::path::{Path, PathBuf};

/// The paired phone. One at a time: a new pairing replaces the old one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub paired: bool,
    #[serde(default)]
    pub created: String,
    /// When the unused pairing link stops working.
    #[serde(default)]
    pub expires: String,
    #[serde(default)]
    pub last_seen: String,
    #[serde(default)]
    pub last_ip: String,
}

/// One spoken take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: u64,
    pub monolog: u64,
    /// The phone's own clock when the take started.
    pub recorded: String,
    #[serde(default)]
    pub received: String,
    #[serde(default)]
    pub stage: Stage,
    #[serde(default)]
    pub text: String,
    /// The text exactly as the engine produced it.
    #[serde(default)]
    pub original: String,
    #[serde(default)]
    pub edited: Option<String>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub segments_done: u32,
    /// `HistoryEntry::at` once the original reached the history.
    #[serde(default)]
    pub history_at: Option<String>,
    /// The phone said `finish`; late audio must be refused even while the
    /// earlier segments are still being transcribed.
    #[serde(default)]
    pub closed: bool,
}

/// Everything the phone half owns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub client: Client,
    #[serde(default)]
    pub monologues: Vec<Monolog>,
    #[serde(default)]
    pub messages: Vec<Message>,
}

/// A loaded `db.json`.
pub struct Db {
    root: PathBuf,
    data: Data,
}

impl Db {
    pub fn path(root: &Path) -> PathBuf {
        root.join("site").join("db.json")
    }

    /// The raw PCM16 the phone sent, exactly as it arrived: a retry decodes
    /// this file instead of asking the phone to send the take again.
    pub fn audio_path(root: &Path, id: u64) -> PathBuf {
        root.join("site").join("audio").join(format!("{id}.pcm"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Open the file.
    ///
    /// This layer has no old vocabulary and no migrations: a file written by
    /// another version is not parsed, not guessed at. It is renamed and kept,
    /// and the phone starts with an empty database instead of refusing to
    /// start at all. Nothing is deleted, and the reason reaches the log.
    pub fn open(root: &Path) -> Result<Self> {
        let path = Self::path(root);
        let data = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Data>(&text) {
                Ok(data) => data,
                Err(error) => {
                    match set_aside(&path) {
                        Some(kept) => crate::log::log(&format!(
                            "site: db.json does not fit this version ({error}); kept as {} and starting empty",
                            kept.display()
                        )),
                        None => crate::log::log(&format!(
                            "site: db.json does not fit this version ({error}) and could not be moved aside; starting empty"
                        )),
                    }
                    Data::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(error) => return Err(error).context("cannot read db.json"),
        };
        Ok(Self {
            root: root.to_path_buf(),
            data,
        })
    }

    pub fn data(&self) -> &Data {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut Data {
        &mut self.data
    }

    /// Write the whole file. The temporary name makes a crash mid-write leave
    /// the previous file intact instead of half a new one.
    pub fn save(&self) -> Result<()> {
        let path = Self::path(&self.root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).context("cannot create site/")?;
        }
        let text = serde_json::to_string_pretty(&self.data).context("cannot encode db.json")?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).context("cannot write db.json")?;
        std::fs::rename(&temporary, &path).context("cannot replace db.json")?;
        Ok(())
    }

    // ---- client ---------------------------------------------------------

    pub fn start_pairing(&mut self, token: &str, now: &str) {
        self.data.client.token = token.to_string();
        self.data.client.paired = false;
        self.data.client.created = now.to_string();
        self.data.client.expires = expires_at(now);
        self.data.client.last_ip.clear();
        self.data.client.last_seen.clear();
    }

    pub fn pairing_alive(&self, now: &str) -> bool {
        let client = &self.data.client;
        !client.paired && !client.token.is_empty() && client.expires.as_str() > now
    }

    pub fn mark_paired(&mut self, ip: &str, now: &str) {
        self.data.client.paired = true;
        self.data.client.expires.clear();
        self.data.client.last_ip = ip.to_string();
        self.data.client.last_seen = now.to_string();
    }

    pub fn revoke(&mut self) {
        self.data.client.paired = false;
        self.data.client.token.clear();
        self.data.client.expires.clear();
        self.data.client.last_ip.clear();
        self.data.client.last_seen.clear();
    }

    pub fn token_matches(&self, token: &str) -> bool {
        !self.data.client.token.is_empty()
            && !token.is_empty()
            && self.data.client.token.as_bytes() == token.as_bytes()
    }

    // ---- monologs -------------------------------------------------------

    pub fn create_monolog(&mut self, name: &str, now: &str) -> Monolog {
        let id = self
            .data
            .monologues
            .iter()
            .map(|monolog| monolog.id)
            .max()
            .unwrap_or(0)
            + 1;
        let monolog = Monolog {
            id,
            name: name.trim().to_string(),
            archived: false,
            created: now.to_string(),
            count: 0,
        };
        self.data.monologues.push(monolog.clone());
        monolog
    }

    /// Monologs with the number of messages each holds.
    pub fn monologs(&self, include_archived: bool) -> Vec<Monolog> {
        self.data
            .monologues
            .iter()
            .filter(|monolog| include_archived || !monolog.archived)
            .map(|monolog| {
                let mut copy = monolog.clone();
                copy.count = self
                    .data
                    .messages
                    .iter()
                    .filter(|message| message.monolog == monolog.id && !message.deleted)
                    .count() as u32;
                copy
            })
            .collect()
    }

    pub fn monolog(&self, id: u64) -> Option<&Monolog> {
        self.data.monologues.iter().find(|monolog| monolog.id == id)
    }

    pub fn rename_monolog(&mut self, id: u64, name: &str) {
        if let Some(monolog) = self
            .data
            .monologues
            .iter_mut()
            .find(|monolog| monolog.id == id)
        {
            monolog.name = name.trim().to_string();
        }
    }

    pub fn archive_monolog(&mut self, id: u64, archived: bool) {
        if let Some(monolog) = self
            .data
            .monologues
            .iter_mut()
            .find(|monolog| monolog.id == id)
        {
            monolog.archived = archived;
        }
    }

    // ---- messages -------------------------------------------------------

    pub fn add_message(&mut self, monolog: u64, recorded: &str, now: &str) -> Message {
        let id = self
            .data
            .messages
            .iter()
            .map(|message| message.id)
            .max()
            .unwrap_or(0)
            + 1;
        let message = Message {
            id,
            monolog,
            recorded: recorded.to_string(),
            received: now.to_string(),
            stage: Stage::Draft,
            text: String::new(),
            original: String::new(),
            edited: None,
            deleted: false,
            duration: 0.0,
            segments_done: 0,
            history_at: None,
            closed: false,
        };
        self.data.messages.push(message.clone());
        message
    }

    pub fn message(&self, id: u64) -> Option<&Message> {
        self.data.messages.iter().find(|message| message.id == id)
    }

    pub fn messages(&self, monolog: Option<u64>) -> Vec<&Message> {
        let mut list: Vec<&Message> = self
            .data
            .messages
            .iter()
            .filter(|message| !message.deleted)
            .filter(|message| monolog.map(|id| message.monolog == id).unwrap_or(true))
            .collect();
        list.sort_by(|left, right| left.recorded.cmp(&right.recorded));
        list
    }

    /// Move a message to another stage. A move the machine does not allow is
    /// refused and logged: this is the one place that can corrupt the story the
    /// interface tells, so it is the one place that checks.
    /// Remember that the phone closed the take, whatever stage it shows.
    pub fn close_take(&mut self, id: u64) {
        if let Some(message) = self.data.messages.iter_mut().find(|item| item.id == id) {
            message.closed = true;
        }
    }

    pub fn set_stage(&mut self, id: u64, stage: Stage) -> bool {
        let Some(message) = self
            .data
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        else {
            return false;
        };
        if message.stage == stage {
            return true;
        }
        if !message.stage.can_move_to(stage) {
            crate::log::log(&format!(
                "site: message {id} cannot go from {:?} to {:?}",
                message.stage, stage
            ));
            return false;
        }
        message.stage = stage;
        true
    }

    pub fn add_seconds(&mut self, id: u64, seconds: f64) {
        if let Some(message) = self
            .data
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        {
            message.duration += seconds;
            if message.stage == Stage::Draft && message.duration > 0.0 {
                let _ = message.stage.can_move_to(Stage::Uploading);
                message.stage = Stage::Uploading;
            }
        }
    }

    pub fn set_progress(&mut self, id: u64, text: &str, segments_done: u32) {
        if let Some(message) = self
            .data
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        {
            message.text = text.to_string();
            message.segments_done = segments_done;
        }
    }

    /// Remember that the original text reached the history. False when it was
    /// already written: a retry or a second completion must not add it twice.
    pub fn mark_history(&mut self, id: u64, at: &str) -> bool {
        let Some(message) = self
            .data
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        else {
            return false;
        };
        if message.history_at.is_some() {
            return false;
        }
        message.history_at = Some(at.to_string());
        true
    }

    pub fn set_edited(&mut self, id: u64, text: &str, now: &str) {
        if let Some(message) = self
            .data
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        {
            message.text = text.to_string();
            message.edited = Some(now.to_string());
        }
    }

    pub fn delete(&mut self, id: u64) {
        if let Some(message) = self
            .data
            .messages
            .iter_mut()
            .find(|message| message.id == id)
        {
            message.deleted = true;
            message.stage = Stage::Cancelled;
        }
    }

    /// Messages the phone left in flight: their audio is on disk and must be
    /// finished or closed by the worker at startup.
    pub fn unfinished(&self) -> Vec<u64> {
        self.data
            .messages
            .iter()
            .filter(|message| message.stage.is_open())
            .map(|message| message.id)
            .collect()
    }
}

/// How long a pairing link may be used.
pub const PAIRING_MINUTES: i64 = 5;

pub fn expires_at(now: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(now) {
        Ok(stamp) => (stamp + chrono::Duration::minutes(PAIRING_MINUTES)).to_rfc3339(),
        // A stamp nobody can read must not keep a link alive.
        Err(_) => String::new(),
    }
}

/// Rename a file this version cannot read, keeping it for the user. Returns
/// where it went, or `None` when it could not be moved (a locked file stays
/// where it is and the caller still starts empty).
fn set_aside(path: &Path) -> Option<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = format!(
        "db.json.old-{stamp}-{}",
        std::process::id()
    );
    let target = path.parent()?.join(name);
    std::fs::rename(path, &target).ok().map(|_| target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_for(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("vb-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_database_from_another_version_is_set_aside_not_fatal() {
        let root = root_for("old-db");
        std::fs::create_dir_all(root.join("site")).unwrap();
        // The shape the 0.4.0 layer wrote: groups, messages with a `group`.
        let old = r#"{"client":{},"groups":[{"id":1}],"messages":[{"id":1,"group":1,"status":"live"}]}"#;
        std::fs::write(Db::path(&root), old).unwrap();
        let db = Db::open(&root).expect("an old file must not stop the phone");
        assert!(db.data().monologues.is_empty());
        assert!(db.data().messages.is_empty());
        let kept: Vec<String> = std::fs::read_dir(root.join("site"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with("db.json.old-"))
            .collect();
        assert_eq!(kept.len(), 1, "the old file is kept, not deleted: {kept:?}");
        assert_eq!(
            std::fs::read_to_string(root.join("site").join(&kept[0])).unwrap(),
            old
        );
        // The next start is clean and does not set anything aside again.
        let again = Db::open(&root).unwrap();
        assert!(again.data().messages.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_message_walks_the_happy_path() {
        let root = root_for("happy");
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let message = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
        assert_eq!(message.stage, Stage::Draft);

        db.add_seconds(message.id, 1.5);
        assert_eq!(db.message(message.id).unwrap().stage, Stage::Uploading);
        assert!(db.set_stage(message.id, Stage::Queued));
        assert!(db.set_stage(message.id, Stage::Transcribing));
        db.set_progress(message.id, "полтекста", 1);
        assert!(db.set_stage(message.id, Stage::Done));
        assert_eq!(db.message(message.id).unwrap().text, "полтекста");
        assert!(!db.set_stage(message.id, Stage::Transcribing), "done is final");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_illegal_move_is_refused() {
        let root = root_for("illegal");
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let message = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
        // Straight to done from a draft: the audio was never even sent.
        assert!(!db.set_stage(message.id, Stage::Done));
        assert_eq!(db.message(message.id).unwrap().stage, Stage::Draft);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_saved_database_reads_back_whole() {
        let root = root_for("roundtrip");
        {
            let mut db = Db::open(&root).unwrap();
            let monolog = db.create_monolog("Дом", "2026-11-12T10:00:00+05:00");
            let message = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
            db.add_seconds(message.id, 2.0);
            db.set_stage(message.id, Stage::Queued);
            db.save().unwrap();
        }
        let reopened = Db::open(&root).unwrap();
        assert_eq!(reopened.monologs(true).len(), 1);
        let message = reopened.messages(None)[0];
        assert_eq!(message.stage, Stage::Queued);
        assert!((message.duration - 2.0).abs() < 0.001);
        let _ = std::fs::remove_dir_all(root);
    }


    #[test]
    fn deleted_messages_leave_the_list_but_not_the_file() {
        let root = root_for("delete");
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let message = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
        db.delete(message.id);
        assert!(db.messages(None).is_empty(), "a deleted take is hidden");
        assert_eq!(db.message(message.id).unwrap().stage, Stage::Cancelled);
        assert!(db.data().messages.len() == 1, "but it is still stored");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unfinished_messages_are_reported_for_the_worker() {
        let root = root_for("unfinished");
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let first = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
        let second = db.add_message(monolog.id, "2026-11-12T15:05:00+05:00", "2026-11-12T15:05:01+05:00");
        db.set_stage(second.id, Stage::Cancelled);
        assert_eq!(db.unfinished(), vec![first.id]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_pairing_link_lives_five_minutes() {
        let mut db = Db::open(&root_for("pairing")).unwrap();
        db.start_pairing("token", "2026-11-12T10:00:00+05:00");
        assert!(db.pairing_alive("2026-11-12T10:04:00+05:00"));
        assert!(!db.pairing_alive("2026-11-12T10:06:00+05:00"));
        db.mark_paired("5.79.1.2", "2026-11-12T10:01:00+05:00");
        // Once paired the token keeps working, and the deadline is gone.
        assert!(!db.pairing_alive("2026-11-12T10:02:00+05:00"));
        assert!(db.data().client.paired);
        assert!(db.data().client.expires.is_empty());
    }
}
