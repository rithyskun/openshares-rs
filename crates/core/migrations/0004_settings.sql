-- 0004_settings.sql — key-value settings (DB-managed allowlist etc.) + audit log.

CREATE TABLE settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE audit_log (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  actor_user_id INTEGER,
  action        TEXT NOT NULL,
  target_type   TEXT,
  target_id     TEXT,
  detail        TEXT,
  created_at    INTEGER NOT NULL
);

CREATE INDEX idx_audit_log_created ON audit_log(created_at DESC);
