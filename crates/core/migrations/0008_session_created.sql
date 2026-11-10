-- When this session was created — shown in the navbar profile menu as
-- "Signed in <datetime>". NULL on pre-existing sessions; new logins set it.
ALTER TABLE sessions ADD COLUMN created_at INTEGER;
