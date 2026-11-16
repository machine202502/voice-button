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
-- ?1 is the lower bound, ?2 the upper one (both may be empty), then the page.
SELECT id, chat_id, message_id, at, sender, kind, file_name,
    seconds, text, error
FROM telegram_chat
WHERE {{range}}
ORDER BY id LIMIT ?3 OFFSET ?4;

-- name: chat_count
SELECT COUNT(*) FROM telegram_chat WHERE {{range}};

-- name: chat_search_page
-- ?1 is the search pattern, ?2 and ?3 the date bounds, then the page.
SELECT id, chat_id, message_id, at, sender, kind, file_name,
    seconds, text, error
FROM telegram_chat
WHERE (vb_lower(text) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(sender) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(file_name) LIKE vb_lower(?1) ESCAPE '\')
  AND {{range}}
ORDER BY id LIMIT ?4 OFFSET ?5;

-- name: chat_search_count
SELECT COUNT(*) FROM telegram_chat
WHERE (vb_lower(text) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(sender) LIKE vb_lower(?1) ESCAPE '\'
    OR vb_lower(file_name) LIKE vb_lower(?1) ESCAPE '\')
  AND {{range}};

-- name: accounts
-- What the window shows and the bot checks: the allowed accounts first, then
-- the requests that still wait for the owner, each in arrival order.
SELECT user_id, status, name, username, added_at, last_seen_at
FROM telegram_account
ORDER BY CASE status WHEN 'allowed' THEN 0 ELSE 1 END, added_at, user_id;

-- name: account_put
-- A message from an account refreshes the name it shows and when it was last
-- seen; a stranger who writes /start becomes a pending row. `status` and
-- `added_at` never change here — only the window's decision changes them.
INSERT INTO telegram_account(user_id, status, name, username, added_at, last_seen_at)
VALUES (?1, 'pending', ?2, ?3, ?4, ?4)
ON CONFLICT(user_id) DO UPDATE SET
    name = excluded.name,
    username = excluded.username,
    last_seen_at = excluded.last_seen_at;

-- name: account_allow
UPDATE telegram_account SET status = 'allowed' WHERE user_id = ?1;

-- name: account_remove
DELETE FROM telegram_account WHERE user_id = ?1;

-- name: account_clear
-- "Remove all accounts" takes access away from everyone who had it and leaves
-- the requests that still wait for a decision.
DELETE FROM telegram_account WHERE status = 'allowed';

-- name: account_clear_pending
-- "Decline all" forgets every request at once. A forgotten account is not
-- blocked: its next /start inserts a new pending row.
DELETE FROM telegram_account WHERE status = 'pending';

-- name: test_chat_insert
INSERT INTO telegram_chat(chat_id, message_id, at, sender, kind, file_name,
    seconds, text, error, added_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);
