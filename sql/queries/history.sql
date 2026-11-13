-- name: count
SELECT COUNT(*) FROM history;

-- name: id_by_index
SELECT id FROM history ORDER BY at DESC, id DESC LIMIT 1 OFFSET ?1;

-- name: id_newest
SELECT id FROM history ORDER BY id DESC LIMIT 1;

-- name: row_by_index
-- The list shows a short preview and the number of appended pieces. Only the
-- text prefix is read, so a page never loads a whole entry.
SELECT h.id, h.at, substr(h.text, 1, 240), h.model,
    (SELECT COUNT(*) FROM history_parts p WHERE p.history_id = h.id),
    h.source, h.state, h.audio, h.group_name
FROM history h
ORDER BY h.at DESC, h.id DESC LIMIT 1 OFFSET ?1;

-- name: page_rows
-- Newest first; `{{filter}}` is empty or a WHERE clause with bound parameters.
SELECT h.id, h.at, substr(h.text, 1, 240), h.model,
    (SELECT COUNT(*) FROM history_parts p WHERE p.history_id = h.id),
    h.source, h.state, h.audio, h.group_name
FROM history h{{filter}}
ORDER BY h.at DESC, h.id DESC LIMIT ? OFFSET ?;

-- name: count_filtered
SELECT COUNT(*) FROM history h{{filter}};

-- name: update
UPDATE history SET at = ?1, text = ?2, model = ?3, source = ?4, state = ?5,
    audio = ?6, group_name = ?7, message_id = ?8 WHERE id = ?9;

-- name: parts_delete
DELETE FROM history_parts WHERE history_id = ?1;

-- name: parts_insert
INSERT INTO history_parts(history_id, position, at, text)
VALUES (?1, ?2, ?3, ?4);

-- name: bump
INSERT INTO kv(key, value) VALUES ('rev.history', '1')
ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1;

-- name: entry
SELECT id, at, text, model, source, state, audio, group_name, message_id
FROM history WHERE id = ?1;

-- name: parts_by_id
SELECT at, text FROM history_parts WHERE history_id = ?1 ORDER BY position;

-- name: insert
INSERT INTO history(at, text, model, source, state, audio, group_name, message_id)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);

-- name: clear_parts
DELETE FROM history_parts;

-- name: clear_history
DELETE FROM history;
