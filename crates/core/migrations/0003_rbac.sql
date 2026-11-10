-- 0003_rbac.sql — roles, role_permissions, user_roles, users.disabled_at.
-- Permission keys live in the code catalog (src/lib/rbac.ts PERMISSIONS);
-- role_permissions stores key strings validated at write time.

CREATE TABLE roles (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  name       TEXT UNIQUE NOT NULL,
  builtin    INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE role_permissions (
  role_id        INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  permission_key TEXT NOT NULL,
  PRIMARY KEY (role_id, permission_key)
);

CREATE TABLE user_roles (
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role_id INTEGER NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
  PRIMARY KEY (user_id, role_id)
);

ALTER TABLE users ADD COLUMN disabled_at INTEGER;

-- Seed the four built-in roles.
INSERT INTO roles (name, builtin, created_at) VALUES
  ('admin',       1, unixepoch()),
  ('editor',      1, unixepoch()),
  ('contributor', 1, unixepoch()),
  ('viewer',      1, unixepoch());

-- Permission bundles (editor: any-scope edits but own-scope deletes only —
-- vault-wide delete stays admin/* only). Scalar subqueries resolve role ids
-- because D1 rejects large UNION ALL seed statements.
INSERT INTO role_permissions (role_id, permission_key) VALUES
  ((SELECT id FROM roles WHERE name = 'admin'),       '*'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'media:view'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'media:upload'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'media:edit:any'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'media:delete:own'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'album:create'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'album:edit:any'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'album:delete:own'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'share:link'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'share:album'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'share:friend'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'friend:invite'),
  ((SELECT id FROM roles WHERE name = 'editor'),      'friend:remove'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'media:view'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'media:upload'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'media:edit:own'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'media:delete:own'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'album:create'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'album:edit:own'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'album:delete:own'),
  ((SELECT id FROM roles WHERE name = 'contributor'), 'share:link'),
  ((SELECT id FROM roles WHERE name = 'viewer'),      'media:view');

-- Bootstrap: every existing user was a vault owner pre-RBAC → admin.
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r WHERE r.name = 'admin';
