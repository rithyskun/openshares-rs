//! Users repository — ports the `users`-table queries in `src/lib/auth.ts`
//! (`findUserByEmail`, `findUserByTelegramId`), the admin surface
//! (`src/app/api/admin/users/**/route.ts`), and the profile writes in
//! `src/app/api/profile/password/route.ts`.
//!
//! Role-assignment helpers live here too (the `user_roles` join rows are
//! user properties); role/permission administration is in
//! [`super::roles`].

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;
use crate::models::User;

/// Fields for [`create`] — the admin "create account" INSERT. Password
/// hashing happens in `crypto` (`hashPassword`); callers pass the
/// `salt:hash` string or `None` for Telegram-only accounts.
#[derive(Debug)]
pub struct NewUser<'a> {
    pub email: Option<&'a str>,
    pub name: &'a str,
    pub password_hash: Option<&'a str>,
}

/// `WHERE id = ?` — session joins, admin target checks.
pub async fn get_by_id(db: &Db, id: i64) -> Result<Option<User>> {
    db.query_opt("SELECT * FROM users WHERE id = ?", libsql::params![id])
        .await
}

/// `findUserByEmail` — `email = ? COLLATE NOCASE`, trimmed like the TS
/// call site (login lowercases before calling; admin dup-checks don't).
pub async fn get_by_email(db: &Db, email: &str) -> Result<Option<User>> {
    db.query_opt(
        "SELECT * FROM users WHERE email = ? COLLATE NOCASE",
        libsql::params![email.trim()],
    )
    .await
}

/// `findUserByTelegramId` — the bot-login lookup
/// (`String(telegramId)` — ids are stored as TEXT).
pub async fn get_by_telegram_id(db: &Db, telegram_id: &str) -> Result<Option<User>> {
    db.query_opt(
        "SELECT * FROM users WHERE telegram_id = ?",
        libsql::params![telegram_id],
    )
    .await
}

/// Email uniqueness check for the admin create/edit routes —
/// `SELECT id FROM users WHERE email = ? COLLATE NOCASE [AND id != ?]`.
/// `exclude_id` is the user being edited (pass `None` on create).
pub async fn email_taken(db: &Db, email: &str, exclude_id: Option<i64>) -> Result<bool> {
    let id: Option<i64> = db
        .scalar_opt(
            "SELECT id FROM users WHERE email = ? COLLATE NOCASE AND (? IS NULL OR id != ?)",
            libsql::params![email, exclude_id, exclude_id],
        )
        .await?;
    Ok(id.is_some())
}

/// `GET /api/admin/users` — all web users, `ORDER BY created_at`.
pub async fn list_all(db: &Db) -> Result<Vec<User>> {
    db.query_all("SELECT * FROM users ORDER BY created_at", ())
        .await
}

/// `POST /api/admin/users` — `INSERT INTO users (email, name,
/// password_hash, created_at, updated_at)`; returns the new id.
/// `RETURNING id` replaces `lastInsertRowid` (race-free on the shared
/// connection).
pub async fn create(db: &Db, new: &NewUser<'_>) -> Result<i64> {
    let ts = now();
    db.scalar(
        "INSERT INTO users (email, name, password_hash, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?) RETURNING id",
        libsql::params![new.email, new.name, new.password_hash, ts, ts],
    )
    .await
}

/// Admin profile edit — `UPDATE users SET name = ? WHERE id = ?`.
pub async fn update_name(db: &Db, id: i64, name: &str) -> Result<()> {
    db.execute(
        "UPDATE users SET name = ? WHERE id = ?",
        libsql::params![name, id],
    )
    .await?;
    Ok(())
}

/// Admin profile edit — `UPDATE users SET email = ? WHERE id = ?`
/// (`None` clears the email; callers check [`email_taken`] first).
pub async fn update_email(db: &Db, id: i64, email: Option<&str>) -> Result<()> {
    db.execute(
        "UPDATE users SET email = ? WHERE id = ?",
        libsql::params![email, id],
    )
    .await?;
    Ok(())
}

/// `UPDATE users SET password_hash = ? WHERE id = ?` — both the admin
/// reset and the profile password change. Callers also revoke sessions
/// ([`super::sessions::delete_all_for_user`] /
/// `delete_all_except`) exactly like the TS routes.
pub async fn update_password_hash(db: &Db, id: i64, password_hash: &str) -> Result<()> {
    db.execute(
        "UPDATE users SET password_hash = ? WHERE id = ?",
        libsql::params![password_hash, id],
    )
    .await?;
    Ok(())
}

/// Suspend — `UPDATE users SET disabled_at = <now> WHERE id = ?`.
/// Callers delete the user's sessions too (the TS PATCH does both).
pub async fn disable(db: &Db, id: i64) -> Result<()> {
    db.execute(
        "UPDATE users SET disabled_at = ? WHERE id = ?",
        libsql::params![now(), id],
    )
    .await?;
    Ok(())
}

/// Unsuspend — `UPDATE users SET disabled_at = NULL WHERE id = ?`.
pub async fn enable(db: &Db, id: i64) -> Result<()> {
    db.execute(
        "UPDATE users SET disabled_at = NULL WHERE id = ?",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

/* ------------------------- user_roles ------------------------- */

/// `rolesOf(userId)` — role names for one user, `ORDER BY r.name`.
pub async fn role_names(db: &Db, user_id: i64) -> Result<Vec<String>> {
    let rows = db
        .query_rows(
            "SELECT r.name FROM user_roles ur JOIN roles r ON r.id = ur.role_id
              WHERE ur.user_id = ? ORDER BY r.name",
            libsql::params![user_id],
        )
        .await?;
    rows.iter().map(|r| r.get::<String>(0)).collect()
}

/// `(user_id, role_name)` row — the admin users list maps these into a
/// per-user role list (`rolesByUser` in the TS route).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct UserRoleName {
    pub user_id: i64,
    pub name: String,
}

/// `SELECT ur.user_id, r.name ... ORDER BY r.name` — every assignment,
/// for the admin users list.
pub async fn all_user_role_names(db: &Db) -> Result<Vec<UserRoleName>> {
    db.query_all(
        "SELECT ur.user_id, r.name FROM user_roles ur JOIN roles r ON r.id = ur.role_id
         ORDER BY r.name",
        (),
    )
    .await
}

/// `INSERT OR IGNORE INTO user_roles` — assign; unknown user/role ids are
/// rejected by the FK. Returns `false` when the row already existed.
pub async fn assign_role(db: &Db, user_id: i64, role_id: i64) -> Result<bool> {
    let affected = db
        .execute(
            "INSERT OR IGNORE INTO user_roles (user_id, role_id) VALUES (?, ?)",
            libsql::params![user_id, role_id],
        )
        .await?;
    Ok(affected > 0)
}

/// `DELETE FROM user_roles WHERE user_id = ? AND role_id = ?` — remove.
/// Callers enforce the last-admin invariant first (the TS route checks
/// `violatesLastAdmin`; see [`super::roles::admin_user_ids`]).
pub async fn remove_role(db: &Db, user_id: i64, role_id: i64) -> Result<()> {
    db.execute(
        "DELETE FROM user_roles WHERE user_id = ? AND role_id = ?",
        libsql::params![user_id, role_id],
    )
    .await?;
    Ok(())
}

/// `SELECT COUNT(*) AS n, COALESCE(SUM(CASE WHEN disabled_at IS NULL
/// THEN 1 ELSE 0 END), 0) AS active_n FROM users` — the admin stats
/// panel's user figure.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct UserTotals {
    pub n: i64,
    pub active_n: i64,
}

/// User totals — admin vault stats.
pub async fn totals(db: &Db) -> Result<UserTotals> {
    db.query_one(
        "SELECT COUNT(*) AS n,
                COALESCE(SUM(CASE WHEN disabled_at IS NULL THEN 1 ELSE 0 END), 0) AS active_n
           FROM users",
        (),
    )
    .await
}
