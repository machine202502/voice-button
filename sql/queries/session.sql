-- name: write
INSERT INTO recordings(name, id, started, interactor, model, seconds, done,
    failed_segments, failed, kept)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(name) DO UPDATE SET id = excluded.id, started = excluded.started,
    interactor = excluded.interactor, model = excluded.model, seconds = excluded.seconds,
    done = excluded.done, failed_segments = excluded.failed_segments,
    failed = excluded.failed, kept = excluded.kept;

-- name: read
SELECT id, started, interactor, model, seconds, done, failed_segments, failed, kept
FROM recordings WHERE name = ?1;

-- name: delete
DELETE FROM recordings WHERE name = ?1;
