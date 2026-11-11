//! Dev-only seed — `DEV_SEED=1` (set by `dev.sh` in `DB=local` mode)
//! creates the `dev@localhost`/`devpassword123` admin that the TS
//! `npm run db:setup` script seeded, but only on a **local** SQLite db
//! whose `users` table is still empty. Both guards matter: a known-
//! password admin must never materialize on a remote/production db.

use openshares_core::crypto::password::hash_password_async;
use openshares_core::db::Db;
use openshares_core::db::repos::users::NewUser;
use openshares_core::db::repos::{roles, users};
use openshares_core::error::Result;

/// Same credentials `scripts/setup-db.ts` defaulted to.
pub const DEV_EMAIL: &str = "dev@localhost";
pub const DEV_PASSWORD: &str = "devpassword123";

/// Seed the dev admin. Returns `true` when the user was created —
/// `false` for a remote db or a `users` table that already has rows.
pub async fn ensure_dev_admin(db: &Db) -> Result<bool> {
    if db.is_remote() {
        tracing::warn!("DEV_SEED ignored — refusing to seed a remote database");
        return Ok(false);
    }
    if !users::list_all(db).await?.is_empty() {
        return Ok(false);
    }

    let hash = hash_password_async(DEV_PASSWORD.to_string()).await?;
    let user_id = users::create(
        db,
        &NewUser {
            email: Some(DEV_EMAIL),
            name: "Dev",
            password_hash: Some(&hash),
        },
    )
    .await?;
    if let Some(admin) = roles::get_by_name(db, "admin").await? {
        users::assign_role(db, user_id, admin.id).await?;
    }
    Ok(true)
}
