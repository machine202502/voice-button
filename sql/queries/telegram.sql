-- name: job_write
-- `{{table}}` and `{{key_column}}` are identifiers selected by the caller:
-- the current job lives in `telegram_current`, the queue in `telegram_queue`.
INSERT OR REPLACE INTO {{table}}({{key_column}}, update_id, chat_id, message_id,
    file_id, file_name, kind, size, seconds, done_seconds, stage, progress, text,
    error, sender, original_at, received_at, started_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18);

-- name: current
SELECT update_id, chat_id, message_id, file_id, file_name, kind, size,
    seconds, done_seconds, stage, progress, text, error, sender, original_at,
    received_at, started_at
FROM telegram_current WHERE id = 1;

-- name: queue
SELECT update_id, chat_id, message_id, file_id, file_name, kind, size,
    seconds, done_seconds, stage, progress, text, error, sender, original_at,
    received_at, started_at
FROM telegram_queue ORDER BY position;

-- name: kv_put
INSERT INTO kv(key, value) VALUES (?1, ?2)
ON CONFLICT(key) DO UPDATE SET value = excluded.value;

-- name: current_clear
DELETE FROM telegram_current;

-- name: queue_clear
DELETE FROM telegram_queue;

-- name: bump
INSERT INTO kv(key, value) VALUES ('rev.telegram', '1')
ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1;

-- name: chat_append
INSERT INTO telegram_chat(update_id, chat_id, message_id, at, sender, kind,
    file_name, seconds, text, error, added_at)
VALUES (0, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);

-- name: chat_page
-- The bounds follow the bot's own clock: a message belongs to the day it was
-- processed, not to the day printed on the file it carried. `at` is still read
-- for older rows that predate the column, so old data keeps working. The order
-- is the bot's own order of processing, never the date on the file.
SELECT id, chat_id, message_id, at, sender, kind, file_name,
    seconds, text, error
FROM telegram_chat
WHERE {{range}}
ORDER BY id LIMIT ?2 OFFSET ?3;

-- name: chat_count
SELECT COUNT(*) FROM telegram_chat WHERE {{range}};

-- name: chat_search_page
-- ?1 is the search pattern. The range bound is ?4 because limit and offset
-- take ?2 and ?3; `{{range}}` is rendered with that parameter number.
SELECT id, chat_id, message_id, at, sender, kind, file_name,
    seconds, text, error
FROM telegram_chat
WHERE (vb_lower(text) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(sender) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(file_name) LIKE vb_lower(?1) ESCAPE '\')
  AND {{range}}
ORDER BY id LIMIT ?2 OFFSET ?3;

-- name: chat_search_count
SELECT COUNT(*) FROM telegram_chat
WHERE (vb_lower(text) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(sender) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(file_name) LIKE vb_lower(?1) ESCAPE '\')
  AND {{range}};

-- name: test_chat_insert
INSERT INTO telegram_chat(chat_id, message_id, at, sender, kind, file_name,
    seconds, text, error, added_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);
