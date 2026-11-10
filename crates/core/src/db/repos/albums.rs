//! Albums repository — ports the `albums`-table queries in
//! `src/app/api/albums/**/route.ts`, the public token gate shared by
//! `/a/[token]/page.tsx` + `api/public/album*/route.ts`, the gallery
//! album list in `app/gallery/page.tsx`, and the move-target lookup in
//! `api/media/move/route.ts`.
//!
//! Public-link liveness is `is_public = 1 AND (public_expires_at IS NULL
//! OR public_expires_at > now)` — [`public_link_live`] is the single
//! copy of that predicate; expiry past the TTL is revoked lazily with
//! [`revoke_public`], exactly like the TS pages.

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;
use crate::models::Album;

/// Fields for [`create`] — `POST /api/albums` and the bot `/newalbum`
/// share the same INSERT (`is_public = 0, views = 0, created_at = now`;
/// `token` is a fresh `newToken(16)`).
#[derive(Debug)]
pub struct NewAlbum<'a> {
    pub slug: &'a str,
    pub owner_user_id: i64,
    pub title: &'a str,
    pub token: &'a str,
}

/// `INSERT INTO albums …` — returns the new id (`RETURNING id` replaces
/// `lastInsertRowid`, race-free on the shared connection).
pub async fn create(db: &Db, new: &NewAlbum<'_>) -> Result<i64> {
    db.scalar(
        "INSERT INTO albums (slug, owner_user_id, title, token, is_public, views, created_at)
         VALUES (?, ?, ?, ?, 0, 0, ?) RETURNING id",
        libsql::params![new.slug, new.owner_user_id, new.title, new.token, now()],
    )
    .await
}

/// `loadAlbum(slug)` — the PATCH/DELETE routes' lookup.
pub async fn get_by_slug(db: &Db, slug: &str) -> Result<Option<Album>> {
    db.query_opt("SELECT * FROM albums WHERE slug = ?", libsql::params![slug])
        .await
}

/// `WHERE id = ?` — the `api/media/move` target-album lookup and the
/// media listing's `album:<slug>` scope resolution.
pub async fn get_by_id(db: &Db, id: i64) -> Result<Option<Album>> {
    db.query_opt("SELECT * FROM albums WHERE id = ?", libsql::params![id])
        .await
}

/// `WHERE token = ?` — the public `/a/<token>` and `api/public/album`
/// surfaces resolve albums by their share token, never by slug.
pub async fn get_by_token(db: &Db, token: &str) -> Result<Option<Album>> {
    db.query_opt(
        "SELECT * FROM albums WHERE token = ?",
        libsql::params![token],
    )
    .await
}

/// The `is_public`/`public_expires_at` gate — true while a public link
/// is live. One pure function so routes and pages can't drift on the
/// `expires_at <= now` boundary.
pub fn public_link_live(is_public: i64, public_expires_at: Option<i64>, at: i64) -> bool {
    is_public == 1 && public_expires_at.is_none_or(|exp| exp > at)
}

/// `PATCH /api/albums/[slug]` isPublic — `UPDATE albums SET is_public =
/// ?, public_expires_at = ?`. Pass `Some(now + PUBLIC_LINK_TTL)` when
/// enabling, `None` when disabling (callers also gate `share:album`).
pub async fn set_public(
    db: &Db,
    id: i64,
    is_public: bool,
    public_expires_at: Option<i64>,
) -> Result<()> {
    db.execute(
        "UPDATE albums SET is_public = ?, public_expires_at = ? WHERE id = ?",
        libsql::params![is_public as i64, public_expires_at, id],
    )
    .await?;
    Ok(())
}

/// Lazy revoke — `UPDATE albums SET is_public = 0 WHERE id = ?`. Fired
/// by the public pages when they notice an expired-but-still-flagged
/// link so the owner's dashboard reflects that sharing stopped.
pub async fn revoke_public(db: &Db, id: i64) -> Result<()> {
    db.execute(
        "UPDATE albums SET is_public = 0 WHERE id = ?",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

/// `UPDATE albums SET title = ? WHERE id = ?` — the rename half of the
/// album PATCH.
pub async fn rename(db: &Db, id: i64, title: &str) -> Result<()> {
    db.execute(
        "UPDATE albums SET title = ? WHERE id = ?",
        libsql::params![title, id],
    )
    .await?;
    Ok(())
}

/// `DELETE FROM albums WHERE id = ?` — items survive: `media.album_id`
/// is `ON DELETE SET NULL`, filing them back into the vault.
pub async fn delete_by_id(db: &Db, id: i64) -> Result<()> {
    db.execute("DELETE FROM albums WHERE id = ?", libsql::params![id])
        .await?;
    Ok(())
}

/// `/a/<token>` page view — `UPDATE albums SET views = views + 1`.
pub async fn increment_views(db: &Db, id: i64) -> Result<()> {
    db.execute(
        "UPDATE albums SET views = views + 1 WHERE id = ?",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

/// `POST /api/public/album/like` — `UPDATE albums SET likes = likes + 1
/// WHERE id = ? RETURNING likes`; `None` when the row vanished.
pub async fn increment_likes(db: &Db, id: i64) -> Result<Option<i64>> {
    db.scalar_opt(
        "UPDATE albums SET likes = likes + 1 WHERE id = ? RETURNING likes",
        libsql::params![id],
    )
    .await
}

/// The `/a/[token]` header row — `ALBUM_SELECT` with the owner name
/// join.
#[derive(Debug, Clone, Deserialize)]
pub struct PublicAlbumHead {
    pub id: i64,
    pub title: String,
    pub is_public: i64,
    pub public_expires_at: Option<i64>,
    pub owner_name: Option<String>,
    pub likes: i64,
}

/// `SELECT a.id, a.title, a.is_public, a.public_expires_at, u.name AS
/// owner_name, a.likes FROM albums a LEFT JOIN users u … WHERE a.token = ?`
/// — public album page + OG metadata + the like route.
pub async fn get_public_by_token(db: &Db, token: &str) -> Result<Option<PublicAlbumHead>> {
    db.query_opt(
        "SELECT a.id, a.title, a.is_public, a.public_expires_at, u.name AS owner_name, a.likes
           FROM albums a LEFT JOIN users u ON u.id = a.owner_user_id
          WHERE a.token = ?",
        libsql::params![token],
    )
    .await
}

/// The album-access half of the `/api/media/[id]?token=` authorization
/// check: a live public album token grants access to that album's items.
/// `SELECT COUNT(*) > 0 FROM media m JOIN albums a ON a.id = m.album_id
/// WHERE m.id = ? AND a.token = ? AND a.is_public = 1 AND (… > ?)`.
pub async fn token_allows_media(db: &Db, media_id: i64, token: &str) -> Result<bool> {
    let n: i64 = db
        .scalar(
            "SELECT COUNT(*) FROM media m JOIN albums a ON a.id = m.album_id
              WHERE m.id = ? AND a.token = ? AND a.is_public = 1
                AND (a.public_expires_at IS NULL OR a.public_expires_at > ?)",
            libsql::params![media_id, token, now()],
        )
        .await?;
    Ok(n > 0)
}

/// One row of [`list_all_with_stats`] — the gallery's `AlbumModel`
/// shape: the album columns plus `item_count` and `cover_slug` from the
/// correlated subqueries.
#[derive(Debug, Clone, Deserialize)]
pub struct AlbumListRow {
    pub id: i64,
    pub slug: String,
    pub owner_user_id: Option<i64>,
    pub title: String,
    pub token: String,
    pub is_public: i64,
    pub public_expires_at: Option<i64>,
    pub views: i64,
    pub likes: i64,
    pub created_at: i64,
    pub item_count: i64,
    pub cover_slug: Option<String>,
}

/// The gallery album list — `app/gallery/page.tsx`'s `albumRows` query:
/// every album newest-first with its media count and first item's slug
/// (the card cover). Note: **all** albums regardless of owner, same as
/// the TS page (owners manage theirs; admins see the vault as one).
pub async fn list_all_with_stats(db: &Db) -> Result<Vec<AlbumListRow>> {
    db.query_all(
        "SELECT a.id, a.slug, a.owner_user_id, a.title, a.token, a.is_public, a.public_expires_at,
                a.views, a.likes, a.created_at,
                (SELECT COUNT(*) FROM media m WHERE m.album_id = a.id)   AS item_count,
                (SELECT m2.slug FROM media m2 WHERE m2.album_id = a.id
                 ORDER BY m2.id ASC LIMIT 1)                             AS cover_slug
           FROM albums a
          ORDER BY a.created_at DESC",
        (),
    )
    .await
}

/// `SELECT COUNT(*) AS n, COALESCE(SUM(is_public), 0) AS public_n FROM
/// albums` — the admin stats panel's album figure.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct AlbumTotals {
    pub n: i64,
    pub public_n: i64,
}

/// Album totals — admin vault stats.
pub async fn totals(db: &Db) -> Result<AlbumTotals> {
    db.query_one(
        "SELECT COUNT(*) AS n, COALESCE(SUM(is_public), 0) AS public_n FROM albums",
        (),
    )
    .await
}
