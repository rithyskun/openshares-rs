//! Role-based access control — port of `src/lib/rbac.ts` (`can` /
//! `permissionsOf`, `PERMISSIONS`, `isValidPermissionKey`). The admin
//! surface (`apiGuard`, `requirePerm`, role management) lands with the
//! API phase.

use std::collections::HashSet;

use crate::db::Db;
use crate::error::Result;

/// `permissionsOf` — union of `permission_key` across the user's roles.
pub async fn permissions_of(db: &Db, user_id: i64) -> Result<HashSet<String>> {
    // Values are read inside `next()` — local-driver rows are bound to the
    // shared statement cursor, so a collect-then-read (`query_rows`) sees
    // only the final position.
    let mut rows = db
        .query_guard(
            "SELECT DISTINCT rp.permission_key
           FROM user_roles ur
           JOIN role_permissions rp ON rp.role_id = ur.role_id
          WHERE ur.user_id = ?",
            libsql::params![user_id],
        )
        .await?;
    let mut perms = HashSet::new();
    while let Some(row) = rows.next().await? {
        perms.insert(row.get::<String>(0)?);
    }
    Ok(perms)
}

/// `can(userId, key, ownerId?)` — `*` | exact key | `key:any`, or
/// `ownerId === userId && key:own`.
pub async fn can(db: &Db, user_id: i64, key: &str, owner_id: Option<i64>) -> Result<bool> {
    let perms = permissions_of(db, user_id).await?;
    if perms.contains("*") || perms.contains(key) || perms.contains(&format!("{key}:any")) {
        return Ok(true);
    }
    Ok(owner_id == Some(user_id) && perms.contains(&format!("{key}:own")))
}

/* ------------------------- permission catalog ------------------------- */

/// `PERMISSIONS` — the code-level permission catalog from rbac.ts: every
/// key a code path can check. Grammar: `resource:action[:scope]`; scope
/// defaults to `any`, `own` is explicit. Order is byte-identical to the
/// TS array — `/api/admin/permissions` groups by resource in this order.
/// `*` (the admin wildcard) is deliberately not a catalog member: it is
/// *valid* but reserved for the built-in `admin` role.
pub const PERMISSIONS: &[&str] = &[
    "media:view",
    "media:upload",
    "media:edit:own",
    "media:edit:any",
    "media:delete:own",
    "media:delete:any",
    "album:create",
    "album:edit:own",
    "album:edit:any",
    "album:delete:own",
    "album:delete:any",
    "share:link",
    "share:album",
    "share:friend",
    "friend:invite",
    "friend:remove",
    "admin:access",
    "user:manage",
    "role:manage",
    "settings:manage",
];

/// `isValidPermissionKey` — `VALID_KEYS.has(key)` = catalog ∪ `{"*"}`.
pub fn is_valid_permission_key(key: &str) -> bool {
    key == "*" || PERMISSIONS.contains(&key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_membership() {
        assert!(is_valid_permission_key("media:view"));
        assert!(is_valid_permission_key("settings:manage"));
        assert!(is_valid_permission_key("*"));
        assert!(!is_valid_permission_key("media:hack"));
        assert!(!is_valid_permission_key(""));
        assert_eq!(PERMISSIONS.len(), 20);
    }
}
