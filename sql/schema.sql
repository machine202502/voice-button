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
    error TEXT,
    -- When the bot handled the message, in UTC. The `at` above is the date of the
    -- original file, which can be years old; the log and its date filter follow
    -- the processing order, so they need this one.
    added_at TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS telegram_chat_at ON telegram_chat(at, id);
CREATE INDEX IF NOT EXISTS telegram_chat_added ON telegram_chat(added_at, id);

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
