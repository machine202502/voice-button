-- Replace the single `telegram.owner` with a list of accounts.
--
-- A new database gets this table from schema.sql; an older one is repaired
-- here: the list is created, the one owner it had becomes an allowed account,
-- and the key it lived in is dropped. The key was also what the reset button
-- deleted, so an empty value means nobody had access.
CREATE TABLE IF NOT EXISTS telegram_account (
    user_id INTEGER PRIMARY KEY,
    status TEXT NOT NULL DEFAULT 'pending',
    name TEXT NOT NULL DEFAULT '',
    username TEXT NOT NULL DEFAULT '',
    added_at TEXT NOT NULL DEFAULT '',
    last_seen_at TEXT NOT NULL DEFAULT ''
);

-- Every real database has `kv` (it holds the runtime state), but a hand-made or
-- very old file may not, and a migration that fails leaves the app unable to
-- start. Creating the table is a no-op where it already exists.
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO telegram_account(user_id, status, name, added_at, last_seen_at)
SELECT CAST(value AS INTEGER), 'allowed', '',
    strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
    strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM kv
WHERE key = 'telegram.owner' AND value <> '' AND CAST(value AS INTEGER) <> 0;

DELETE FROM kv WHERE key = 'telegram.owner';
