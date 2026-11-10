CREATE TABLE IF NOT EXISTS rate_limits (
  bucket       TEXT PRIMARY KEY,
  window_start INTEGER NOT NULL,
  count        INTEGER NOT NULL
);