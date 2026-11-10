//! Media repository — ports the `media`-table queries in
//! `src/app/api/media/**/route.ts`, the album-file INSERT in
//! `api/albums/[slug]/files/route.ts`, the public album paging in
//! `api/public/album/route.ts`, and the vault stats in `app/admin/page.tsx`.
//!
//! Deletion semantics match the TS routes: `DELETE FROM media` is a
//! **hard** delete (the file itself stays in Telegram; `shares` rows
//! cascade). "Soft" state is the `missing` flag — a dead `file_id` gets
//! marked and surfaces as `'dead'` in listings, it is never removed.

use serde::Deserialize;

use crate::db::{Db, now};
use crate::error::Result;
use crate::models::Media;

/// Page-size contract from `GET /api/media`: default 60, hard max 100
/// (the repo clamps to the max; the route still applies its default).
pub const DEFAULT_LIMIT: i64 = 60;
pub const MAX_LIMIT: i64 = 100;

/// The listing SELECT — `availability` folds the `missing` flag into a
/// `'dead'` pseudo-value exactly like the TS query (kept out of the
/// CHECK-constrained column on purpose, see migration 0007).
const LIST_SELECT: &str = "SELECT id, slug, owner_user_id, kind, caption, width, height, duration, file_size, shares_count,
       CASE WHEN missing THEN 'dead' ELSE availability END AS availability, created_at
  FROM media";

/// One row of [`LIST_SELECT`] — the gallery item shape.
#[derive(Debug, Clone, Deserialize)]
pub struct MediaListItem {
    pub id: i64,
    pub slug: String,
    pub owner_user_id: Option<i64>,
    pub kind: String,
    pub caption: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    pub file_size: Option<i64>,
    pub shares_count: i64,
    /// 'available' | 'telegram-only' | 'dead'.
    pub availability: String,
    pub created_at: i64,
}

/// Which media set a listing reads — `scope=vault` (unfiled items,
/// `(created_at, id) DESC` on `idx_media_unfiled`) or `scope=album:<id>`
/// (`id ASC` on `idx_media_album_id`). The route resolves the album slug
/// to `albums.id` first, so the repo only sees the id.
#[derive(Debug, Default, Clone, Copy)]
pub enum MediaScope {
    #[default]
    Vault,
    Album(i64),
}

/// Keyset cursor — vault pages continue below a `(created_at, id)` pair,
/// album pages continue after an `id`. Wire format is the route's
/// `"<created_at>:<id>"` / `"<id>"` strings (see [`parse_vault_cursor`] /
/// [`parse_album_cursor`]).
#[derive(Debug, Clone, Copy)]
pub enum MediaCursor {
    Vault { created_at: i64, id: i64 },
    Album { id: i64 },
}

/// Everything `GET /api/media` can filter/page by. `kind` is only ever
/// `"photo"`/`"video"` (validated by the caller, like the TS route).
#[derive(Debug, Default)]
pub struct MediaListQuery<'a> {
    pub scope: MediaScope,
    /// Raw search text — [`like_pattern`] escapes it before binding.
    pub search: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub missing_only: bool,
    pub cursor: Option<MediaCursor>,
    pub limit: i64,
}

/// Result page — `next_cursor` is the opaque string the client echoes
/// back (`"<created_at>:<id>"` for vault, `"<id>"` for albums), `None`
/// when the page was short.
#[derive(Debug)]
pub struct MediaPage {
    pub items: Vec<MediaListItem>,
    pub next_cursor: Option<String>,
}

/// `likePattern` — escape `\`, `%`, `_` so the search text stays
/// literal inside the `%…%` pattern (paired with `ESCAPE '\'`).
pub fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// Vault cursor `"<created_at>:<id>"` → keyset (`None` on garbage —
/// the TS route ignores unparseable cursors too).
pub fn parse_vault_cursor(s: &str) -> Option<MediaCursor> {
    let (c, i) = s.split_once(':')?;
    Some(MediaCursor::Vault {
        created_at: c.parse().ok()?,
        id: i.parse().ok()?,
    })
}

/// Album cursor `"<id>"` → keyset.
pub fn parse_album_cursor(s: &str) -> Option<MediaCursor> {
    Some(MediaCursor::Album {
        id: s.parse().ok()?,
    })
}

/// `GET /api/media` — keyset-paginated listing. Builds the same SQL the
/// TS route assembles: scope WHERE first (vault = `album_id IS NULL`),
/// then `caption LIKE ? ESCAPE '\'` / `kind = ?` / `missing = 1`, the
/// cursor predicate appended last, `LIMIT ?` bound to `limit + 1` so a
/// short page means "no more".
///
/// A mismatched cursor kind (e.g. an album cursor on the vault scope) is
/// ignored, matching the TS `if` ladders that only apply the predicate
/// for the matching shape.
pub async fn list(db: &Db, query: &MediaListQuery<'_>) -> Result<MediaPage> {
    let mut where_parts: Vec<&str> = Vec::new();
    let order = match query.scope {
        MediaScope::Vault => {
            where_parts.push("album_id IS NULL");
            "ORDER BY created_at DESC, id DESC"
        }
        MediaScope::Album(_) => {
            where_parts.push("album_id = ?");
            "ORDER BY id ASC"
        }
    };
    if query.search.is_some() {
        where_parts.push("caption LIKE ? ESCAPE '\\'");
    }
    if query.kind.is_some() {
        where_parts.push("kind = ?");
    }
    if query.missing_only {
        where_parts.push("missing = 1");
    }

    let mut sql = format!("{LIST_SELECT} WHERE {}", where_parts.join(" AND "));
    match (&query.scope, &query.cursor) {
        (MediaScope::Vault, Some(MediaCursor::Vault { .. })) => {
            sql.push_str(" AND (created_at < ? OR (created_at = ? AND id < ?))");
        }
        (MediaScope::Album(_), Some(MediaCursor::Album { .. })) => {
            sql.push_str(" AND id > ?");
        }
        _ => {}
    }
    sql.push(' ');
    sql.push_str(order);
    sql.push_str(" LIMIT ?");

    // Bind in placeholder order: scope → search → kind → cursor → limit.
    let mut vals: Vec<libsql::Value> = Vec::new();
    if let MediaScope::Album(id) = query.scope {
        vals.push(id.into());
    }
    if let Some(s) = query.search {
        vals.push(like_pattern(s).into());
    }
    if let Some(k) = query.kind {
        vals.push(k.into());
    }
    match (&query.scope, &query.cursor) {
        (MediaScope::Vault, Some(MediaCursor::Vault { created_at, id })) => {
            vals.push((*created_at).into());
            vals.push((*created_at).into());
            vals.push((*id).into());
        }
        (MediaScope::Album(_), Some(MediaCursor::Album { id })) => {
            vals.push((*id).into());
        }
        _ => {}
    }
    let limit = query.limit.clamp(1, MAX_LIMIT);
    vals.push((limit + 1).into());

    let mut rows = db
        .query_all::<MediaListItem>(&sql, libsql::params_from_iter(vals))
        .await?;
    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }
    let next_cursor = if has_more {
        rows.last().map(|last| match query.scope {
            MediaScope::Vault => format!("{}:{}", last.created_at, last.id),
            MediaScope::Album(_) => last.id.to_string(),
        })
    } else {
        None
    };
    Ok(MediaPage {
        items: rows,
        next_cursor,
    })
}

/// `loadMedia(idOrSlug)` — the streaming/patch/delete route loads by
/// slug (the `:id` path segment is really the media slug everywhere in
/// the TS app).
pub async fn get_by_slug(db: &Db, slug: &str) -> Result<Option<Media>> {
    db.query_opt("SELECT * FROM media WHERE slug = ?", libsql::params![slug])
        .await
}

/// `WHERE slug IN (?, …)` — the batch lookup in `api/media/move`. Slug
/// order in the result is unspecified (same as the TS `IN` query).
pub async fn get_many_by_slugs(db: &Db, slugs: &[String]) -> Result<Vec<Media>> {
    if slugs.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; slugs.len()].join(", ");
    let sql = format!("SELECT * FROM media WHERE slug IN ({placeholders})");
    let vals: Vec<libsql::Value> = slugs.iter().map(|s| s.as_str().into()).collect();
    db.query_all(&sql, libsql::params_from_iter(vals)).await
}

/// Columns for the web-upload INSERT (`api/albums/[slug]/files`) and the
/// bot ingestion insert — one shape covers both (the bot also fills
/// `media_group_id` from its own inline query; the field is here for
/// parity).
#[derive(Debug, Default)]
pub struct NewMedia<'a> {
    pub slug: &'a str,
    pub owner_user_id: Option<i64>,
    /// Sender's Telegram id or the literal `'upload'` provenance marker.
    pub telegram_id: &'a str,
    pub album_id: Option<i64>,
    pub kind: &'a str,
    pub file_id: &'a str,
    pub file_unique_id: Option<&'a str>,
    pub thumb_file_id: Option<&'a str>,
    pub file_size: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    pub mime: Option<&'a str>,
    pub caption: Option<&'a str>,
    /// 'available' | 'telegram-only'.
    pub availability: &'a str,
    pub media_group_id: Option<&'a str>,
}

/// Media INSERT — `shares_count` starts at 0, `created_at` at now;
/// returns the new id (`RETURNING id` replaces `lastInsertRowid`,
/// race-free on the shared connection).
pub async fn insert(db: &Db, new: &NewMedia<'_>) -> Result<i64> {
    db.scalar(
        "INSERT INTO media
           (slug, owner_user_id, telegram_id, album_id, kind, file_id, file_unique_id, thumb_file_id,
            file_size, width, height, duration, mime, caption, shares_count, availability, created_at, media_group_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?, ?) RETURNING id",
        libsql::params![
            new.slug,
            new.owner_user_id,
            new.telegram_id,
            new.album_id,
            new.kind,
            new.file_id,
            new.file_unique_id,
            new.thumb_file_id,
            new.file_size,
            new.width,
            new.height,
            new.duration,
            new.mime,
            new.caption,
            new.availability,
            now(),
            new.media_group_id
        ],
    )
    .await
}

/// `PATCH /api/media/[id]` — `UPDATE media SET caption = ? WHERE id = ?`
/// (`None` clears; the route trims + truncates to 1000 chars first).
pub async fn update_caption(db: &Db, id: i64, caption: Option<&str>) -> Result<()> {
    db.execute(
        "UPDATE media SET caption = ? WHERE id = ?",
        libsql::params![caption, id],
    )
    .await?;
    Ok(())
}

/// `DELETE /api/media/[id]` — hard delete. `shares` rows cascade
/// (`ON DELETE CASCADE`); `album_id`-holding albums are unaffected.
/// Returns whether a row was removed.
pub async fn delete_by_id(db: &Db, id: i64) -> Result<bool> {
    let affected = db
        .execute("DELETE FROM media WHERE id = ?", libsql::params![id])
        .await?;
    Ok(affected > 0)
}

/// `UPDATE media SET missing = 1 WHERE id = ?` — flag a dead `file_id`
/// (self-heal on failed Telegram fetch, same as the streaming route).
pub async fn mark_missing(db: &Db, id: i64) -> Result<()> {
    db.execute(
        "UPDATE media SET missing = 1 WHERE id = ?",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

/// `api/media/move` — `UPDATE media SET album_id = ? WHERE id = ?`
/// (`None` re-files into the vault).
pub async fn set_album_id(db: &Db, id: i64, album_id: Option<i64>) -> Result<()> {
    db.execute(
        "UPDATE media SET album_id = ? WHERE id = ?",
        libsql::params![album_id, id],
    )
    .await?;
    Ok(())
}

/// `UPDATE media SET shares_count = shares_count + 1 WHERE id = ?` —
/// both share modes bump it after inserting the `shares` row.
pub async fn increment_shares_count(db: &Db, id: i64) -> Result<()> {
    db.execute(
        "UPDATE media SET shares_count = shares_count + 1 WHERE id = ?",
        libsql::params![id],
    )
    .await?;
    Ok(())
}

/// `SELECT COUNT(*) FROM media WHERE album_id = ?` — the "N items"
/// figure on album cards / the public album header.
pub async fn count_in_album(db: &Db, album_id: i64) -> Result<i64> {
    db.scalar(
        "SELECT COUNT(*) FROM media WHERE album_id = ?",
        libsql::params![album_id],
    )
    .await
}

/// First live item's slug — the album cover (`ORDER BY id ASC LIMIT 1`,
/// `missing = 0` like the /a OG-metadata query).
pub async fn first_live_slug_in_album(db: &Db, album_id: i64) -> Result<Option<String>> {
    db.scalar_opt(
        "SELECT slug FROM media WHERE album_id = ? AND missing = 0 ORDER BY id ASC LIMIT 1",
        libsql::params![album_id],
    )
    .await
}

/* ------------------------- vault stats (admin) ------------------------- */

/// `SELECT COUNT(*) AS n, COALESCE(SUM(file_size), 0) AS bytes FROM media`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct MediaTotals {
    pub n: i64,
    pub bytes: i64,
}

/// Vault totals — the admin stats panel header.
pub async fn totals(db: &Db) -> Result<MediaTotals> {
    db.query_one(
        "SELECT COUNT(*) AS n, COALESCE(SUM(file_size), 0) AS bytes FROM media",
        (),
    )
    .await
}

/// `(kind, n)` — `SELECT kind, COUNT(*) … GROUP BY kind`.
pub async fn counts_by_kind(db: &Db) -> Result<Vec<(String, i64)>> {
    let rows = db
        .query_rows("SELECT kind, COUNT(*) AS n FROM media GROUP BY kind", ())
        .await?;
    rows.iter()
        .map(|r| Ok((r.get::<String>(0)?, r.get::<i64>(1)?)))
        .collect()
}

/// `(availability, n)` with the `missing → 'dead'` fold —
/// `GROUP BY availability` on the CASE expression (the TS query keeps
/// the same alias/label).
pub async fn counts_by_availability(db: &Db) -> Result<Vec<(String, i64)>> {
    let rows = db
        .query_rows(
            "SELECT CASE WHEN missing THEN 'dead' ELSE availability END AS availability, COUNT(*) AS n
               FROM media GROUP BY availability",
            (),
        )
        .await?;
    rows.iter()
        .map(|r| Ok((r.get::<String>(0)?, r.get::<i64>(1)?)))
        .collect()
}

/// `(owner_name, n, bytes)` — top storage owners,
/// `GROUP BY m.owner_user_id ORDER BY bytes DESC LIMIT ?`.
pub async fn top_owners(db: &Db, limit: i64) -> Result<Vec<(String, i64, i64)>> {
    let rows = db
        .query_rows(
            "SELECT COALESCE(u.name, 'unknown') AS name, COUNT(*) AS n, COALESCE(SUM(m.file_size), 0) AS bytes
               FROM media m LEFT JOIN users u ON u.id = m.owner_user_id
              GROUP BY m.owner_user_id ORDER BY bytes DESC LIMIT ?",
            libsql::params![limit],
        )
        .await?;
    rows.iter()
        .map(|r| Ok((r.get::<String>(0)?, r.get::<i64>(1)?, r.get::<i64>(2)?)))
        .collect()
}
