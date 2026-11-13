//! One SQLite file for everything the app keeps.
//!
//! The database is typed: history, phone monologs/messages and the Telegram
//! queue/chat each have their own table and columns. No row stores a JSON
//! payload; JSON is only a transport for HTTP calls and the settings file.
//!
//! Every list is read with `LIMIT/OFFSET`, so a window can ask for one page
//! even when a table holds tens of thousands of rows.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub struct Store {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("cannot create {}", root.display()))?;
        let path = root.join("voice-button.sqlite3");
        let conn = Connection::open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        // SQLite's own lower() only folds ASCII. Search has to work for
        // Russian, so every connection gets a Unicode-aware function.
        conn.create_scalar_function(
            "vb_lower",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |context| {
                let value: String = context.get(0)?;
                Ok(value.to_lowercase())
            },
        )?;
        // Opening a database that already has the schema must not run the DDL
        // again: the window opens several short-lived connections per second.
        let ready: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'history'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if ready.is_none() {
            conn.execute_batch(SCHEMA)?;
        }
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The single connection, locked. Modules keep their SQL next to the data
    /// they own instead of hiding it behind a generic document API.
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn();
        conn.query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| row.get(0))
            .optional()
            .context("cannot read the key/value table")
    }

    pub fn kv_put(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO kv(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    pub fn kv_delete(&self, key: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
        Ok(())
    }

    /// A counter stored in `kv`, bumped after a write so the window knows a
    /// table changed without watching file timestamps (WAL writes do not move
    /// the main file's mtime).
    pub fn bump(&self, key: &str) {
        let conn = self.conn();
        let _ = conn.execute(
            "INSERT INTO kv(key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1",
            [key],
        );
    }

    pub fn revision(&self, key: &str) -> u64 {
        self.kv_get(key)
            .ok()
            .flatten()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    /// Take a value and delete it in one transaction, so two processes cannot
    /// both consume the same request.
    pub fn kv_take(&self, key: &str) -> Option<String> {
        let conn = self.conn();
        let transaction = conn.unchecked_transaction().ok()?;
        let value: Option<String> = transaction
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| row.get(0))
            .optional()
            .ok()
            .flatten();
        if value.is_some() {
            let _ = transaction.execute("DELETE FROM kv WHERE key = ?1", [key]);
        }
        transaction.commit().ok()?;
        value
    }
}

/// The whole schema. There is no migration and no backward compatibility: the
/// app is installed fresh and owns these tables.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    text TEXT NOT NULL,
    model TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL DEFAULT 'user',
    state TEXT NOT NULL DEFAULT 'complete',
    audio TEXT,
    group_name TEXT,
    message_id INTEGER
);

CREATE TABLE IF NOT EXISTS history_parts (
    history_id INTEGER NOT NULL,
    position INTEGER NOT NULL,
    at TEXT NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (history_id, position)
);

CREATE TABLE IF NOT EXISTS telegram_current (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    update_id INTEGER NOT NULL,
    chat_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    file_id TEXT NOT NULL DEFAULT '',
    file_name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL DEFAULT 0,
    seconds REAL NOT NULL DEFAULT 0,
    done_seconds REAL NOT NULL DEFAULT 0,
    stage TEXT NOT NULL DEFAULT '',
    progress REAL NOT NULL DEFAULT 0,
    text TEXT NOT NULL DEFAULT '',
    error TEXT,
    sender TEXT NOT NULL DEFAULT '',
    original_at TEXT NOT NULL DEFAULT '',
    received_at TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS telegram_queue (
    position INTEGER PRIMARY KEY,
    update_id INTEGER NOT NULL,
    chat_id INTEGER NOT NULL,
    message_id INTEGER NOT NULL,
    file_id TEXT NOT NULL DEFAULT '',
    file_name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL DEFAULT 0,
    seconds REAL NOT NULL DEFAULT 0,
    done_seconds REAL NOT NULL DEFAULT 0,
    stage TEXT NOT NULL DEFAULT '',
    progress REAL NOT NULL DEFAULT 0,
    text TEXT NOT NULL DEFAULT '',
    error TEXT,
    sender TEXT NOT NULL DEFAULT '',
    original_at TEXT NOT NULL DEFAULT '',
    received_at TEXT NOT NULL DEFAULT '',
    started_at TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS telegram_chat (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    update_id INTEGER NOT NULL DEFAULT 0,
    chat_id INTEGER NOT NULL DEFAULT 0,
    message_id INTEGER NOT NULL DEFAULT 0,
    at TEXT NOT NULL DEFAULT '',
    sender TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT '',
    file_name TEXT NOT NULL DEFAULT '',
    seconds REAL NOT NULL DEFAULT 0,
    text TEXT NOT NULL DEFAULT '',
    error TEXT
);

CREATE INDEX IF NOT EXISTS telegram_chat_at ON telegram_chat(at, id);

CREATE TABLE IF NOT EXISTS site_client (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    token TEXT NOT NULL DEFAULT '',
    paired INTEGER NOT NULL DEFAULT 0,
    created TEXT NOT NULL DEFAULT '',
    expires TEXT NOT NULL DEFAULT '',
    last_seen TEXT NOT NULL DEFAULT '',
    last_ip TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS monologs (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    archived INTEGER NOT NULL DEFAULT 0,
    created TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS site_messages (
    id INTEGER PRIMARY KEY,
    monolog_id INTEGER NOT NULL,
    recorded TEXT NOT NULL DEFAULT '',
    received TEXT NOT NULL DEFAULT '',
    stage TEXT NOT NULL DEFAULT 'draft',
    text TEXT NOT NULL DEFAULT '',
    original TEXT NOT NULL DEFAULT '',
    edited TEXT,
    deleted INTEGER NOT NULL DEFAULT 0,
    duration REAL NOT NULL DEFAULT 0,
    segments_done INTEGER NOT NULL DEFAULT 0,
    history_at TEXT,
    closed INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS site_messages_monolog
    ON site_messages(monolog_id, recorded, id);

CREATE TABLE IF NOT EXISTS recordings (
    name TEXT PRIMARY KEY,
    id INTEGER NOT NULL DEFAULT 0,
    started TEXT NOT NULL DEFAULT '',
    interactor TEXT NOT NULL DEFAULT 'user',
    model TEXT NOT NULL DEFAULT '',
    seconds REAL NOT NULL DEFAULT 0,
    done INTEGER NOT NULL DEFAULT 0,
    failed_segments INTEGER NOT NULL DEFAULT 0,
    failed INTEGER NOT NULL DEFAULT 0,
    kept INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS site_network (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    port INTEGER NOT NULL DEFAULT 0,
    enabled INTEGER NOT NULL DEFAULT 0,
    firewall_rule TEXT NOT NULL DEFAULT '',
    firewall_created TEXT NOT NULL DEFAULT '',
    upnp_router TEXT NOT NULL DEFAULT '',
    upnp_external_port INTEGER NOT NULL DEFAULT 0,
    upnp_internal_ip TEXT NOT NULL DEFAULT '',
    upnp_added INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    updated TEXT NOT NULL DEFAULT ''
);
";
