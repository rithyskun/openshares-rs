//! Role-based access control — port of `src/lib/rbac.ts` (`can` /
//! `permissionsOf`). The admin surface (`apiGuard`, `requirePerm`,
//! role management) lands with the API phase.

use std::collections::HashSet;

use crate::db::Db;
use crate::error::Result;

/// `permissionsOf` — union of `permission_key` across the user's roles.
pub async fn permissions_of(db: &Db, user_id: i64) -> Result<HashSet<String>> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT rp.permission_key
           FROM user_roles ur
           JOIN role_permissions rp ON rp.role_id = ur.role_id
          WHERE ur.user_id = ?",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().collect())
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
