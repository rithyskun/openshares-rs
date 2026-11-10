-- Telegram media_group_id on media: a multi-photo send (folder drag/drop)
-- shares one id, so the first item can create an album and the rest find it.
ALTER TABLE media ADD COLUMN media_group_id TEXT;
CREATE INDEX IF NOT EXISTS idx_media_group ON media(media_group_id);
