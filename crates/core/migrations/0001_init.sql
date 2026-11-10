CREATE TABLE IF NOT EXISTS users (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  email         TEXT UNIQUE,
  password_hash TEXT,
  name          TEXT NOT NULL DEFAULT '',
  telegram_id   TEXT UNIQUE,
  bot_profile   TEXT,
  notifications INTEGER NOT NULL DEFAULT 1,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
  id         TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_user ON sessions(user_id);

CREATE TABLE IF NOT EXISTS telegram_users (
  telegram_id  TEXT PRIMARY KEY,
  username     TEXT,
  first_name   TEXT,
  last_name    TEXT,
  updated_at   INTEGER NOT NULL
);

-- purpose: 'login'  -> passwordless sign-in: user sends /start <code>, bot binds the
--                       visitor's telegram login -> browser polls and gets a session
--          'link'   -> attach a telegram account to an existing web account
--          'friend' -> one-time share invite accepted from another telegram user
CREATE TABLE IF NOT EXISTS link_codes (
  code       TEXT PRIMARY KEY,
  purpose    TEXT NOT NULL,
  user_id    INTEGER REFERENCES users(id) ON DELETE CASCADE,
  expiry_at  INTEGER,
  used       INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_link_codes ON link_codes(user_id, purpose);

CREATE TABLE IF NOT EXISTS friend_links (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  friend_telegram_id TEXT NOT NULL REFERENCES telegram_users(telegram_id) ON DELETE CASCADE,
  created_at   INTEGER NOT NULL,
  UNIQUE(owner_user_id, friend_telegram_id)
);
CREATE INDEX IF NOT EXISTS idx_friend_links_owner ON friend_links(owner_user_id);

CREATE TABLE IF NOT EXISTS media (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  slug          TEXT UNIQUE NOT NULL,
  owner_user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
  telegram_id   TEXT NOT NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('photo','video')),
  file_id       TEXT NOT NULL,
  file_unique_id TEXT,
  thumb_file_id TEXT,
  file_size     INTEGER,
  width         INTEGER,
  height        INTEGER,
  duration      INTEGER,
  mime          TEXT,
  caption       TEXT,
  shares_count  INTEGER NOT NULL DEFAULT 0,
  availability  TEXT NOT NULL DEFAULT 'available' CHECK (availability IN ('available','telegram-only')),
  created_at    INTEGER NOT NULL,
  album_id      INTEGER REFERENCES albums(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_media_owner ON media(owner_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_media_tg    ON media(telegram_id);

CREATE TABLE IF NOT EXISTS shares (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  media_id      INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
  kind          TEXT NOT NULL CHECK (kind IN ('telegram','link')),
  token         TEXT UNIQUE,
  to_telegram_id TEXT,
  created_by    INTEGER REFERENCES users(id) ON DELETE CASCADE,
  created_by_tg TEXT,
  expires_at    INTEGER,
  created_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_shares_media ON shares(media_id);

CREATE TABLE IF NOT EXISTS albums (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  slug          TEXT UNIQUE NOT NULL,
  owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  title         TEXT NOT NULL DEFAULT '',
  token         TEXT UNIQUE NOT NULL,
  is_public     INTEGER NOT NULL DEFAULT 0,
  views         INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  public_expires_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_albums_owner ON albums(owner_user_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_albums_token ON albums(token);
