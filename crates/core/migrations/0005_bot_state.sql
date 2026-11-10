-- Pending bot conversation state for multi-step flows, keyed by telegram user.
-- action 'album_title'  -> next non-command text becomes the new album's title
-- action 'album_upload' -> incoming media lands in album_id until /done
CREATE TABLE IF NOT EXISTS bot_state (
  telegram_id TEXT PRIMARY KEY,
  action      TEXT NOT NULL,
  album_id    INTEGER,
  updated_at  INTEGER NOT NULL
);
