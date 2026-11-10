//! Roles repository — ports the `roles` / `role_permissions` queries in
//! `src/app/api/admin/roles/**/route.ts` (list/create/rename/delete,
//! permission-set replacement) and the last-admin guard queries in
//! `src/lib/rbac.ts` (`roleHasWildcard`, `adminUserIds`,
//! `adminUserIdsExceptRole`).
//!
//! User-side role reads/writes (`rolesOf`, `user_roles` rows) are in
//! [`super::users`]; request-time permission checks (`can`,
//! `permissions_of`) stay in `crate::rbac`.

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;
use crate::models::{Role, RolePermission};

/// `GET /api/admin/roles` — `ORDER BY builtin DESC, name` (built-ins
/// first, then customs alphabetically).
pub async fn list(db: &Db) -> Result<Vec<Role>> {
    db.query_all("SELECT * FROM roles ORDER BY builtin DESC, name", ())
        .await
}

/// `loadRole(id)` — the role PATCH/DELETE pre-check.
pub async fn get_by_id(db: &Db, id: i64) -> Result<Option<Role>> {
    db.query_opt("SELECT * FROM roles WHERE id = ?", libsql::params![id])
        .await
}

/// `SELECT * FROM roles WHERE name = ?` — role-name resolution for
/// `api/admin/users/[id]/roles` and the create-clash check.
pub async fn get_by_name(db: &Db, name: &str) -> Result<Option<Role>> {
    db.query_opt("SELECT * FROM roles WHERE name = ?", libsql::params![name])
        .await
}

/// `SELECT id FROM roles WHERE name = ? AND id != ?` — the rename clash
/// check (pass `exclude_id = None` for create).
pub async fn name_taken(db: &Db, name: &str, exclude_id: Option<i64>) -> Result<bool> {
    let id: Option<i64> = db
        .scalar_opt(
            "SELECT id FROM roles WHERE name = ? AND (? IS NULL OR id != ?)",
            libsql::params![name, exclude_id, exclude_id],
        )
        .await?;
    Ok(id.is_some())
}

/// `INSERT INTO roles (name, builtin, created_at) VALUES (?, 0, ?)` —
/// custom roles are `builtin = 0`; returns the new id (`RETURNING id`
/// replaces `lastInsertRowid`, race-free on the shared connection).
pub async fn create(db: &Db, name: &str) -> Result<i64> {
    db.scalar(
        "INSERT INTO roles (name, builtin, created_at) VALUES (?, 0, ?) RETURNING id",
        libsql::params![name, now()],
    )
    .await
}

/// `UPDATE roles SET name = ? WHERE id = ?` — callers refuse built-ins
/// (the TS PATCH checks `role.builtin` before this).
pub async fn rename(db: &Db, id: i64, name: &str) -> Result<()> {
    db.execute(
        "UPDATE roles SET name = ? WHERE id = ?",
        libsql::params![name, id],
    )
    .await?;
    Ok(())
}

/// `DELETE FROM roles WHERE id = ?` — custom roles only (caller checks
/// `builtin` + the last-admin invariant). `user_roles` and
/// `role_permissions` rows cascade.
pub async fn delete_by_id(db: &Db, id: i64) -> Result<()> {
    db.execute("DELETE FROM roles WHERE id = ?", libsql::params![id])
        .await?;
    Ok(())
}

/// `SELECT role_id, permission_key FROM role_permissions ORDER BY
/// permission_key` — the admin roles list maps these into per-role key
/// sets.
pub async fn all_permissions(db: &Db) -> Result<Vec<RolePermission>> {
    db.query_all(
        "SELECT role_id, permission_key FROM role_permissions ORDER BY permission_key",
        (),
    )
    .await
}

/// The permission-set half of `PATCH /api/admin/roles/[id]` —
/// `DELETE … WHERE role_id = ?` then one `INSERT` per key, in a single
/// transaction so a mid-write crash can't leave a half-replaced set.
/// Key validation (catalog membership, `*` rules) is the caller's job.
pub async fn replace_permissions(db: &Db, role_id: i64, keys: &[String]) -> Result<()> {
    let tx = db.transaction().await?;
    tx.execute(
        "DELETE FROM role_permissions WHERE role_id = ?",
        libsql::params![role_id],
    )
    .await?;
    for key in keys {
        tx.execute(
            "INSERT INTO role_permissions (role_id, permission_key) VALUES (?, ?)",
            libsql::params![role_id, key.as_str()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// `(role_id, member_count)` — `SELECT role_id, COUNT(*) AS n FROM
/// user_roles GROUP BY role_id`, the memberCount figures on the admin
/// roles list.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct RoleMemberCount {
    pub role_id: i64,
    pub n: i64,
}

/// Member counts per role — admin roles list.
pub async fn member_counts(db: &Db) -> Result<Vec<RoleMemberCount>> {
    db.query_all(
        "SELECT role_id, COUNT(*) AS n FROM user_roles GROUP BY role_id",
        (),
    )
    .await
}

/// `roleHasWildcard(roleId)` — does this role currently grant `*`?
pub async fn has_wildcard(db: &Db, role_id: i64) -> Result<bool> {
    let n: i64 = db
        .scalar(
            "SELECT COUNT(*) FROM role_permissions WHERE role_id = ? AND permission_key = '*'",
            libsql::params![role_id],
        )
        .await?;
    Ok(n > 0)
}

/// Ids of users who would hold `*` — `adminUserIds` /
/// `adminUserIdsExceptRole` merged: `except_role` skips one role's
/// grants (the role about to lose `*` or be deleted). Only active users
/// count (`u.disabled_at IS NULL`), matching the TS queries — the
/// last-admin invariant compares the returned set's size.
pub async fn admin_user_ids(db: &Db, except_role: Option<i64>) -> Result<Vec<i64>> {
    let rows = db
        .query_rows(
            "SELECT DISTINCT ur.user_id
               FROM user_roles ur
               JOIN role_permissions rp ON rp.role_id = ur.role_id AND rp.permission_key = '*'
               JOIN users u ON u.id = ur.user_id
              WHERE u.disabled_at IS NULL AND (? IS NULL OR ur.role_id != ?)",
            libsql::params![except_role, except_role],
        )
        .await?;
    rows.iter().map(|r| r.get::<i64>(0)).collect()
}
