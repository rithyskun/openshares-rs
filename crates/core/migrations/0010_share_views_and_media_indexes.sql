-- 0010_share_views_and_media_indexes.sql
--
-- shares.views: /s/<token> page renders increment it (same semantics as
-- albums.views) so the owner can see which links actually get opened.
ALTER TABLE shares ADD COLUMN views INTEGER NOT NULL DEFAULT 0;

-- Paginated media listing: the vault scope is `album_id IS NULL ORDER BY
-- created_at DESC, id DESC`; the album scope is `album_id = ? ORDER BY id ASC`.
-- idx_media_album (0009) is single-column and can't serve the ordering —
-- these two cover both access paths.
CREATE INDEX IF NOT EXISTS idx_media_unfiled
  ON media(created_at DESC, id DESC) WHERE album_id IS NULL;
CREATE INDEX IF NOT EXISTS idx_media_album_id ON media(album_id, id);
