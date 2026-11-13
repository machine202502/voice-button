-- name: schema_ready
SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'history';

-- name: user_version
PRAGMA user_version;

-- name: kv_get
SELECT value FROM kv WHERE key = ?1;

-- name: kv_put
INSERT INTO kv(key, value) VALUES (?1, ?2)
ON CONFLICT(key) DO UPDATE SET value = excluded.value;

-- name: kv_delete
DELETE FROM kv WHERE key = ?1;

-- name: kv_bump
INSERT INTO kv(key, value) VALUES (?1, '1')
ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1;

-- name: table_columns
PRAGMA table_info({{table}});

-- name: test_legacy_fixture
-- The shape an earlier release left behind: the table exists, but it has no
-- `added_at`. Used by `an_old_database_gets_the_columns_it_is_missing`.
CREATE TABLE history (id INTEGER PRIMARY KEY, at TEXT);
CREATE TABLE telegram_chat (
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
    error TEXT);
INSERT INTO telegram_chat(chat_id, message_id, at, text)
    VALUES (1, 5, '2026-11-01T10:00:00Z', 'old');

-- name: test_legacy_added_at
SELECT added_at FROM telegram_chat WHERE message_id = 5;

-- name: test_legacy_insert
INSERT INTO telegram_chat(chat_id, message_id, at, text, added_at)
VALUES (1, 6, '2026-11-02T10:00:00Z', 'new', '2026-11-02T10:00:00Z');

-- name: test_legacy_count
SELECT COUNT(*) FROM telegram_chat;

-- name: test_scale_history_insert
INSERT INTO history(at, text, model, source, state)
VALUES (?1, ?2, 'model', 'user', 'complete');

-- name: test_scale_chat_insert
INSERT INTO telegram_chat(at, sender, kind, text)
VALUES ('2026-11-12T10:00:00+05:00', 'Owner', 'voice', ?1);

-- name: test_scale_message_insert
INSERT INTO site_messages(id, monolog_id, recorded, stage, text)
VALUES (?1, ?2, ?3, 'done', ?4);
