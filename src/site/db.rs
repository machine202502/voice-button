//! Storage for monologs and their messages.
//!
//! The phone's data is typed SQLite: one row per monolog, one row per message,
//! one client row. Stages and flags are columns, never a serialized blob. The
//! daemon keeps the working set in memory while it runs; the window reads pages
//! straight from the database and never loads a whole monolog.
//!
//! Stages are not free-form: `set_stage` refuses a move the state machine does
//! not allow, so a message can never jump back into "transcribing" after it is
//! done, and can never sit in a state that lies about the work left.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::monolog::{Monolog, Stage};
use crate::dates::DateFilter;
use std::path::{Path, PathBuf};
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::store::Store;

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

/// Everything the phone half owns, in the daemon's memory. Messages are not
/// kept here: they are read by id from SQLite, so a phone with 100 000 takes
/// costs the daemon a few bytes, not 100 000 structs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub client: Client,
    #[serde(default)]
    pub monologues: Vec<Monolog>,
}

/// A loaded phone database.
pub struct Db {
    root: PathBuf,
    store: Arc<Store>,
    data: Data,
    dirty_client: bool,
    dirty_monologs: BTreeSet<u64>,
}

impl Db {
    pub fn path(root: &Path) -> PathBuf {
        root.join("voice-button.sqlite3")
    }

    /// The raw PCM16 the phone sent, exactly as it arrived: a retry decodes
    /// this file instead of asking the phone to send the take again.
    pub fn audio_path(root: &Path, id: u64) -> PathBuf {
        root.join("site").join("audio").join(format!("{id}.pcm"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn open(root: &Path) -> Result<Self> {
        let store = Arc::new(Store::open(root)?);
        let mut data = Data::default();
        {
            let conn = store.conn();
            let client = conn
                .query_row(
                    crate::sql::query("site_db/client"),
                    [],
                    |row| {
                        Ok(Client {
                            token: row.get(0)?,
                            paired: row.get::<_, i64>(1)? != 0,
                            created: row.get(2)?,
                            expires: row.get(3)?,
                            last_seen: row.get(4)?,
                            last_ip: row.get(5)?,
                        })
                    },
                )
                .optional()?;
            data.client = client.unwrap_or_default();
            {
                let mut statement =
                    conn.prepare(crate::sql::query("site_db/monologs"))?;
                data.monologues = statement
                    .query_map([], |row| {
                        Ok(Monolog {
                            id: row.get::<_, i64>(0)? as u64,
                            name: row.get(1)?,
                            archived: row.get::<_, i64>(2)? != 0,
                            created: row.get(3)?,
                            count: 0,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            store,
            data,
            dirty_client: false,
            dirty_monologs: BTreeSet::new(),
        })
    }

    pub fn data(&self) -> &Data {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut Data {
        self.dirty_client = true;
        self.dirty_monologs.extend(self.data.monologues.iter().map(|monolog| monolog.id));
        &mut self.data
    }

    /// Write the changed client/monolog rows. Messages are written by their own
    /// methods, so nothing here has to keep a copy of them.
    pub fn save(&mut self) -> Result<()> {
        if !self.dirty_client && self.dirty_monologs.is_empty() {
            return Ok(());
        }
        let mut conn = self.store.conn();
        let transaction = conn.transaction()?;
        if self.dirty_client {
            let client = &self.data.client;
            transaction.execute(
                crate::sql::query("site_db/client_put"),
                params![
                    client.token,
                    client.paired as i64,
                    client.created,
                    client.expires,
                    client.last_seen,
                    client.last_ip
                ],
            )?;
        }
        for monolog in &self.data.monologues {
            if !self.dirty_monologs.contains(&monolog.id) {
                continue;
            }
            transaction.execute(
                crate::sql::query("site_db/monolog_put"),
                params![
                    monolog.id as i64,
                    monolog.name,
                    monolog.archived as i64,
                    monolog.created
                ],
            )?;
        }
        transaction.execute(crate::sql::query("site_db/bump"), [])?;
        transaction.commit()?;
        self.dirty_client = false;
        self.dirty_monologs.clear();
        Ok(())
    }

    // ---- client ---------------------------------------------------------

    pub fn start_pairing(&mut self, token: &str, now: &str) {
        self.dirty_client = true;
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
        self.dirty_client = true;
        self.data.client.paired = true;
        self.data.client.expires.clear();
        self.data.client.last_ip = ip.to_string();
        self.data.client.last_seen = now.to_string();
    }

    pub fn revoke(&mut self) {
        self.dirty_client = true;
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
        self.dirty_monologs.insert(id);
        monolog
    }

    /// Monologs with the number of messages each holds.
    pub fn monologs(&self, include_archived: bool) -> Vec<Monolog> {
        let conn = self.store.conn();
        self.data
            .monologues
            .iter()
            .filter(|monolog| include_archived || !monolog.archived)
            .map(|monolog| {
                let mut copy = monolog.clone();
                copy.count = conn
                    .query_row(
                        crate::sql::query("site_db/monolog_message_count"),
                        [monolog.id as i64],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or(0) as u32;
                copy
            })
            .collect()
    }

    pub fn monolog(&self, id: u64) -> Option<&Monolog> {
        self.data.monologues.iter().find(|monolog| monolog.id == id)
    }

    pub fn rename_monolog(&mut self, id: u64, name: &str) {
        self.dirty_monologs.insert(id);
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
        self.dirty_monologs.insert(id);
        if let Some(monolog) = self
            .data
            .monologues
            .iter_mut()
            .find(|monolog| monolog.id == id)
        {
            monolog.archived = archived;
        }
    }

    /// Remove one monolog and every message it owns, in one transaction.
    ///
    /// The message ids are returned so the caller can stop the engine and drop
    /// the audio files those messages kept; a monolog that does not exist
    /// answers `None`. Nothing is tombstoned here: the phone asked for the rows
    /// to be gone, and the revision tells the window to read the list again.
    pub fn delete_monolog(&mut self, id: u64) -> Option<Vec<u64>> {
        let mut conn = self.store.conn();
        let transaction = conn.transaction().ok()?;
        let exists: i64 = transaction
            .query_row(
                crate::sql::query("site_db/monolog_exists"),
                [id as i64],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if exists == 0 {
            return None;
        }
        let mut messages = Vec::new();
        if let Ok(mut statement) =
            transaction.prepare(crate::sql::query("site_db/monolog_message_ids"))
        {
            if let Ok(rows) = statement.query_map([id as i64], |row| row.get::<_, i64>(0)) {
                messages = rows
                    .filter_map(|row| row.ok())
                    .map(|value| value as u64)
                    .collect();
            }
        }
        let _ = transaction.execute(
            crate::sql::query("site_db/monolog_messages_delete"),
            [id as i64],
        );
        let _ = transaction.execute(crate::sql::query("site_db/monolog_delete"), [id as i64]);
        // The window keeps one counter for message and monolog writes; the
        // delete has to move it or the list would keep showing the monolog.
        let _ = transaction.execute(crate::sql::query("site_db/bump"), []);
        if transaction.commit().is_err() {
            return None;
        }
        drop(conn);
        self.data.monologues.retain(|monolog| monolog.id != id);
        self.dirty_monologs.remove(&id);
        Some(messages)
    }

    // ---- messages -------------------------------------------------------

    pub fn add_message(&mut self, monolog: u64, recorded: &str, now: &str) -> Message {
        let conn = self.store.conn();
        let id: i64 = conn
            .query_row(crate::sql::query("site_db/message_next_id"), [], |row| row.get(0))
            .unwrap_or(1);
        let message = Message {
            id: id as u64,
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
        let _ = conn.execute(
            crate::sql::query("site_db/message_insert"),
            params![message.id as i64, monolog as i64, message.recorded, message.received],
        );
        bump(&conn);
        message
    }

    pub fn message(&self, id: u64) -> Option<Message> {
        self.store
            .conn()
            .query_row(
                crate::sql::query("site_db/message_by_id"),
                [id as i64],
                read_message,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn messages(&self, monolog: Option<u64>) -> Vec<Message> {
        let conn = self.store.conn();
        let sql = if monolog.is_some() {
            crate::sql::query("site_db/messages_in_monolog")
        } else {
            crate::sql::query("site_db/messages_all")
        };
        let mut statement = match conn.prepare(sql) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = if let Some(monolog) = monolog {
            statement.query_map([monolog as i64], read_message)
        } else {
            statement.query_map([], read_message)
        };
        rows.map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    pub fn close_take(&mut self, id: u64) {
        let conn = self.store.conn();
        if conn
            .execute(crate::sql::query("site_db/message_close"), [id as i64])
            .unwrap_or(0)
            > 0
        {
            bump(&conn);
        }
    }

    pub fn set_stage(&mut self, id: u64, stage: Stage) -> bool {
        let conn = self.store.conn();
        let Some(current) = conn
            .query_row(
                crate::sql::query("site_db/message_stage"),
                [id as i64],
                |row| Ok(Stage::from_storage(&row.get::<_, String>(0)?)),
            )
            .optional()
            .ok()
            .flatten()
        else {
            return false;
        };
        if current == stage {
            return true;
        }
        // A take the engine is waiting for or working on may always be closed: a
        // queued message whose audio never arrived is silence, and one interrupted
        // mid-transcription may end. Without this, a take left open by a restart
        // could never leave "queued", which is exactly how a message used to hang.
        // A draft is not covered: nothing was ever sent for it.
        let closing = matches!(current, Stage::Queued | Stage::Transcribing) && stage.is_final();
        // A take whose audio never arrived (the phone started it and vanished) is
        // silence, whatever stage it was left in. Only silence: a draft cannot
        // become text out of nowhere.
        let abandoned = stage == Stage::Silent
            && matches!(
                current,
                Stage::Draft | Stage::Uploading | Stage::Queued | Stage::Transcribing
            );
        // A restart can leave a take mid-transcription; it goes back to the queue
        // instead of staying "transcribing" for ever.
        let requeueing = current == Stage::Transcribing && stage == Stage::Queued;
        if !current.can_move_to(stage) && !closing && !requeueing && !abandoned {
            crate::log::log(&format!(
                "site: message {id} cannot go from {current:?} to {stage:?}"
            ));
            return false;
        }
        let _ = conn.execute(
            crate::sql::query("site_db/message_stage_update"),
            params![stage.as_storage(), id as i64],
        );
        bump(&conn);
        true
    }

    pub fn add_seconds(&mut self, id: u64, seconds: f64) {
        let conn = self.store.conn();
        let Some((stage, duration)) = conn
            .query_row(
                crate::sql::query("site_db/message_stage_duration"),
                [id as i64],
                |row| {
                    Ok((
                        Stage::from_storage(&row.get::<_, String>(0)?),
                        row.get::<_, f64>(1)?,
                    ))
                },
            )
            .optional()
            .ok()
            .flatten()
        else {
            return;
        };
        let duration = duration + seconds;
        let stage = if stage == Stage::Draft && duration > 0.0 {
            Stage::Uploading
        } else {
            stage
        };
        let _ = conn.execute(
            crate::sql::query("site_db/message_duration_stage"),
            params![duration, stage.as_storage(), id as i64],
        );
        bump(&conn);
    }

    pub fn set_progress(&mut self, id: u64, text: &str, segments_done: u32) {
        let conn = self.store.conn();
        if conn
            .execute(
                crate::sql::query("site_db/message_text_segments"),
                params![text, segments_done as i64, id as i64],
            )
            .unwrap_or(0)
            > 0
        {
            bump(&conn);
        }
    }

    /// Remember that the original text reached the history. False when it was
    /// already written: a retry or a second completion must not add it twice.
    pub fn mark_history(&mut self, id: u64, at: &str) -> bool {
        let conn = self.store.conn();
        let changed = conn
            .execute(
                crate::sql::query("site_db/message_mark_history"),
                params![at, id as i64],
            )
            .unwrap_or(0);
        if changed > 0 {
            bump(&conn);
        }
        changed > 0
    }

    pub fn set_edited(&mut self, id: u64, text: &str, now: &str) {
        let conn = self.store.conn();
        if conn
            .execute(
                crate::sql::query("site_db/message_text_edited"),
                params![text, now, id as i64],
            )
            .unwrap_or(0)
            > 0
        {
            bump(&conn);
        }
    }

    pub fn delete(&mut self, id: u64) {
        let conn = self.store.conn();
        if conn
            .execute(
                crate::sql::query("site_db/message_delete"),
                [id as i64],
            )
            .unwrap_or(0)
            > 0
        {
            bump(&conn);
        }
    }

    /// Every message id and stage, for the tests that inspect the whole phone
    /// state. Never used to render a list.
    pub fn stages(&self) -> Vec<(u64, Stage)> {
        let conn = self.store.conn();
        let Ok(mut statement) = conn.prepare(crate::sql::query("site_db/message_stages")) else {
            return Vec::new();
        };
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    Stage::from_storage(&row.get::<_, String>(1)?),
                ))
            })
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    }

    pub fn message_count(&self, monolog: u64) -> usize {
        self.store
            .conn()
            .query_row(
                crate::sql::query("site_db/message_count"),
                [monolog as i64],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as usize)
            .unwrap_or(0)
    }

    /// One page of the feed, oldest-first like the screen. `before` asks for
    /// messages older than that cursor; `after` for newer ones. With neither,
    /// the newest page is returned.
    pub fn message_page(
        &self,
        monolog: u64,
        before: Option<(&str, u64)>,
        after: Option<(&str, u64)>,
        limit: usize,
    ) -> Vec<Message> {
        let conn = self.store.conn();
        let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match (before, after) {
            (Some((recorded, id)), _) => (
                crate::sql::query("site_db/message_page_before"),
                vec![
                    Box::new(monolog as i64),
                    Box::new(recorded.to_string()),
                    Box::new(id as i64),
                    Box::new(limit as i64),
                ],
            ),
            (_, Some((recorded, id))) => (
                crate::sql::query("site_db/message_page_after"),
                vec![
                    Box::new(monolog as i64),
                    Box::new(recorded.to_string()),
                    Box::new(id as i64),
                    Box::new(limit as i64),
                ],
            ),
            _ => (
                crate::sql::query("site_db/message_page_newest"),
                vec![Box::new(monolog as i64), Box::new(limit as i64)],
            ),
        };
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|value| value.as_ref()).collect();
        let newest_first = after.is_none();
        let mut rows: Vec<Message> = match conn.prepare(&sql) {
            Ok(mut statement) => statement
                .query_map(refs.as_slice(), read_message)
                .map(|rows| rows.flatten().collect())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        if newest_first {
            rows.reverse();
        }
        rows
    }

    /// One page of monologs, newest first, with counts from SQL.
    pub fn monologs_page(&self, archived: bool, offset: usize, limit: usize) -> (Vec<Monolog>, usize) {
        let conn = self.store.conn();
        // The API's own list is not date-filtered: the window passes empty bounds,
        // which the condition reads as "everything".
        let count_sql = crate::sql::render(
            "site_db/monologs_count_archived",
            &[("range", &DateFilter::clause("m.created", "m.created"))],
        );
        let total: i64 = conn
            .query_row(&count_sql, params!["", "", archived as i64], |row| row.get(0))
            .unwrap_or(0);
        let mut result = Vec::new();
        if let Ok(mut statement) = conn.prepare(crate::sql::query("site_db/monologs_page_rows")) {
            if let Ok(rows) = statement.query_map(
                params![archived as i64, limit as i64, offset as i64],
                |row| {
                    Ok(Monolog {
                        id: row.get::<_, i64>(0)? as u64,
                        name: row.get(1)?,
                        archived: row.get::<_, i64>(2)? != 0,
                        created: row.get(3)?,
                        count: row.get::<_, i64>(4)? as u32,
                    })
                },
            ) {
                result = rows.flatten().collect();
            }
        }
        (result, total.max(0) as usize)
    }

    pub fn monolog_count(&self, archived: bool) -> usize {
        let sql = crate::sql::render(
            "site_db/monologs_count_archived",
            &[("range", &DateFilter::clause("m.created", "m.created"))],
        );
        self.store
            .conn()
            .query_row(&sql, params!["", "", archived as i64], |row| {
                row.get::<_, i64>(0)
            })
            .map(|count| count as usize)
            .unwrap_or(0)
    }

    /// Newest non-empty text of a monolog, for its card.
    pub fn last_text(&self, monolog: u64) -> Option<Message> {
        self.store
            .conn()
            .query_row(
                crate::sql::query("site_db/message_last_text"),
                [monolog as i64],
                read_message,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn monolog_busy(&self, monolog: u64) -> bool {
        self.store
            .conn()
            .query_row(
                crate::sql::query("site_db/monolog_busy"),
                [monolog as i64],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .unwrap_or(false)
    }

    /// Messages the phone left in flight: their audio is on disk and must be
    /// finished or closed by the worker at startup.
    pub fn unfinished(&self) -> Vec<u64> {
        let conn = self.store.conn();
        let Ok(mut statement) = conn.prepare(crate::sql::query("site_db/messages_unfinished")) else {
            return Vec::new();
        };
        statement
            .query_map([], |row| row.get::<_, i64>(0))
            .map(|rows| rows.flatten().map(|id| id as u64).collect())
            .unwrap_or_default()
    }
}

/// Add one to the window's revision counter after a message write.
fn bump(conn: &rusqlite::Connection) {
    let _ = conn.execute(crate::sql::query("site_db/bump"), []);
}

fn read_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get::<_, i64>(0)? as u64,
        monolog: row.get::<_, i64>(1)? as u64,
        recorded: row.get(2)?,
        received: row.get(3)?,
        stage: Stage::from_storage(&row.get::<_, String>(4)?),
        text: row.get(5)?,
        original: row.get(6)?,
        edited: row.get(7)?,
        deleted: row.get::<_, i64>(8)? != 0,
        duration: row.get(9)?,
        segments_done: row.get::<_, i64>(10)? as u32,
        history_at: row.get(11)?,
        closed: row.get::<_, i64>(12)? != 0,
    })
}

/// The paired client record, without opening a monolog or a message.
pub fn client(store: &Store) -> Option<Client> {
    store
        .conn()
        .query_row(
            crate::sql::query("site_db/client"),
            [],
            |row| {
                Ok(Client {
                    token: row.get(0)?,
                    paired: row.get::<_, i64>(1)? != 0,
                    created: row.get(2)?,
                    expires: row.get(3)?,
                    last_seen: row.get(4)?,
                    last_ip: row.get(5)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
}

/// One monolog row as the window lists it. `preview` and `last_at` come from
/// the newest message, computed in SQL, so the list does not load a monolog's
/// messages to show a single line.
#[derive(Debug, Clone, Default)]
pub struct MonologRow {
    pub id: u64,
    pub name: String,
    pub archived: bool,
    pub created: String,
    pub count: u32,
    pub preview: String,
    pub last_at: String,
}

/// One message row as the window lists it: only what the row draws.
#[derive(Debug, Clone, Default)]
pub struct MessageRow {
    pub id: u64,
    pub recorded: String,
    pub stage: Stage,
    pub text: String,
}

/// The date condition for a monolog list: a monolog belongs to the day it was
/// started, which is what its card shows.
fn monolog_clause() -> String {
    DateFilter::clause("m.created", "m.created")
}

pub fn monolog_count(store: &Store, archived: bool, needle: &str, range: DateFilter) -> usize {
    let needle = needle.trim().to_lowercase();
    let conn = store.conn();
    let clause = monolog_clause();
    let result = if needle.is_empty() {
        let sql = crate::sql::render("site_db/monologs_count_archived", &[("range", &clause)]);
        conn.query_row(
            &sql,
            params![range.since(), range.until(), archived as i64],
            |row| row.get::<_, i64>(0),
        )
    } else {
        let sql = crate::sql::render("site_db/monologs_count_search", &[("range", &clause)]);
        conn.query_row(
            &sql,
            params![
                range.since(),
                range.until(),
                archived as i64,
                format!("%{needle}%")
            ],
            |row| row.get::<_, i64>(0),
        )
    };
    if let Err(error) = &result {
        crate::log::log(&format!("monolog search failed: {error}"));
    }
    result.map(|count| count as usize).unwrap_or(0)
}

pub fn monolog_page(
    store: &Store,
    archived: bool,
    needle: &str,
    range: DateFilter,
    offset: usize,
    limit: usize,
) -> Vec<MonologRow> {
    let needle = needle.trim().to_lowercase();
    let conn = store.conn();
    let clause = monolog_clause();
    let sql = if needle.is_empty() {
        crate::sql::render("site_db/monolog_page", &[("range", &clause)])
    } else {
        crate::sql::render("site_db/monolog_page_search", &[("range", &clause)])
    };
    let mut statement = match conn.prepare(&sql) {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    let mapper = |row: &rusqlite::Row<'_>| {
        Ok(MonologRow {
            id: row.get::<_, i64>(0)? as u64,
            name: row.get(1)?,
            archived: row.get::<_, i64>(2)? != 0,
            created: row.get(3)?,
            count: row.get::<_, i64>(4)? as u32,
            preview: row.get(5)?,
            last_at: row.get(6)?,
        })
    };
    let rows = if needle.is_empty() {
        statement.query_map(
            params![
                range.since(),
                range.until(),
                archived as i64,
                limit as i64,
                offset as i64
            ],
            mapper,
        )
    } else {
        statement.query_map(
            params![
                range.since(),
                range.until(),
                archived as i64,
                format!("%{needle}%"),
                limit as i64,
                offset as i64
            ],
            mapper,
        )
    };
    rows.map(|rows| rows.flatten().collect()).unwrap_or_default()
}

pub fn message_count(store: &Store, monolog: u64) -> usize {
    store
        .conn()
        .query_row(
            crate::sql::query("site_db/message_count"),
            [monolog as i64],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count as usize)
        .unwrap_or(0)
}

pub fn message_page(store: &Store, monolog: u64, offset: usize, limit: usize) -> Vec<MessageRow> {
    let conn = store.conn();
    let mut statement = match conn.prepare(crate::sql::query("site_db/message_rows")) {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    statement
        .query_map(params![monolog as i64, limit as i64, offset as i64], |row| {
            Ok(MessageRow {
                id: row.get::<_, i64>(0)? as u64,
                recorded: row.get(1)?,
                stage: Stage::from_storage(&row.get::<_, String>(2)?),
                text: row.get(3)?,
            })
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// Texts of the selected takes, in the order they were spoken. The selection
/// may span pages that are not loaded, so the query goes to the table.
pub fn message_texts(store: &Store, ids: &[u64]) -> Vec<String> {
    if ids.is_empty() {
        return Vec::new();
    }
    let marks = vec!["?"; ids.len()].join(",");
    let sql = crate::sql::render("site_db/message_texts", &[("marks", &marks)]);
    let conn = store.conn();
    let Ok(mut statement) = conn.prepare(&sql) else {
        return Vec::new();
    };
    let values: Vec<i64> = ids.iter().map(|id| *id as i64).collect();
    let refs: Vec<&dyn rusqlite::ToSql> = values.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
    statement
        .query_map(refs.as_slice(), |row| row.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}

/// One monolog by id, for the detail panel when the page it belongs to is not
/// loaded.
pub fn monolog_row(store: &Store, id: u64) -> Option<MonologRow> {
    let conn = store.conn();
    conn.query_row(
        crate::sql::query("site_db/monolog_row"),
        [id as i64],
        |row| {
            Ok(MonologRow {
                id: row.get::<_, i64>(0)? as u64,
                name: row.get(1)?,
                archived: row.get::<_, i64>(2)? != 0,
                created: row.get(3)?,
                count: row.get::<_, i64>(4)? as u32,
                preview: row.get(5)?,
                last_at: row.get(6)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

/// Every visible message id of a monolog, for "Select all". This is the one
/// action that deliberately touches every id; it does not load texts.
pub fn message_ids(store: &Store, monolog: u64) -> Vec<u64> {
    let conn = store.conn();
    let Ok(mut statement) = conn.prepare(crate::sql::query("site_db/message_ids")) else {
        return Vec::new();
    };
    statement
        .query_map([monolog as i64], |row| row.get::<_, i64>(0))
        .map(|rows| rows.flatten().map(|id| id as u64).collect())
        .unwrap_or_default()
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
    fn message_pages_walk_backwards_and_forwards() {
        let root = root_for("cursor");
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Лента", "2026-11-12T10:00:00+05:00");
        let mut ids = Vec::new();
        for index in 0..5 {
            let clock = format!("2026-11-12T10:00:0{index}+05:00");
            ids.push(db.add_message(monolog.id, &clock, &clock).id);
        }
        db.save().unwrap();

        let newest = db.message_page(monolog.id, None, None, 2);
        assert_eq!(newest.iter().map(|m| m.id).collect::<Vec<_>>(), ids[3..]);
        let older = db.message_page(monolog.id, Some((&newest[0].recorded, newest[0].id)), None, 2);
        assert_eq!(older.iter().map(|m| m.id).collect::<Vec<_>>(), ids[1..3]);
        let newer = db.message_page(
            monolog.id,
            None,
            Some((&newest[0].recorded, newest[0].id)),
            2,
        );
        assert_eq!(newer.iter().map(|m| m.id).collect::<Vec<_>>(), ids[4..]);
        assert_eq!(db.message_count(monolog.id), 5);
        let (page, total) = db.monologs_page(false, 0, 10);
        assert_eq!(total, 1);
        assert_eq!(page[0].count, 5);
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
        let message = reopened.messages(None)[0].clone();
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
        assert!(db.data().monologues.len() == 1, "the monolog is still stored");
        assert!(db.message(message.id).is_some(), "and so is the deleted take");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_take_left_open_by_a_restart_can_always_be_closed() {
        // The stuck-status bug: a message left in `queued` by a restart has no
        // audio, and settling it as silence was refused because the stage machine
        // did not allow `queued -> silent`. It then stayed "waiting" for ever.
        let root = root_for("recovery-stages");
        let mut db = Db::open(&root).unwrap();
        let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
        let queued = db.add_message(monolog.id, "2026-11-12T15:00:00+05:00", "2026-11-12T15:00:01+05:00");
        assert!(db.set_stage(queued.id, Stage::Uploading));
        assert!(db.set_stage(queued.id, Stage::Queued));
        assert!(
            db.set_stage(queued.id, Stage::Silent),
            "a queued take with no audio settles as silence"
        );

        let busy = db.add_message(monolog.id, "2026-11-12T15:05:00+05:00", "2026-11-12T15:05:01+05:00");
        assert!(db.set_stage(busy.id, Stage::Uploading));
        assert!(db.set_stage(busy.id, Stage::Queued));
        assert!(db.set_stage(busy.id, Stage::Transcribing));
        assert!(
            db.set_stage(busy.id, Stage::Queued),
            "a take interrupted mid-transcription goes back to the queue"
        );
        // A finished message still may not be reopened.
        assert!(db.set_stage(busy.id, Stage::Transcribing));
        assert!(db.set_stage(busy.id, Stage::Done));
        assert!(!db.set_stage(busy.id, Stage::Transcribing), "done stays done");
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

    #[test]
    fn the_monolog_list_can_be_narrowed_to_a_day() {
        // The list offers today / yesterday / this week / all time, the same
        // choices as the Telegram log. A monolog belongs to the day it was started.
        let root = root_for("monolog-days");
        let mut db = Db::open(&root).unwrap();
        let today = chrono::Local::now();
        let stamp = |days: u64| {
            let naive = (today.date_naive() - chrono::Days::new(days))
                .and_hms_opt(12, 0, 0)
                .unwrap();
            let moment = chrono::TimeZone::from_local_datetime(&chrono::Local, &naive)
                .earliest()
                .unwrap();
            moment
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S+00:00")
                .to_string()
        };
        db.create_monolog("Сегодня", &stamp(0));
        db.create_monolog("Вчера", &stamp(1));
        db.create_monolog("Старый", &stamp(30));
        db.save().unwrap();

        let store = Store::open(&root).unwrap();
        assert_eq!(monolog_count(&store, false, "", DateFilter::everything()), 3);
        assert_eq!(monolog_count(&store, false, "", DateFilter::today()), 1);
        assert_eq!(monolog_count(&store, false, "", DateFilter::yesterday()), 1);
        assert_eq!(monolog_count(&store, false, "", DateFilter::week()), 2);

        let today_rows = monolog_page(&store, false, "", DateFilter::today(), 0, 10);
        assert_eq!(today_rows.len(), 1);
        assert_eq!(today_rows[0].name, "Сегодня");
        let yesterday_rows = monolog_page(&store, false, "", DateFilter::yesterday(), 0, 10);
        assert_eq!(yesterday_rows.len(), 1, "yesterday is its own window");
        assert_eq!(yesterday_rows[0].name, "Вчера");
        // The search box narrows within the chosen day.
        assert_eq!(
            monolog_count(&store, false, "вчера", DateFilter::yesterday()),
            1
        );
        assert_eq!(monolog_count(&store, false, "вчера", DateFilter::today()), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_window_reads_pages_without_opening_a_monolog() {
        let root = root_for("pages");
        {
            let mut db = Db::open(&root).unwrap();
            let monolog = db.create_monolog("Работа", "2026-11-12T10:00:00+05:00");
            for index in 0..500 {
                let clock = format!("2026-11-12T15:00:{:02}+05:00", index % 60);
                let message = db.add_message(monolog.id, &clock, &clock);
                db.set_progress(message.id, &format!("take {index}"), 1);
            }
            db.save().unwrap();
        }
        let store = Store::open(&root).unwrap();
        assert_eq!(monolog_count(&store, false, "", DateFilter::everything()), 1);
        let page = monolog_page(&store, false, "", DateFilter::everything(), 0, 10);
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].count, 500);
        assert_eq!(monolog_count(&store, false, "Работа", DateFilter::everything()), 1);
        assert_eq!(monolog_count(&store, false, "nothing like this", DateFilter::everything()), 0);
        assert_eq!(message_count(&store, page[0].id), 500);
        assert_eq!(message_page(&store, page[0].id, 0, 200).len(), 200);
        assert_eq!(message_page(&store, page[0].id, 400, 200).len(), 100);
        let ids: Vec<u64> = message_page(&store, page[0].id, 0, 3)
            .iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(message_texts(&store, &ids).len(), 3);
        let _ = std::fs::remove_dir_all(root);
    }
}
