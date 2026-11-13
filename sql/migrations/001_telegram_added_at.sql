-- Repair a database created before `telegram_chat.added_at` existed.
--
-- SQLite has no `ADD COLUMN IF NOT EXISTS`; the migration runner checks for the
-- column first and only runs this file when it is missing. The repair is
-- additive and idempotent: it only adds what is missing and backfills it from
-- the data that is already there.
ALTER TABLE telegram_chat ADD COLUMN added_at TEXT NOT NULL DEFAULT '';

-- Messages that predate the column were handled at about their own time;
-- using `at` keeps the date filter meaningful for them.
UPDATE telegram_chat SET added_at = at WHERE added_at = '';
