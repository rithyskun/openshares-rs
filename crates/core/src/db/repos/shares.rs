//! Shares repository — ports the `shares`-table queries in
//! `src/app/api/media/[id]/share/route.ts` (list/create/revoke),
//! `api/media/by-token/[token]/route.ts` (token → slug), the
//! `/s/[token]/page.tsx` public share page (media join + view counter),
//! and `api/public/share/[token]/like/route.ts`.
//!
//! Two share kinds share the table: `'link'` rows mint `/s/<token>` URLs
//! (`expires_at = created_at + PUBLIC_LINK_TTL`); `'telegram'` rows are
//! friend-delivery receipts with `to_telegram_id` and no token/expiry.

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;

/// `INSERT … kind = 'link'` — `POST /api/media/[id]/share {mode:"link"}`.
/// `expires_at` is `now() + PUBLIC_LINK_TTL` (computed by the caller so
/// tests can inject); `media.shares_count` is bumped separately
/// ([`crate::db::repos::media::increment_shares_count`], same as the TS
/// route doing two statements).
pub async fn create_link(
    db: &Db,
    media_id: i64,
    token: &str,
    created_by: i64,
    expires_at: i64,
) -> Result<()> {
    db.execute(
        "INSERT INTO shares (media_id, kind, token, created_by, expires_at, created_at)
         VALUES (?, 'link', ?, ?, ?, ?)",
        libsql::params![media_id, token, created_by, expires_at, now()],
    )
    .await?;
    Ok(())
}

/// `INSERT … kind = 'telegram'` — `mode:"telegram"` after a successful
/// bot delivery to a friend's chat.
pub async fn create_telegram(
    db: &Db,
    media_id: i64,
    to_telegram_id: &str,
    created_by: i64,
) -> Result<()> {
    db.execute(
        "INSERT INTO shares (media_id, kind, to_telegram_id, created_by, created_at)
         VALUES (?, 'telegram', ?, ?, ?)",
        libsql::params![media_id, to_telegram_id, created_by, now()],
    )
    .await?;
    Ok(())
}

/// One live link row for the share-management list.
#[derive(Debug, Clone, Deserialize)]
pub struct ShareLinkRow {
    pub token: Option<String>,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub views: i64,
    pub likes: i64,
}

/// `GET /api/media/[id]/share` — live link shares for one media, newest
/// first. `created_by = None` maps to the admin `media:delete:any`
/// "see all" case (the TS `(? IS NULL OR created_by = ?)` predicate);
/// `Some(uid)` restricts to links that user created.
pub async fn list_links(
    db: &Db,
    media_id: i64,
    created_by: Option<i64>,
) -> Result<Vec<ShareLinkRow>> {
    db.query_all(
        "SELECT token, created_at, expires_at, views, likes FROM shares
          WHERE media_id = ? AND kind = 'link'
            AND (? IS NULL OR created_by = ?)
            AND (expires_at IS NULL OR expires_at > ?)
          ORDER BY created_at DESC",
        libsql::params![media_id, created_by, created_by, now()],
    )
    .await
}

/// `created_by` for one link — the DELETE route's own-vs-admin check.
/// Outer `Option` = row exists; inner = the nullable column.
pub async fn get_link_creator(db: &Db, media_id: i64, token: &str) -> Result<Option<Option<i64>>> {
    let rows = db
        .query_rows(
            "SELECT created_by FROM shares WHERE media_id = ? AND token = ? AND kind = 'link'",
            libsql::params![media_id, token],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some(row.get::<Option<i64>>(0)?)),
        None => Ok(None),
    }
}

/// `DELETE FROM shares WHERE media_id = ? AND token = ? AND kind =
/// 'link'` — revoke; returns whether a row was removed (`info.changes`).
pub async fn delete_link(db: &Db, media_id: i64, token: &str) -> Result<bool> {
    let affected = db
        .execute(
            "DELETE FROM shares WHERE media_id = ? AND token = ? AND kind = 'link'",
            libsql::params![media_id, token],
        )
        .await?;
    Ok(affected > 0)
}

/// The share half of `/api/media/[id]?token=` authorization — does a
/// live `'link'` share row exist for this (media, token) pair?
/// `COUNT(*) > 0` version of the TS `authorized()` check.
pub async fn live_link_exists(db: &Db, media_id: i64, token: &str) -> Result<bool> {
    let n: i64 = db
        .scalar(
            "SELECT COUNT(*) FROM shares
              WHERE media_id = ? AND token = ? AND kind = 'link'
                AND (expires_at IS NULL OR expires_at > ?)",
            libsql::params![media_id, token, now()],
        )
        .await?;
    Ok(n > 0)
}

/// `api/media/by-token/[token]` — resolve a live link token to its
/// media slug (`shares ⋈ media` join, expiry filtered).
pub async fn media_slug_by_link_token(db: &Db, token: &str) -> Result<Option<String>> {
    db.scalar_opt(
        "SELECT m.slug FROM shares sh JOIN media m ON m.id = sh.media_id
          WHERE sh.token = ? AND sh.kind = 'link'
            AND (sh.expires_at IS NULL OR sh.expires_at > ?)",
        libsql::params![token, now()],
    )
    .await
}

/// `/s/<token>` page row — `SHARE_SELECT` verbatim: the media display
/// fields (with `missing → 'dead'` folded into `availability`), the
/// owner's display name, and the share's timestamps/counters.
#[derive(Debug, Clone, Deserialize)]
pub struct SharedMediaRow {
    pub slug: String,
    pub kind: String,
    pub caption: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    pub file_size: Option<i64>,
    /// 'available' | 'telegram-only' | 'dead'.
    pub availability: String,
    pub owner_name: Option<String>,
    /// The SHARE's creation time (share age, not media age).
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub likes: i64,
}

/// The `SHARE_SELECT` query — `WHERE sh.token = ? AND sh.kind = 'link'`
/// **without** the expiry check (the page renders an explicit "Link
/// expired" state for dead rows, so it needs expired rows too; callers
/// apply the `expires_at <= now` check themselves).
pub async fn get_shared_media_by_token(db: &Db, token: &str) -> Result<Option<SharedMediaRow>> {
    db.query_opt(
        "SELECT m.slug, m.kind, m.caption, m.width, m.height, m.duration, m.file_size,
                CASE WHEN m.missing THEN 'dead' ELSE m.availability END AS availability,
                u.name AS owner_name, sh.created_at AS created_at, sh.expires_at AS expires_at, sh.likes AS likes
           FROM shares sh JOIN media m ON m.id = sh.media_id
           LEFT JOIN users u ON u.id = m.owner_user_id
          WHERE sh.token = ? AND sh.kind = 'link'",
        libsql::params![token],
    )
    .await
}

/// The like route's initial read — `(likes, expires_at)` for a link
/// token, `None` when the token doesn't name a `'link'` share.
pub async fn get_link_like_state(db: &Db, token: &str) -> Result<Option<(i64, Option<i64>)>> {
    let rows = db
        .query_rows(
            "SELECT likes, expires_at FROM shares WHERE token = ? AND kind = 'link'",
            libsql::params![token],
        )
        .await?;
    match rows.first() {
        Some(row) => Ok(Some((row.get::<i64>(0)?, row.get::<Option<i64>>(1)?))),
        None => Ok(None),
    }
}

/// `/s/<token>` page render — `UPDATE shares SET views = views + 1
/// WHERE token = ?` (called only for live links, like the TS page).
pub async fn increment_views(db: &Db, token: &str) -> Result<()> {
    db.execute(
        "UPDATE shares SET views = views + 1 WHERE token = ?",
        libsql::params![token],
    )
    .await?;
    Ok(())
}

/// `POST /api/public/share/<token>/like` — `UPDATE shares SET likes =
/// likes + 1 WHERE token = ? RETURNING likes`.
pub async fn increment_likes(db: &Db, token: &str) -> Result<Option<i64>> {
    db.scalar_opt(
        "UPDATE shares SET likes = likes + 1 WHERE token = ? RETURNING likes",
        libsql::params![token],
    )
    .await
}

/// `SELECT COUNT(*) AS n, COALESCE(SUM(CASE WHEN kind = 'link' AND
/// (expires_at IS NULL OR expires_at > ?) THEN 1 ELSE 0 END), 0) AS
/// active_links FROM shares` — the admin stats panel's share figure.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ShareTotals {
    pub n: i64,
    pub active_links: i64,
}

/// Share totals — admin vault stats.
pub async fn totals(db: &Db) -> Result<ShareTotals> {
    db.query_one(
        "SELECT COUNT(*) AS n,
                COALESCE(SUM(CASE WHEN kind = 'link' AND (expires_at IS NULL OR expires_at > ?) THEN 1 ELSE 0 END), 0) AS active_links
           FROM shares",
        libsql::params![now()],
    )
    .await
}
