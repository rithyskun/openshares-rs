-- A stored file_id can die when the bot token rotates or the original Telegram
-- message is deleted. `missing` flags those rows at send/fetch failure time so
-- reads can report "gone" instead of retrying doomed fetches. Kept separate
-- from `availability` because that column has a CHECK constraint.
ALTER TABLE media ADD COLUMN missing INTEGER NOT NULL DEFAULT 0;
