-- 0009_media_album_idx.sql — index media.album_id.
--
-- The albums listing runs two correlated subqueries per album row
-- (COUNT + first-slug) filtered on media.album_id, and album DELETEs
-- scan the table for the ON DELETE SET NULL cascade — all full media
-- scans without this index. Measured: ~11-34k rows read per albums
-- query, ~4.7M/day total → the D1 free-tier read-limit burn.
CREATE INDEX IF NOT EXISTS idx_media_album ON media(album_id);
