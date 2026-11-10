//! `GET /api/media/{slug}` — port of `src/app/api/media/[id]/route.ts`
//! (the GET handler only — the path param is the media `slug` everywhere
//! in the TS app, despite the `[id]` folder name).
//!
//! Authorization mirrors the TS `authorized()`: a session user who owns
//! the item or holds `media:view`, OR a `?token=` that is either a live
//! `'link'` share for this media or a live public-album token covering
//! it. The stream itself is delegated to [`crate::media::serve_media`]
//! (cache hit → ranged `FsCache` read; miss → Telegram CDN tee), which
//! owns the dead-`file_id` self-heal (`missing = 1` → 410) the TS route
//! did inline.
//!
//! PATCH (caption edit) and DELETE are session-gated — a signed body
//! plus `media:edit`/`media:delete` scoped to the item's owner. Both
//! collapse every failure (no session, missing media, denied
//! permission) into the same `403 "Not allowed."`, exactly like the TS
//! `!media || !user || !can(...)` check — no existence leak.

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use openshares_core::audit::audit;
use openshares_core::crypto::password::SessionUser;
use openshares_core::db::repos::{albums, media, shares};
use openshares_core::models::Media;

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::MaybeUser;
use crate::state::AppState;

/// Route table — merged by `routes::public_router` under `/api`.
/// PATCH/DELETE do their own session resolution (`MaybeUser`) so the
/// TS route's 403-collapse holds; they are NOT token-authorized.
pub fn router() -> Router<AppState> {
    Router::new().route(
        "/media/{slug}",
        get(get_media).patch(patch_media).delete(delete_media),
    )
}

#[derive(Debug, Deserialize)]
struct MediaStreamQuery {
    token: Option<String>,
    thumb: Option<String>,
    download: Option<String>,
}

/// `authorized()` — same three gates, same order as the TS route:
/// session first (owner or `media:view`), then `?token=` against the
/// `'link'` shares and the public-album grant.
async fn authorized(
    state: &AppState,
    media: &Media,
    token: Option<&str>,
    user: Option<&SessionUser>,
) -> Result<bool> {
    if let Some(u) = user {
        if media.owner_user_id == Some(u.id) || u.can("media:view", None) {
            return Ok(true);
        }
    }
    if let Some(token) = token {
        if shares::live_link_exists(&state.db, media.id, token).await? {
            return Ok(true);
        }
        // A live public-album token grants access to that album's items.
        return Ok(albums::token_allows_media(&state.db, media.id, token).await?);
    }
    Ok(false)
}

async fn get_media(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<MediaStreamQuery>,
    headers: HeaderMap,
    maybe_user: MaybeUser,
) -> Result<Response> {
    let media = media::get_by_slug(&state.db, &slug)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;

    let token = query.token.as_deref().filter(|t| !t.is_empty());
    if !authorized(&state, &media, token, maybe_user.0.as_ref()).await? {
        return Err(ApiError::forbidden("Not allowed."));
    }
    if media.availability == "telegram-only" {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "This file is too big for the Bot API to download (>20 MB). It stays viewable in Telegram.",
        ));
    }
    if media.missing != 0 {
        return Err(ApiError::new(
            StatusCode::GONE,
            "The original Telegram file is no longer reachable (bot changed or message deleted).",
        ));
    }

    // `?thumb=1` renders the smaller thumb (photo sizes / video poster)
    // when the media row has one; otherwise the original file.
    let file_id = match query.thumb.as_deref() {
        Some("1") => media
            .thumb_file_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&media.file_id),
        _ => &media.file_id,
    };
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    let mut response = match crate::media::serve_media(
        &state,
        file_id,
        media.mime.as_deref(),
        range,
    )
    .await
    {
        Ok(r) => r,
        // Dead file_id upstream → flag the row and 410 (the TS route's
        // self-heal — `serve_media` can't do it, it only sees file_ids).
        Err(e) if e.status == StatusCode::NOT_FOUND => {
            let _ = media::mark_missing(&state.db, media.id).await;
            return Err(ApiError::new(
                StatusCode::GONE,
                "The original Telegram file is no longer reachable (bot changed or message deleted).",
            ));
        }
        Err(e) => return Err(e),
    };

    // `?download=1` → save-as dialog with a real filename instead of
    // inline display. The TS route sets this header on every streaming
    // path (200 / 206 / cache hit) — same here, applied to whatever
    // `serve_media` produced.
    if query.download.as_deref() == Some("1") {
        let base_type = media.mime.as_deref().unwrap_or(if media.kind == "photo" {
            "image/jpeg"
        } else {
            "video/mp4"
        });
        let ext = match ext_from_mime(base_type) {
            "" => {
                if media.kind == "photo" {
                    ".jpg"
                } else {
                    ".mp4"
                }
            }
            e => e,
        };
        let disposition = format!("attachment; filename=\"openshares-{slug}{ext}\"");
        match HeaderValue::from_str(&disposition) {
            Ok(value) => {
                response
                    .headers_mut()
                    .insert(header::CONTENT_DISPOSITION, value);
            }
            Err(e) => {
                tracing::warn!(error = %e, slug = %slug, "invalid Content-Disposition — skipped");
            }
        }
    }
    Ok(response)
}

/// Load a media row only when a session exists — mirrors the TS
/// `user ? await loadMedia(id) : null`. Both handlers then collapse
/// `!media || !user || !can(...)` into one `403 "Not allowed."` (no
/// 401, no existence leak on missing media).
async fn load_for_user(state: &AppState, user: &MaybeUser, slug: &str) -> Result<Option<Media>> {
    match user.0 {
        Some(_) => Ok(media::get_by_slug(&state.db, slug).await?),
        None => Ok(None),
    }
}

/// `(caption ?? "").trim().slice(0, 1000) || null` — `Option<Value>`
/// handles every JSON shape; a non-string `caption` mirrors the TS
/// `(5).trim()` TypeError → 500.
fn caption_from_body(body: &Map<String, Value>) -> Result<Option<String>> {
    let caption: String = match body.get("caption") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.trim().chars().take(1000).collect(),
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal server error.",
            ));
        }
    };
    Ok(if caption.is_empty() {
        None
    } else {
        Some(caption)
    })
}

/// `PATCH /api/media/{slug} { caption }` — `media:edit` scoped to the
/// item's owner, then a signed body. Response `{ok, caption}` (null
/// when cleared). Audit: `media.caption.edit`.
async fn patch_media(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    maybe_user: MaybeUser,
    req: Request,
) -> Result<Response> {
    let media = load_for_user(&state, &maybe_user, &slug).await?;
    let (Some(user), Some(media)) = (maybe_user.0, media) else {
        return Err(ApiError::forbidden("Not allowed."));
    };
    if !user.can("media:edit", media.owner_user_id) {
        return Err(ApiError::forbidden("Not allowed."));
    }

    let body: Map<String, Value> = json_body(req, Some(&user.sid)).await?;
    let caption = caption_from_body(&body)?;
    media::update_caption(&state.db, media.id, caption.as_deref()).await?;
    audit(
        &state.cache,
        Some(user.id),
        "media.caption.edit",
        Some("media"),
        Some(&media.slug),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true, "caption": caption })).into_response())
}

/// `DELETE /api/media/{slug}` — `media:delete` scoped to the owner, a
/// signed body (content unused — `verifySignedBody` only), then a hard
/// delete (`media::delete_by_id`); the file itself stays in Telegram.
/// Audit: `media.delete`.
async fn delete_media(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    maybe_user: MaybeUser,
    req: Request,
) -> Result<Response> {
    let media = load_for_user(&state, &maybe_user, &slug).await?;
    let (Some(user), Some(media)) = (maybe_user.0, media) else {
        return Err(ApiError::forbidden("Not allowed."));
    };
    if !user.can("media:delete", media.owner_user_id) {
        return Err(ApiError::forbidden("Not allowed."));
    }

    raw_body(req, Some(&user.sid)).await?;
    media::delete_by_id(&state.db, media.id).await?;
    audit(
        &state.cache,
        Some(user.id),
        "media.delete",
        Some("media"),
        Some(&media.slug),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// `extFromMime` — download-filename extension lookup.
fn ext_from_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/webp" => ".webp",
        "image/gif" => ".gif",
        "image/heic" => ".heic",
        "video/mp4" => ".mp4",
        "video/quicktime" => ".mov",
        "video/webm" => ".webm",
        "video/x-matroska" => ".mkv",
        "video/x-msvideo" => ".avi",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ext_from_mime_map() {
        assert_eq!(ext_from_mime("image/jpeg"), ".jpg");
        assert_eq!(ext_from_mime("IMAGE/PNG"), ".png");
        assert_eq!(ext_from_mime("video/mp4"), ".mp4");
        assert_eq!(ext_from_mime("application/octet-stream"), "");
    }

    #[test]
    fn caption_normalization() {
        let empty = Map::new();
        assert_eq!(caption_from_body(&empty).unwrap(), None);
        let null = Map::from_iter([("caption".into(), Value::Null)]);
        assert_eq!(caption_from_body(&null).unwrap(), None);
        let s = Map::from_iter([("caption".into(), json!("  hi  "))]);
        assert_eq!(caption_from_body(&s).unwrap(), Some("hi".to_string()));
        let long = Map::from_iter([("caption".into(), json!("x".repeat(1500)))]);
        assert_eq!(
            caption_from_body(&long).unwrap().map(|c| c.len()),
            Some(1000)
        );
        // non-string → the TS `(5).trim()` TypeError → 500
        let num = Map::from_iter([("caption".into(), json!(5))]);
        assert_eq!(
            caption_from_body(&num).unwrap_err().status,
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
