//! Public album + like surfaces — ports of:
//! - `src/app/api/public/album/route.ts`      → `GET /api/public/album`
//! - `src/app/api/public/album/like/route.ts` → `POST /api/public/album/like`
//! - `src/app/api/public/share/[token]/like/route.ts` →
//!   `POST /api/public/share/{token}/like`
//!
//! Token-gated public surfaces — same trust level as
//! `/api/media/{slug}?token=`: no session, no RBAC. The album gate is
//! `is_public = 1` + unexpired `public_expires_at` (lazy-revoked on
//! expiry, mirroring `/a/[token]`); the share gate is a live `'link'`
//! row. Likes dedupe one-per-visitor via the `oslk_*` HttpOnly cookie —
//! [`LikeKind`] / [`has_liked`] / [`like_cookie_header`] port
//! `src/lib/likes.ts` (the cookie is the only record; clearing it allows
//! another like).

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use serde::Deserialize;
use serde_json::json;

use openshares_core::db::now;
use openshares_core::db::repos::media::{
    DEFAULT_LIMIT, MAX_LIMIT, MediaCursor, MediaListQuery, MediaScope,
};
use openshares_core::db::repos::{albums, media, shares};

use crate::error::{ApiError, Result};
use crate::extract::cookie_value;
use crate::state::AppState;

/// `LIKE_COOKIE_TTL` — 1 year in seconds (likes.ts).
const LIKE_COOKIE_TTL: i64 = 365 * 24 * 60 * 60;

/// Route table — merged by `routes::public_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/public/album", get(public_album))
        .route("/public/album/like", axum::routing::post(like_album))
        .route(
            "/public/share/{token}/like",
            axum::routing::post(like_share),
        )
}

/* ------------------------- likes.ts port ------------------------- */

/// `LikeKind` — picks the `oslk_s_` / `oslk_a_` cookie prefix.
#[derive(Debug, Clone, Copy)]
enum LikeKind {
    Share,
    Album,
}

impl LikeKind {
    /// `likeCookieName` — `oslk_<s|a>_<token>`.
    fn cookie_name(self, token: &str) -> String {
        let prefix = match self {
            Self::Share => "s",
            Self::Album => "a",
        };
        format!("oslk_{prefix}_{token}")
    }
}

/// `hasLiked` — true when this browser already liked the token. The TS
/// `!!cookie.value` is falsy on an empty value, so a present-but-empty
/// cookie doesn't count.
fn has_liked(headers: &HeaderMap, kind: LikeKind, token: &str) -> bool {
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    cookie_value(cookie_header, &kind.cookie_name(token)).is_some_and(|v| !v.is_empty())
}

/// `likeCookieHeader` — the `Set-Cookie` marking this visitor as liked.
fn like_cookie_header(kind: LikeKind, token: &str, secure: bool) -> String {
    format!(
        "{}=1; Path=/; Max-Age={LIKE_COOKIE_TTL}; SameSite=Lax{}; HttpOnly",
        kind.cookie_name(token),
        if secure { "; Secure" } else { "" },
    )
}

/// `req.nextUrl.protocol === "https:"` — behind a reverse proxy the
/// scheme arrives via `x-forwarded-proto`; a direct TLS connection
/// would put it in the request URI.
fn request_is_https(headers: &HeaderMap, uri: &Uri) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("https"))
        .unwrap_or_else(|| uri.scheme_str() == Some("https"))
}

/// `{likes, liked: true}` + the dedup `Set-Cookie` on a fresh like.
fn liked_response(likes: i64, cookie: Option<String>) -> Response {
    let body = json!({ "likes": likes, "liked": true });
    let Some(cookie) = cookie else {
        return Json(body).into_response();
    };
    match HeaderValue::from_str(&cookie) {
        Ok(value) => ([(header::SET_COOKIE, value)], Json(body)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "invalid Set-Cookie value — like dedup skipped");
            Json(body).into_response()
        }
    }
}

/* ---------------------- GET /public/album ---------------------- */

#[derive(Debug, Deserialize)]
struct PublicAlbumQuery {
    token: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

/// `GET /api/public/album?token=<t>&cursor=<id>&limit=<n>` — page
/// through a live public album's media (`id ASC` keyset, like
/// `MediaScope::Album`). An expired share lazily revokes and 404s.
async fn public_album(
    State(state): State<AppState>,
    Query(query): Query<PublicAlbumQuery>,
) -> Result<Response> {
    let token = query.token.as_deref().unwrap_or_default().trim();
    if token.is_empty() {
        return Err(ApiError::bad_request("Missing token."));
    }

    let Some(album) = albums::get_public_by_token(&state.db, token).await? else {
        return Err(ApiError::not_found("Link expired."));
    };
    if !albums::public_link_live(album.is_public, album.public_expires_at, now()) {
        if album.is_public == 1 {
            // Lazy revoke — mirrors the /a/[token] page behavior.
            albums::revoke_public(&state.db, album.id).await?;
        }
        return Err(ApiError::not_found("Link expired."));
    }

    // TS: cursor = Number(param ?? "0"), applied when finite && > 0.
    let cursor = query
        .cursor
        .as_deref()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| MediaCursor::Album { id: n as i64 });
    // TS: limit = Number(param) → min(raw, 100) when finite && >= 1,
    // else the 60-row page default.
    let limit = query
        .limit
        .as_deref()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 1.0)
        .map(|n| (n as i64).min(MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);

    let page = media::list(
        &state.db,
        &MediaListQuery {
            scope: MediaScope::Album(album.id),
            cursor,
            limit,
            ..Default::default()
        },
    )
    .await?;

    // A cursor-less request is the `/a/<token>` page render — the old
    // page incremented `albums.views` and rendered the album head inline.
    // Paginated calls (cursor present) stay lean `{items, nextCursor}`.
    let head = if cursor.is_none() {
        albums::increment_views(&state.db, album.id).await?;
        Some(json!({
            "title": album.title,
            "ownerName": album.owner_name,
            "likes": album.likes,
            "publicExpiresAt": album.public_expires_at,
            "itemCount": media::count_in_album(&state.db, album.id).await?,
            "coverSlug": media::first_live_slug_in_album(&state.db, album.id).await?,
        }))
    } else {
        None
    };

    Ok(Json(json!({
        "album": head,
        "items": page.items.iter().map(|m| json!({
            "slug": m.slug,
            "kind": m.kind,
            "caption": m.caption,
            "availability": m.availability,
        })).collect::<Vec<_>>(),
        "nextCursor": page.next_cursor,
    }))
    .into_response())
}

/* -------------------- POST /public/album/like -------------------- */

#[derive(Debug, Deserialize)]
struct AlbumLikeQuery {
    token: Option<String>,
}

/// `POST /api/public/album/like?token=<t>` — same live-album gate as the
/// GET; one like per visitor, deduped by the `oslk_a_<token>` cookie.
async fn like_album(
    State(state): State<AppState>,
    Query(query): Query<AlbumLikeQuery>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response> {
    let token = query.token.as_deref().unwrap_or_default().trim();
    if token.is_empty() {
        return Err(ApiError::bad_request("Missing token."));
    }

    let Some(album) = albums::get_public_by_token(&state.db, token).await? else {
        return Err(ApiError::not_found("Link expired."));
    };
    if !albums::public_link_live(album.is_public, album.public_expires_at, now()) {
        if album.is_public == 1 {
            // Lazy revoke — mirrors the /a/[token] page and the GET above.
            albums::revoke_public(&state.db, album.id).await?;
        }
        return Err(ApiError::not_found("Link expired."));
    }

    if has_liked(&headers, LikeKind::Album, token) {
        return Ok(liked_response(album.likes, None));
    }
    let likes = albums::increment_likes(&state.db, album.id)
        .await?
        .unwrap_or(album.likes + 1);
    let secure = request_is_https(&headers, &uri);
    Ok(liked_response(
        likes,
        Some(like_cookie_header(LikeKind::Album, token, secure)),
    ))
}

/* ----------------- POST /public/share/{token}/like ----------------- */

/// `POST /api/public/share/<token>/like` — live `'link'` shares only;
/// same per-visitor cookie dedup under `oslk_s_<token>`.
async fn like_share(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Response> {
    let Some((likes, expires_at)) = shares::get_link_like_state(&state.db, &token).await? else {
        return Err(ApiError::not_found("Link expired."));
    };
    if expires_at.is_some_and(|exp| exp <= now()) {
        return Err(ApiError::not_found("Link expired."));
    }

    if has_liked(&headers, LikeKind::Share, &token) {
        return Ok(liked_response(likes, None));
    }
    let likes = shares::increment_likes(&state.db, &token)
        .await?
        .unwrap_or(likes + 1);
    let secure = request_is_https(&headers, &uri);
    Ok(liked_response(
        likes,
        Some(like_cookie_header(LikeKind::Share, &token, secure)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_cookie_names() {
        assert_eq!(LikeKind::Album.cookie_name("tok"), "oslk_a_tok");
        assert_eq!(LikeKind::Share.cookie_name("tok"), "oslk_s_tok");
    }

    #[test]
    fn like_cookie_header_shape() {
        assert_eq!(
            like_cookie_header(LikeKind::Share, "tok", false),
            "oslk_s_tok=1; Path=/; Max-Age=31536000; SameSite=Lax; HttpOnly"
        );
        assert_eq!(
            like_cookie_header(LikeKind::Album, "tok", true),
            "oslk_a_tok=1; Path=/; Max-Age=31536000; SameSite=Lax; Secure; HttpOnly"
        );
    }

    #[test]
    fn has_liked_requires_nonempty_value() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("oslk_s_tok=1"));
        assert!(has_liked(&headers, LikeKind::Share, "tok"));

        // present-but-empty value doesn't count (TS `!!cookie.value`)
        headers.insert(header::COOKIE, HeaderValue::from_static("oslk_s_tok="));
        assert!(!has_liked(&headers, LikeKind::Share, "tok"));

        // different kind/token doesn't match
        assert!(!has_liked(&headers, LikeKind::Album, "tok"));
        assert!(!has_liked(&headers, LikeKind::Share, "other"));

        let empty = HeaderMap::new();
        assert!(!has_liked(&empty, LikeKind::Share, "tok"));
    }

    #[test]
    fn https_detection() {
        let uri: Uri = "https://example.com/api/public/album".parse().unwrap();
        assert!(request_is_https(&HeaderMap::new(), &uri));

        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let http_uri: Uri = "/api/public/album".parse().unwrap();
        assert!(request_is_https(&headers, &http_uri));

        headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
        assert!(!request_is_https(&headers, &uri));

        let no_headers = HeaderMap::new();
        assert!(!request_is_https(&no_headers, &http_uri));
    }
}
