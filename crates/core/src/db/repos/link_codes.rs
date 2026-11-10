//! Link-codes repository — ports the `link_codes` writes in
//! `src/app/api/friends/route.ts` (`POST {action:"invite"}`) and
//! `src/app/api/profile/link/route.ts` (minting `/link CODE`).
//!
//! Reading/consuming codes stays inline in `crate::bot` (`/start CODE`
//! handles the 'friend'/'link'/'login' purposes); these helpers only
//! cover what the `/api/*` routes mint.

use crate::db::{Db, now};
use crate::error::Result;

/// `INSERT INTO link_codes (code, purpose, user_id, expiry_at, used,
/// created_at)` — `purpose` is `'friend'` for invites, `'link'` for
/// account linking. `expiry_at` mirrors the TS column name (it's an
/// "expires at" unix timestamp; `None` = never).
///
/// Note the friend-invite TS route does the insert with `expiry_at =
/// NULL` then a follow-up `UPDATE` — a leftover from a D1 prepared-
/// statement quirk. Setting it directly here is equivalent; pass
/// `Some(now() + 7 * 86400)` for the 7-day invite lifetime.
pub async fn create(
    db: &Db,
    code: &str,
    purpose: &str,
    user_id: i64,
    expiry_at: Option<i64>,
) -> Result<()> {
    db.execute(
        "INSERT INTO link_codes (code, purpose, user_id, expiry_at, used, created_at)
         VALUES (?, ?, ?, ?, 0, ?)",
        libsql::params![code, purpose, user_id, expiry_at, now()],
    )
    .await?;
    Ok(())
}

/// `UPDATE link_codes SET expiry_at = ? WHERE code = ?` — kept for
/// parity with the TS invite route's two-step write.
pub async fn set_expiry(db: &Db, code: &str, expiry_at: Option<i64>) -> Result<()> {
    db.execute(
        "UPDATE link_codes SET expiry_at = ? WHERE code = ?",
        libsql::params![expiry_at, code],
    )
    .await?;
    Ok(())
}
