//! Sessions repository — ports the `sessions`-table queries in
//! `src/lib/auth.ts` (`createSession`, `destroySession`,
//! `getSessionUserByToken`, `cleanupExpiredSessions`) and the
//! sign-out-others DELETE in `api/profile/password/route.ts`.
//!
//! The JWT half of the session (sign/verify the `os_session` cookie)
//! lives in `crate::crypto` — these are only the revocation-anchor rows.

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;

/// Session TTL used by `createSession` — 1 day, seconds.
pub const SESSION_TTL: i64 = 24 * 3600;

/// The `sessions ⋈ users` row behind `getSessionUserByToken` — the
/// session's own id column is implied by the lookup key, so the row
/// carries the user fields plus `session_created_at` for the
/// "Signed in <datetime>" stamp (`SessionUser` in auth.ts).
#[derive(Debug, Clone, Deserialize)]
pub struct SessionUserRow {
    pub id: i64,
    pub email: Option<String>,
    pub name: String,
    pub telegram_id: Option<String>,
    pub session_created_at: Option<i64>,
}

/// `createSession` row insert — `expires_at = now + ttl`, `created_at =
/// now`. Pass `SESSION_TTL` to mirror the TS default; the caller mints
/// the `sid` (`newToken()`) and the cookie JWT.
pub async fn create(db: &Db, sid: &str, user_id: i64, ttl_secs: i64) -> Result<()> {
    let ts = now();
    db.execute(
        "INSERT INTO sessions (id, user_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
        libsql::params![sid, user_id, ts + ttl_secs, ts],
    )
    .await?;
    Ok(())
}

/// The DB leg of `getSessionUserByToken`:
/// `SELECT u.id, u.email, u.name, u.telegram_id, s.created_at AS
/// session_created_at FROM sessions s JOIN users u … WHERE s.id = ? AND
/// s.user_id = ? AND s.expires_at > ? AND u.disabled_at IS NULL`.
///
/// The `sid`/`user_id` pair comes from verified JWT claims — an expired
/// session or suspended user resolves to `None`, same as the TS code.
pub async fn get_user(db: &Db, sid: &str, user_id: i64) -> Result<Option<SessionUserRow>> {
    db.query_opt::<SessionUserRow>(
        "SELECT u.id, u.email, u.name, u.telegram_id, s.created_at AS session_created_at
           FROM sessions s JOIN users u ON u.id = s.user_id
          WHERE s.id = ? AND s.user_id = ? AND s.expires_at > ? AND u.disabled_at IS NULL",
        libsql::params![sid, user_id, now()],
    )
    .await
}

/// `destroySession` — `DELETE FROM sessions WHERE id = ?`.
pub async fn delete(db: &Db, sid: &str) -> Result<()> {
    db.execute("DELETE FROM sessions WHERE id = ?", libsql::params![sid])
        .await?;
    Ok(())
}

/// `DELETE FROM sessions WHERE user_id = ?` — sign-out-everywhere after
/// an admin password reset or a suspension.
pub async fn delete_all_for_user(db: &Db, user_id: i64) -> Result<()> {
    db.execute(
        "DELETE FROM sessions WHERE user_id = ?",
        libsql::params![user_id],
    )
    .await?;
    Ok(())
}

/// `DELETE FROM sessions WHERE user_id = ? AND id != ?` — the profile
/// password change keeps the session that performed it and kills the
/// rest.
pub async fn delete_all_except(db: &Db, user_id: i64, keep_sid: &str) -> Result<()> {
    db.execute(
        "DELETE FROM sessions WHERE user_id = ? AND id != ?",
        libsql::params![user_id, keep_sid],
    )
    .await?;
    Ok(())
}

/// `cleanupExpiredSessions` — `DELETE FROM sessions WHERE expires_at <=
/// ?` (best-effort sweep piggybacking on login).
pub async fn cleanup_expired(db: &Db) -> Result<()> {
    db.execute(
        "DELETE FROM sessions WHERE expires_at <= ?",
        libsql::params![now()],
    )
    .await?;
    Ok(())
}
