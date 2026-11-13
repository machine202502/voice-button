-- name: client
SELECT token, paired, created, expires, last_seen, last_ip
FROM site_client WHERE id = 1;

-- name: monologs
SELECT id, name, archived, created FROM monologs ORDER BY id;

-- name: client_put
INSERT INTO site_client(id, token, paired, created, expires, last_seen, last_ip)
VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
ON CONFLICT(id) DO UPDATE SET token = excluded.token, paired = excluded.paired,
    created = excluded.created, expires = excluded.expires,
    last_seen = excluded.last_seen, last_ip = excluded.last_ip;

-- name: monolog_put
INSERT INTO monologs(id, name, archived, created)
VALUES (?1, ?2, ?3, ?4)
ON CONFLICT(id) DO UPDATE SET name = excluded.name,
    archived = excluded.archived, created = excluded.created;

-- name: bump
INSERT INTO kv(key, value) VALUES ('rev.site', '1')
ON CONFLICT(key) DO UPDATE SET value = CAST(value AS INTEGER) + 1;

-- name: monolog_message_count
SELECT COUNT(*) FROM site_messages
WHERE monolog_id = ?1 AND deleted = 0;

-- name: monolog_exists
SELECT COUNT(*) FROM monologs WHERE id = ?1;

-- name: monolog_message_ids
SELECT id FROM site_messages WHERE monolog_id = ?1;

-- name: monolog_messages_delete
DELETE FROM site_messages WHERE monolog_id = ?1;

-- name: monolog_delete
DELETE FROM monologs WHERE id = ?1;

-- name: message_next_id
SELECT COALESCE(MAX(id), 0) + 1 FROM site_messages;

-- name: message_insert
INSERT INTO site_messages(id, monolog_id, recorded, received, stage)
VALUES (?1, ?2, ?3, ?4, 'draft');

-- name: message_by_id
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages WHERE id = ?1;

-- name: messages_in_monolog
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages WHERE deleted = 0 AND monolog_id = ?1
ORDER BY recorded, id;

-- name: messages_all
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages WHERE deleted = 0 ORDER BY recorded, id;

-- name: message_close
UPDATE site_messages SET closed = 1 WHERE id = ?1;

-- name: message_stage
SELECT stage FROM site_messages WHERE id = ?1;

-- name: message_stage_update
UPDATE site_messages SET stage = ?1 WHERE id = ?2;

-- name: message_stage_duration
SELECT stage, duration FROM site_messages WHERE id = ?1;

-- name: message_duration_stage
UPDATE site_messages SET duration = ?1, stage = ?2 WHERE id = ?3;

-- name: message_text_segments
UPDATE site_messages SET text = ?1, segments_done = ?2 WHERE id = ?3;

-- name: message_mark_history
UPDATE site_messages SET history_at = ?1
WHERE id = ?2 AND history_at IS NULL;

-- name: message_text_edited
UPDATE site_messages SET text = ?1, edited = ?2 WHERE id = ?3;

-- name: message_delete
UPDATE site_messages SET deleted = 1, stage = 'cancelled' WHERE id = ?1;

-- name: message_stages
SELECT id, stage FROM site_messages ORDER BY id;

-- name: message_count
SELECT COUNT(*) FROM site_messages WHERE monolog_id = ?1 AND deleted = 0;

-- name: message_page_before
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages
WHERE deleted = 0 AND monolog_id = ?1
AND (recorded < ?2 OR (recorded = ?2 AND id < ?3))
ORDER BY recorded DESC, id DESC LIMIT ?4;

-- name: message_page_after
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages
WHERE deleted = 0 AND monolog_id = ?1
AND (recorded > ?2 OR (recorded = ?2 AND id > ?3))
ORDER BY recorded ASC, id ASC LIMIT ?4;

-- name: message_page_newest
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages
WHERE deleted = 0 AND monolog_id = ?1
ORDER BY recorded DESC, id DESC LIMIT ?2;

-- name: monologs_count_archived
-- ?1 and ?2 are the date bounds (both may be empty), ?3 the archive flag.
SELECT COUNT(*) FROM monologs m WHERE {{range}} AND m.archived = ?3;

-- name: monologs_page_rows
SELECT m.id, m.name, m.archived, m.created,
    (SELECT COUNT(*) FROM site_messages s
     WHERE s.monolog_id = m.id AND s.deleted = 0)
FROM monologs m WHERE m.archived = ?1
ORDER BY m.created DESC, m.id DESC LIMIT ?2 OFFSET ?3;

-- name: message_last_text
SELECT id, monolog_id, recorded, received, stage, text, original, edited,
    deleted, duration, segments_done, history_at, closed
FROM site_messages
WHERE monolog_id = ?1 AND deleted = 0 AND trim(text) <> ''
ORDER BY recorded DESC, id DESC LIMIT 1;

-- name: monolog_busy
SELECT EXISTS(SELECT 1 FROM site_messages
WHERE monolog_id = ?1 AND deleted = 0
  AND stage IN ('draft','uploading','queued','transcribing'));

-- name: messages_unfinished
SELECT id FROM site_messages
WHERE stage IN ('draft', 'uploading', 'queued', 'transcribing') ORDER BY id;

-- name: monologs_count_search
-- ?1 and ?2 are the date bounds, ?3 the archive flag, ?4 the pattern.
SELECT COUNT(*) FROM monologs m WHERE {{range}} AND m.archived = ?3
  AND (vb_lower(m.name) LIKE ?4 ESCAPE '\' OR vb_lower(COALESCE((
      SELECT text FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
      ORDER BY s.recorded DESC, s.id DESC LIMIT 1), '')) LIKE ?4 ESCAPE '\');

-- name: monolog_page
SELECT m.id, m.name, m.archived, m.created,
    (SELECT COUNT(*) FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0),
    COALESCE((SELECT text FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
        ORDER BY s.recorded DESC, s.id DESC LIMIT 1), ''),
    COALESCE((SELECT recorded FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
        ORDER BY s.recorded DESC, s.id DESC LIMIT 1), '')
FROM monologs m WHERE {{range}} AND m.archived = ?3
ORDER BY m.created DESC, m.id DESC LIMIT ?4 OFFSET ?5;

-- name: monolog_page_search
SELECT m.id, m.name, m.archived, m.created,
    (SELECT COUNT(*) FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0),
    COALESCE((SELECT text FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
        ORDER BY s.recorded DESC, s.id DESC LIMIT 1), ''),
    COALESCE((SELECT recorded FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
        ORDER BY s.recorded DESC, s.id DESC LIMIT 1), '')
FROM monologs m WHERE {{range}} AND m.archived = ?3
  AND (vb_lower(m.name) LIKE ?4 ESCAPE '\' OR vb_lower(COALESCE((
      SELECT text FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
      ORDER BY s.recorded DESC, s.id DESC LIMIT 1), '')) LIKE ?4 ESCAPE '\')
ORDER BY m.created DESC, m.id DESC LIMIT ?5 OFFSET ?6;

-- name: message_rows
SELECT id, recorded, stage, text FROM site_messages
WHERE monolog_id = ?1 AND deleted = 0
ORDER BY recorded, id LIMIT ?2 OFFSET ?3;

-- name: message_texts
SELECT text FROM site_messages WHERE id IN ({{marks}}) AND deleted = 0
ORDER BY recorded, id;

-- name: monolog_row
SELECT m.id, m.name, m.archived, m.created,
    (SELECT COUNT(*) FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0),
    COALESCE((SELECT text FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
        ORDER BY s.recorded DESC, s.id DESC LIMIT 1), ''),
    COALESCE((SELECT recorded FROM site_messages s WHERE s.monolog_id = m.id AND s.deleted = 0
        ORDER BY s.recorded DESC, s.id DESC LIMIT 1), '')
FROM monologs m WHERE m.id = ?1;

-- name: message_ids
SELECT id FROM site_messages WHERE monolog_id = ?1 AND deleted = 0
ORDER BY recorded, id;
