//! Friends repository — ports the `friend_links ⋈ telegram_users`
//! queries in `src/app/api/friends/**/route.ts`, the album page's friend
//! picker, and the `friend_id → telegram_id` lookup behind
//! `api/media/[id]/share {mode:"telegram"}`.

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;

/// One row of the friends list — the link id plus the friend's Telegram
/// profile and how many shares they've received (`shared` subquery).
#[derive(Debug, Clone, Deserialize)]
pub struct FriendRow {
    /// `friend_links.id` — the DELETE route's path parameter.
    pub id: i64,
    pub telegram_id: String,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// Count of telegram-kind shares delivered to this friend.
    pub shared: i64,
}

/// `GET /api/friends` — my friends newest-first, each with a count of
/// shares delivered to them. The display name is assembled by the caller
/// (`first + last || username || telegram_id` in the TS map).
pub async fn list_for_owner(db: &Db, owner_id: i64) -> Result<Vec<FriendRow>> {
    db.query_all::<FriendRow>(
        "SELECT f.id          AS id,
                t.telegram_id AS telegram_id,
                t.username    AS username,
                t.first_name  AS first_name,
                t.last_name   AS last_name,
                (SELECT COUNT(*) FROM shares s WHERE s.to_telegram_id = t.telegram_id) AS shared
           FROM friend_links f JOIN telegram_users t ON t.telegram_id = f.friend_telegram_id
          WHERE f.owner_user_id = ?
          ORDER BY f.created_at DESC",
        libsql::params![owner_id],
    )
    .await
}

/// The telegram-share target lookup — `friend_id` from the request must
/// resolve to a link row the caller owns (`f.id = ? AND
/// f.owner_user_id = ?`), yielding the friend's `telegram_id`.
#[derive(Debug, Clone, Deserialize)]
pub struct FriendLinkTarget {
    pub link_id: i64,
    pub owner_user_id: i64,
    pub telegram_id: String,
}

/// `SELECT f.id AS link_id, f.owner_user_id, t.telegram_id … WHERE
/// f.id = ? AND f.owner_user_id = ?` — `mode:"telegram"` share.
pub async fn get_link_target(
    db: &Db,
    link_id: i64,
    owner_id: i64,
) -> Result<Option<FriendLinkTarget>> {
    db.query_opt::<FriendLinkTarget>(
        "SELECT f.id AS link_id, f.owner_user_id AS owner_user_id, t.telegram_id AS telegram_id
           FROM friend_links f JOIN telegram_users t ON t.telegram_id = f.friend_telegram_id
          WHERE f.id = ? AND f.owner_user_id = ?",
        libsql::params![link_id, owner_id],
    )
    .await
}

/// `INSERT OR IGNORE INTO friend_links` — accepting a friend invite
/// (also used by the bot's `/add`/`/start` flow inline). Returns `false`
/// when the link already existed (`UNIQUE(owner_user_id,
/// friend_telegram_id)`).
pub async fn add(db: &Db, owner_id: i64, friend_telegram_id: &str) -> Result<bool> {
    let affected = db
        .execute(
            "INSERT OR IGNORE INTO friend_links (owner_user_id, friend_telegram_id, created_at)
         VALUES (?, ?, ?)",
            libsql::params![owner_id, friend_telegram_id, now()],
        )
        .await?;
    Ok(affected > 0)
}

/// `DELETE FROM friend_links WHERE id = ? AND owner_user_id = ?` —
/// `DELETE /api/friends/<id>`; returns `info.changes > 0` (404 check).
pub async fn remove(db: &Db, link_id: i64, owner_id: i64) -> Result<bool> {
    let affected = db
        .execute(
            "DELETE FROM friend_links WHERE id = ? AND owner_user_id = ?",
            libsql::params![link_id, owner_id],
        )
        .await?;
    Ok(affected > 0)
}
