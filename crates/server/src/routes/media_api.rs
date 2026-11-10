//! Session-gated media endpoints — ports of:
//! - `src/app/api/media/route.ts`            → `GET /api/media` (cursor list)
//! - `src/app/api/media/[id]/share/route.ts` → `GET /api/media/{slug}/share`
//!   (live `/s/<token>` link list), `POST /api/media/{slug}/share`
//!   (`mode:"link"` mints `/s/<token>`; `mode:"telegram"` delivers through
//!   the bot), `DELETE /api/media/{slug}/share` (revoke one link).
//! - `/api/media/summary` — new: vault totals the gallery header used to
//!   compute inline (`{count, bytes}`, `media:view`-gated).
//! - `src/app/api/media/move/route.ts`       → `POST /api/media/move`
//!   (keeps the `route.test.ts` contract: per-owner `media:edit`, album
//!   target gated by `album:edit`, `{moved, failed, skipped}` counts and a
//!   single `media.move` audit entry).
//!
//! Authorization mirrors the TS handlers: `AuthUser` 401s without a
//! session, `rbac::can` 403s with `"Not allowed."`, and every mutating
//! body is read through `crate::body::json_body` (HMAC signing).
//!
//! `GET /media` decodes the query string by hand over [`RawQuery`] so
//! `URLSearchParams.get` semantics hold exactly: first occurrence of a
//! key wins, `+` and `%XX` decode, malformed pairs are ignored.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, RawQuery, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use openshares_core::audit::audit;
use openshares_core::bot::{BotContext, ShareMedia, deliver_share};
use openshares_core::crypto::security::{Limiter, rate_limited};
use openshares_core::db::repos::media::{
    DEFAULT_LIMIT, MAX_LIMIT, MediaCursor, MediaListItem, MediaListQuery, MediaScope,
};
use openshares_core::db::repos::{albums, friends, media, shares};
use openshares_core::db::{PUBLIC_LINK_TTL, now};
use openshares_core::util::{new_token, parse_int_prefix};

use crate::body::json_body;
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::routes::albums::too_many_requests;
use crate::state::AppState;

/// `MAX_BATCH` — `api/media/move` refuses batches over 100 items.
const MAX_BATCH: usize = 100;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/media", get(list_media))
        .route("/media/summary", get(media_summary))
        .route("/media/move", post(move_media))
        .route(
            "/media/{slug}/share",
            get(list_share_links)
                .post(create_share)
                .delete(revoke_share_link),
        )
}

/* -------------------------- GET /media -------------------------- */

/// `GET /api/media?scope=vault|album:<slug>&q=&kind=&missing=1&cursor=&limit=`
/// — the paginated gallery listing. `media:view` first (403), then scope
/// resolution: album scope 404s (`"Not found."`) for missing/non-owned
/// albums — existence hiding, same as `/album/[slug]`. Unknown scopes
/// 400 (`"Unknown scope."`).
async fn list_media(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
) -> Result<Response> {
    let uid = user.0.id;
    if !user.0.can("media:view", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    let get = |key: &str| param_get(raw.as_deref(), key);

    let scope_raw = get("scope").unwrap_or_else(|| "vault".to_string());
    let scope = if scope_raw == "vault" || scope_raw.is_empty() {
        MediaScope::Vault
    } else if let Some(album_slug) = scope_raw.strip_prefix("album:") {
        let album = albums::get_by_slug(&state.db, album_slug).await?;
        match album {
            Some(album)
                if album.owner_user_id == Some(uid) || user.0.can("album:edit:any", None) =>
            {
                MediaScope::Album(album.id)
            }
            _ => return Err(ApiError::not_found("Not found.")),
        }
    } else {
        return Err(ApiError::bad_request("Unknown scope."));
    };

    // Cursors that don't parse are ignored — the TS `Number.isFinite`
    // guards apply the predicate only for well-formed cursors.
    let cursor = match scope {
        MediaScope::Vault => get("cursor").as_deref().and_then(vault_cursor),
        MediaScope::Album(_) => get("cursor").as_deref().and_then(album_cursor),
    };
    let search = get("q")
        .map(|q| q.trim().chars().take(200).collect::<String>())
        .filter(|q| !q.is_empty());
    let kind = get("kind").filter(|k| matches!(k.as_str(), "photo" | "video"));
    let missing_only = get("missing").as_deref() == Some("1");
    // `parseInt(p, 10) || 60` → clamped into [1, 100].
    let raw_limit = get("limit")
        .as_deref()
        .and_then(parse_int_prefix)
        .unwrap_or_default();
    let limit = if raw_limit == 0 {
        DEFAULT_LIMIT
    } else {
        raw_limit.clamp(1, MAX_LIMIT)
    };

    let page = media::list(
        &state.db,
        &MediaListQuery {
            scope,
            search: search.as_deref(),
            kind: kind.as_deref(),
            missing_only,
            cursor,
            limit,
        },
    )
    .await?;

    Ok(Json(json!({
        "items": page.items.iter().map(item_json).collect::<Vec<_>>(),
        "nextCursor": page.next_cursor,
    }))
    .into_response())
}

/// The gallery item shape (`toItem` in the TS route).
fn item_json(m: &MediaListItem) -> Value {
    json!({
        "slug": m.slug,
        "ownerId": m.owner_user_id,
        "kind": m.kind,
        "caption": m.caption,
        "width": m.width,
        "height": m.height,
        "duration": m.duration,
        "fileSize": m.file_size,
        "availability": m.availability,
        "sharesCount": m.shares_count,
        "createdAt": m.created_at,
    })
}

/// `?k=v&k2=v2` first-wins lookup — `URLSearchParams.get`. Both the key
/// and the value are percent-decoded (`+` → space).
fn param_get(raw: Option<&str>, key: &str) -> Option<String> {
    let raw = raw?;
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if percent_decode(k) == key {
            return Some(percent_decode(v));
        }
    }
    None
}

/// Percent-decoder matching `decodeURIComponent`-in-query semantics:
/// `%XX` → byte, `+` → space, malformed `%` left literal, non-UTF-8 →
/// U+FFFD via `from_utf8_lossy` (same as `URLSearchParams`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `Number(s)` → finite → `i64` — `""`/whitespace → `Some(0)` (JS
/// `Number("")` is 0), hex `0x…` like `Number("0x10")`, garbage → None.
fn js_str_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    t.parse::<f64>().ok()
}

fn js_num_i64(s: &str) -> Option<i64> {
    js_str_num(s).filter(|n| n.is_finite()).map(|n| n as i64)
}

/// Vault cursor — `cursor.split(":").map(Number)` destructured `[c, i]`
/// in the TS route: the first two segments only, both must be finite.
fn vault_cursor(s: &str) -> Option<MediaCursor> {
    let mut parts = s.split(':');
    let created_at = js_num_i64(parts.next().unwrap_or(""))?;
    let id = js_num_i64(parts.next().unwrap_or(""))?;
    Some(MediaCursor::Vault { created_at, id })
}

/// Album cursor — `Number(cursor)` applied when finite.
fn album_cursor(s: &str) -> Option<MediaCursor> {
    js_num_i64(s).map(|id| MediaCursor::Album { id })
}

/// `GET /api/media/summary` — `{count, bytes}` vault-wide totals for
/// the gallery header (the old page's inline `COUNT(*)`/`SUM(file_size)`).
/// Same `media:view` gate as the listing.
async fn media_summary(State(state): State<AppState>, user: AuthUser) -> Result<Response> {
    if !user.0.can("media:view", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    let t = media::totals(&state.db).await?;
    Ok(Json(json!({ "count": t.n, "bytes": t.bytes })).into_response())
}

/* ---------------------- /media/{slug}/share ---------------------- */

/// `GET /api/media/{slug}/share` — live `/s/<token>` links for one item.
/// `share:link` required (403); `media:delete:any` holders see every
/// user's links, others only their own (`created_by = NULL` → all in
/// `shares::list_links`).
async fn list_share_links(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
) -> Result<Response> {
    let uid = user.0.id;
    let media = media::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;
    if !user.0.can("share:link", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    let see_all = user.0.can("media:delete:any", None);
    let rows =
        shares::list_links(&state.db, media.id, if see_all { None } else { Some(uid) }).await?;

    let links = rows
        .iter()
        .map(|r| {
            let token = r.token.clone().unwrap_or_default();
            json!({
                "token": r.token,
                "url": format!("{}/s/{}", state.config.app_url, token),
                "createdAt": r.created_at,
                "expiresAt": r.expires_at,
                "views": r.views,
                "likes": r.likes,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "links": links })).into_response())
}

/// `DELETE /api/media/{slug}/share { token }` — revoke one link. Own
/// links need `share:link` (checked before the row exists, like the TS
/// route); revoking someone else's needs `media:delete:any`. Audit:
/// `share.link.revoke` with the token's first 8 chars as detail.
async fn revoke_share_link(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    let body: Map<String, Value> = json_body(req, Some(&user.0.sid)).await?;
    let token = body
        .get("token")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if token.is_empty() {
        return Err(ApiError::bad_request("Missing token."));
    }

    let media = media::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;
    if !user.0.can("share:link", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }
    let Some(created_by) = shares::get_link_creator(&state.db, media.id, token).await? else {
        return Err(ApiError::not_found("Link not found."));
    };
    let own = created_by == Some(uid);
    if !own && !user.0.can("media:delete:any", None) {
        return Err(ApiError::forbidden("Not allowed."));
    }

    shares::delete_link(&state.db, media.id, token).await?;
    let detail: String = token.chars().take(8).collect();
    audit(
        &state.cache,
        Some(uid),
        "share.link.revoke",
        Some("media"),
        Some(&media.slug),
        Some(&detail),
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// `POST /api/media/{slug}/share` — `{mode:"link"}` mints a public
/// `/s/<token>` (`share:link`); `{mode:"telegram", friend_id}` delivers
/// the file into a friend's chat via the bot (`share:friend`). The
/// route is `API_LIMITER`-rated on `share:<uid>` before anything else.
async fn create_share(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    if rate_limited(&state.db, Limiter::Api, &format!("share:{uid}")).await {
        return Ok(too_many_requests());
    }
    let media = media::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;
    let body: Map<String, Value> = json_body(req, Some(&user.0.sid)).await?;

    match body
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
    {
        "link" => {
            if !user.0.can("share:link", None) {
                return Err(ApiError::forbidden("Not allowed."));
            }
            let token = new_token(16);
            let expires_at = now() + PUBLIC_LINK_TTL;
            shares::create_link(&state.db, media.id, &token, uid, expires_at).await?;
            media::increment_shares_count(&state.db, media.id).await?;
            audit(
                &state.cache,
                Some(uid),
                "share.link.create",
                Some("media"),
                Some(&slug),
                None,
            )
            .await;
            Ok(Json(json!({
                "mode": "link",
                "url": format!("{}/s/{}", state.config.app_url, token),
                "token": token,
                "expiresAt": expires_at,
            }))
            .into_response())
        }
        "telegram" => {
            if !user.0.can("share:friend", None) {
                return Err(ApiError::forbidden("Not allowed."));
            }
            // `Number(body.friend_id ?? -1)` — strings coerce, anything
            // unresolvable lands on the same 400 below.
            let friend_id = match body.get("friend_id") {
                Some(Value::Number(n)) => n.as_i64().unwrap_or(-1),
                Some(Value::String(s)) => js_num_i64(s).unwrap_or(-1),
                _ => -1,
            };
            let Some(friend) = friends::get_link_target(&state.db, friend_id, uid).await? else {
                return Err(ApiError::bad_request("Pick a friend on your list first."));
            };
            if media.missing != 0 {
                return Err(ApiError::new(
                    StatusCode::GONE,
                    "The original Telegram file is gone — it can't be re-delivered.",
                ));
            }
            let Some(tg) = state.tg.clone() else {
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "Bot is not configured.",
                ));
            };
            let ctx = BotContext {
                db: state.db.clone(),
                tg,
                app_url: state.config.app_url.clone(),
                cache: state.cache.clone(),
            };
            let share_media = ShareMedia {
                id: Some(media.id),
                kind: media.kind.clone(),
                file_id: media.file_id.clone(),
                caption: media.caption.clone(),
            };
            // `user.name || "a friend"` — empty string is falsy in JS.
            let owner_name = if user.0.name.is_empty() {
                "a friend"
            } else {
                user.0.name.as_str()
            };
            if let Err(e) = deliver_share(&ctx, &share_media, &friend.telegram_id, owner_name).await
            {
                // `/chat not found|blocked|bot was/i` → the friendly hint;
                // anything else surfaces the raw Telegram error.
                let msg = e.to_string();
                let lower = msg.to_lowercase();
                let friendly = if lower.contains("chat not found")
                    || lower.contains("blocked")
                    || lower.contains("bot was")
                {
                    "Your friend hasn't started the bot yet (or blocked it). They need to press Start in the bot chat once.".to_string()
                } else {
                    msg
                };
                return Err(ApiError::new(StatusCode::BAD_GATEWAY, friendly));
            }
            shares::create_telegram(&state.db, media.id, &friend.telegram_id, uid).await?;
            media::increment_shares_count(&state.db, media.id).await?;
            let detail = format!("tg:{}", friend.telegram_id);
            audit(
                &state.cache,
                Some(uid),
                "share.friend.send",
                Some("media"),
                Some(&slug),
                Some(&detail),
            )
            .await;
            Ok(Json(json!({ "mode": "telegram", "ok": true })).into_response())
        }
        _ => Err(ApiError::bad_request("Unknown mode.")),
    }
}

/* ------------------------ POST /media/move ------------------------ */

/// Validated move target — `albumId: null` files back into the vault.
enum AlbumTarget {
    Vault,
    Id(i64),
}

/// `body.slugs` — must be a JSON array of ≤ `MAX_BATCH` entries;
/// non-strings/empties filtered, then deduped preserving order (the TS
/// `[...new Set(...)]`). Errors are the TS route's exact messages.
fn parse_slugs(raw: Option<&Value>) -> Result<Vec<String>> {
    let Some(Value::Array(items)) = raw else {
        return Err(ApiError::bad_request("No items selected."));
    };
    if items.len() > MAX_BATCH {
        return Err(ApiError::bad_request(format!(
            "Move at most {MAX_BATCH} items at a time."
        )));
    }
    let mut seen = HashSet::new();
    let mut slugs = Vec::new();
    for item in items {
        if let Value::String(s) = item {
            if !s.is_empty() && seen.insert(s.clone()) {
                slugs.push(s.clone());
            }
        }
    }
    if slugs.is_empty() {
        return Err(ApiError::bad_request("No items selected."));
    }
    Ok(slugs)
}

/// `body.albumId` — `=== null` → vault; otherwise `Number(v)` must be a
/// positive integer (`"5"` counts, `"nope"`/`5.5`/booleans-that-aren't-1
/// don't). Absent → `Number(undefined)` → NaN → invalid, same as TS.
fn parse_album_target(raw: Option<&Value>) -> Result<AlbumTarget> {
    match raw {
        None => Err(ApiError::bad_request("Invalid album.")),
        Some(Value::Null) => Ok(AlbumTarget::Vault),
        Some(v) => {
            let n = match v {
                Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                Value::Number(n) => n.as_f64(),
                Value::String(s) => js_str_num(s),
                _ => None,
            };
            match n {
                Some(n) if n.is_finite() && n.fract() == 0.0 && n > 0.0 && n <= i64::MAX as f64 => {
                    Ok(AlbumTarget::Id(n as i64))
                }
                _ => Err(ApiError::bad_request("Invalid album.")),
            }
        }
    }
}

/// `POST /api/media/move { slugs: string[], albumId: number | null }` —
/// batch re-file into an album or back to the vault. Per-item
/// `media:edit` on the item's owner (one permission lookup per distinct
/// owner), `album:edit` on the target; already-there rows are `skipped`,
/// missing slugs and denied items count `failed`. One `media.move`
/// audit entry, only when `moved > 0`.
async fn move_media(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    let body: Map<String, Value> = json_body(req, Some(&user.0.sid)).await?;

    let slugs = parse_slugs(body.get("slugs"))?;
    let album = match parse_album_target(body.get("albumId"))? {
        AlbumTarget::Vault => None,
        AlbumTarget::Id(id) => {
            let album = albums::get_by_id(&state.db, id)
                .await?
                .ok_or_else(|| ApiError::not_found("Album not found."))?;
            if !user.0.can("album:edit", album.owner_user_id) {
                return Err(ApiError::forbidden("Not allowed."));
            }
            Some(album)
        }
    };

    let rows = media::get_many_by_slugs(&state.db, &slugs).await?;
    let target_id = album.as_ref().map(|a| a.id);

    // One permission lookup per distinct owner, not per row.
    let mut owner_allowed: HashMap<Option<i64>, bool> = HashMap::new();
    let (mut moved, mut failed, mut skipped) = (0i64, 0i64, 0i64);
    for row in &rows {
        if row.album_id == target_id {
            skipped += 1;
            continue;
        }
        let allowed = match owner_allowed.get(&row.owner_user_id) {
            Some(v) => *v,
            None => {
                let v = user.0.can("media:edit", row.owner_user_id);
                owner_allowed.insert(row.owner_user_id, v);
                v
            }
        };
        if !allowed {
            failed += 1;
            continue;
        }
        media::set_album_id(&state.db, row.id, target_id).await?;
        moved += 1;
    }
    failed += slugs.len() as i64 - rows.len() as i64; // slugs that don't exist anymore

    if moved > 0 {
        let target_slug = album
            .as_ref()
            .map(|a| a.slug.clone())
            .unwrap_or_else(|| "vault".to_string());
        let detail = format!(
            "{moved} item(s) → {}",
            album.as_ref().map(|a| a.title.as_str()).unwrap_or("vault")
        );
        audit(
            &state.cache,
            Some(uid),
            "media.move",
            Some("album"),
            Some(&target_slug),
            Some(&detail),
        )
        .await;
    }
    Ok(
        Json(json!({ "ok": true, "moved": moved, "failed": failed, "skipped": skipped }))
            .into_response(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_get_first_wins_and_decodes() {
        let raw = Some("a=1&a=2&b=x+y&c=%2Fz%26w&flag".to_string());
        assert_eq!(param_get(raw.as_deref(), "a"), Some("1".to_string()));
        assert_eq!(param_get(raw.as_deref(), "b"), Some("x y".to_string()));
        assert_eq!(param_get(raw.as_deref(), "c"), Some("/z&w".to_string()));
        assert_eq!(param_get(raw.as_deref(), "flag"), Some("".to_string()));
        assert_eq!(param_get(raw.as_deref(), "nope"), None);
        assert_eq!(param_get(None, "a"), None);
    }

    #[test]
    fn percent_decode_edge_cases() {
        assert_eq!(percent_decode("%41%42"), "AB");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%"), "%");
        // encoded slash stays decoded (path is already routed by then)
        assert_eq!(percent_decode("a%2fb"), "a/b");
    }

    #[test]
    fn js_num_semantics() {
        assert_eq!(js_num_i64("5"), Some(5));
        assert_eq!(js_num_i64(" 42 "), Some(42));
        assert_eq!(js_num_i64(""), Some(0)); // Number("") === 0
        assert_eq!(js_num_i64("0x10"), Some(16));
        assert_eq!(js_num_i64("abc"), None);
        assert_eq!(js_num_i64("Infinity"), None);
        assert_eq!(js_num_i64("5.9"), Some(5)); // truncated — garbage anyway
    }

    #[test]
    fn cursor_parsing() {
        match vault_cursor("1700000000:42") {
            Some(MediaCursor::Vault { created_at, id }) => {
                assert_eq!((created_at, id), (1700000000, 42));
            }
            _ => panic!("expected vault cursor"),
        }
        assert!(vault_cursor("garbage").is_none());
        assert!(vault_cursor("1:2:3").is_some()); // extra parts ignored (TS destructure)
        match album_cursor("42") {
            Some(MediaCursor::Album { id }) => assert_eq!(id, 42),
            _ => panic!("expected album cursor"),
        }
        assert!(album_cursor("nope").is_none());
    }

    #[test]
    fn slugs_validation() {
        // not an array
        assert_eq!(
            parse_slugs(Some(&json!("x"))).unwrap_err().status,
            axum::http::StatusCode::BAD_REQUEST
        );
        assert!(parse_slugs(None).is_err());
        // oversized batch counts raw entries
        let big = json!((0..=MAX_BATCH).map(|i| i.to_string()).collect::<Vec<_>>());
        assert_eq!(
            parse_slugs(Some(&big)).unwrap_err().message,
            "Move at most 100 items at a time."
        );
        // filter + dedupe + order
        let v = json!(["a", "", "b", 3, "a", null]);
        assert_eq!(
            parse_slugs(Some(&v)).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        // all-filtered → empty → error
        assert!(parse_slugs(Some(&json!(["", 5]))).is_err());
    }

    #[test]
    fn album_target_parsing() {
        // absent → Number(undefined) → NaN → invalid
        assert!(parse_album_target(None).is_err());
        // null → vault
        assert!(matches!(
            parse_album_target(Some(&Value::Null)).unwrap(),
            AlbumTarget::Vault
        ));
        match parse_album_target(Some(&json!(5))).unwrap() {
            AlbumTarget::Id(5) => {}
            _ => panic!("expected album id 5"),
        }
        // numeric strings count (Number("5"))
        assert!(matches!(
            parse_album_target(Some(&json!("5"))).unwrap(),
            AlbumTarget::Id(5)
        ));
        for bad in [
            json!("nope"),
            json!(5.5),
            json!(0),
            json!(-3),
            json!({}),
            json!([1]),
        ] {
            assert!(
                parse_album_target(Some(&bad)).is_err(),
                "expected invalid album target: {bad}"
            );
        }
        // true → 1 (JS Number(true)); false → 0 → invalid
        assert!(matches!(
            parse_album_target(Some(&json!(true))).unwrap(),
            AlbumTarget::Id(1)
        ));
        assert!(parse_album_target(Some(&json!(false))).is_err());
    }
}
