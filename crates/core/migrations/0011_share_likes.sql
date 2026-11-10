-- 0011_share_likes.sql
--
-- likes alongside views: /s/<token> and /a/<token> render a heart button and
-- each visitor's first like (deduped by an HttpOnly cookie) bumps the counter,
-- so owners can see engagement, not just opens.
ALTER TABLE shares ADD COLUMN likes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE albums ADD COLUMN likes INTEGER NOT NULL DEFAULT 0;
