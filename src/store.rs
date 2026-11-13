//! One SQLite file for everything the app keeps.
//!
//! The database is typed: history, phone monologs/messages and the Telegram
//! queue/chat each have their own table and columns. No row stores a JSON
//! payload; JSON is only a transport for HTTP calls and the settings file.
//!
//! Every list is read with `LIMIT/OFFSET`, so a window can ask for one page
//! even when a table holds tens of thousands of rows.
//!
//! The DDL and every statement live in `sql/`; this module only opens the
//! connection, applies migrations and exposes it to the modules that own the
//! data.

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub struct Store {
    path: PathBuf,
    conn: Mutex<Connection>,
}

/// `PRAGMA user_version` as the migration cursor.
fn user_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(crate::sql::query("store/user_version"), [], |row| row.get(0))?)
}

/// Whether `table` already has a `column`, read from `PRAGMA table_info`.
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let sql = crate::sql::render("store/table_columns", &[("table", table)]);
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in rows {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Apply every migration whose 1-based index is above the stored version, each
/// in its own transaction, and advance `user_version` with it.
///
/// The first migration repairs a missing column. SQLite has no
/// `ADD COLUMN IF NOT EXISTS`, so the file holds both statements and Rust
/// decides whether they are needed: an already-repaired database just records
/// the version.
fn migrate(conn: &Connection) -> Result<()> {
    let current = user_version(conn)?;
    for (index, (name, sql)) in crate::sql::migrations().iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let needed = match *name {
            "001_telegram_added_at" => !column_exists(conn, "telegram_chat", "added_at")?,
            _ => true,
        };
        let transaction = conn.unchecked_transaction()?;
        if needed {
            transaction.execute_batch(sql)?;
        }
        transaction.pragma_update(None, "user_version", version)?;
        transaction.commit()?;
    }
    Ok(())
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
        // `history` is the marker of an initialized file.
        let ready: Option<i64> = conn
            .query_row(crate::sql::query("store/schema_ready"), [], |row| row.get(0))
            .optional()?;
        if ready.is_none() {
            conn.execute_batch(crate::sql::schema())?;
            // A new file already has the latest shape; migrations start above it.
            conn.pragma_update(None, "user_version", crate::sql::migrations().len() as i64)?;
        } else {
            migrate(&conn)?;
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
        conn.query_row(crate::sql::query("store/kv_get"), [key], |row| row.get(0))
            .optional()
            .context("cannot read the key/value table")
    }

    pub fn kv_put(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(
            crate::sql::query("store/kv_put"),
            rusqlite::params![key, value],
        )?;
        Ok(())
    }

    pub fn kv_delete(&self, key: &str) -> Result<()> {
        let conn = self.conn();
        conn.execute(crate::sql::query("store/kv_delete"), [key])?;
        Ok(())
    }

    /// A counter stored in `kv`, bumped after a write so the window knows a
    /// table changed without watching file timestamps (WAL writes do not move
    /// the main file's mtime).
    pub fn bump(&self, key: &str) {
        let conn = self.conn();
        let _ = conn.execute(crate::sql::query("store/kv_bump"), [key]);
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
            .query_row(crate::sql::query("store/kv_get"), [key], |row| row.get(0))
            .optional()
            .ok()
            .flatten();
        if value.is_some() {
            let _ = transaction.execute(crate::sql::query("store/kv_delete"), [key]);
        }
        transaction.commit().ok()?;
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_database_gets_the_columns_it_is_missing() {
        // A database exactly as an earlier release left it: the table exists, but
        // it has no `added_at`, and every query that names it fails.
        let root = std::env::temp_dir().join(format!(
            "vb-store-repair-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        {
            let conn = Connection::open(root.join("voice-button.sqlite3")).unwrap();
            conn.execute_batch(crate::sql::query("store/test_legacy_fixture"))
                .unwrap();
        }

        let store = Store::open(&root).unwrap();
        let conn = store.conn();
        let backfilled: String = conn
            .query_row(
                crate::sql::query("store/test_legacy_added_at"),
                [],
                |row| row.get(0),
            )
            .expect("the column exists after the repair");
        assert_eq!(backfilled, "2026-11-01T10:00:00Z", "backfilled from the date it had");

        // A new message can be stored, which is what the log needs.
        conn.execute(crate::sql::query("store/test_legacy_insert"), [])
            .expect("insert with the column named");
        let count: i64 = conn
            .query_row(crate::sql::query("store/test_legacy_count"), [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_database_starts_at_the_latest_migration() {
        let root = std::env::temp_dir().join(format!(
            "vb-store-fresh-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let store = Store::open(&root).unwrap();
        let version: i64 = store
            .conn()
            .query_row(crate::sql::query("store/user_version"), [], |row| row.get(0))
            .unwrap();
        // The schema already has the latest shape, so no migration may run on
        // it: the cursor starts above every migration.
        assert_eq!(version, crate::sql::migrations().len() as i64);
        let _ = std::fs::remove_dir_all(&root);
    }
}
