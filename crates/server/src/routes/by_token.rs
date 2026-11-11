//! `GET /api/media/by-token/{token}` — port of
//! `src/app/api/media/by-token/[token]/route.ts`.
//!
//! Resolves an unexpired `'link'` share token to its media slug, then
//! 302-redirects to the streaming route — which re-validates the token
//! (`/api/media/{slug}?token={token}`). Original query params
//! (`?download=1`, `?thumb=1`) are forwarded through the redirect.
//!
//! One deliberate difference from the TS code: the `Location` is a
//! relative URI rather than an absolute `new URL(origin + path)` —
//! axum handlers don't know their public origin (that lives in
//! `APP_URL`), and a relative redirect is equivalent for the browser.

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};

use openshares_core::db::repos::shares;

use crate::error::{ApiError, Result};
use crate::openapi::ErrorBody;
use crate::state::AppState;

/// Route table — merged by `routes::public_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new().route("/media/by-token/{token}", get(by_token))
}

/// 302 → `/api/media/{slug}?token=<token>` — original query params
/// (`download`, `thumb`) forwarded, `token` appended last.
#[utoipa::path(
    get,
    path = "/api/media/by-token/{token}",
    tag = "public",
    params(
        ("token" = String, Path, description = "Live 'link' share token"),
    ),
    responses(
        (status = 302, description = "Redirect to the streaming route (token re-validated there)", headers(("Location" = String, description = "/api/media/{slug}?…&token={token}"))),
        (status = 404, description = "Unknown or expired token", body = ErrorBody),
    ),
    security(())
)]
pub(crate) async fn by_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Response> {
    let slug = shares::media_slug_by_link_token(&state.db, &token)
        .await?
        .ok_or_else(|| ApiError::not_found("Not found."))?;

    let location = redirect_location(&slug, raw_query.as_deref(), &token);
    let location = HeaderValue::from_str(&location).map_err(|e| {
        tracing::warn!(error = %e, "redirect location not a valid header value");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.")
    })?;
    Ok((StatusCode::FOUND, [(header::LOCATION, location)]).into_response())
}

/// `/api/media/<slug>?<original query, token dropped>&token=<token>` —
/// the `url.search = req.nextUrl.search` + `url.searchParams.set("token",
/// token)` sequence in the TS route: every original pair is preserved
/// verbatim, `token` is replaced (never duplicated), appended last.
/// Tokens are `newToken` base64url — no percent-encoding needed.
fn redirect_location(slug: &str, raw_query: Option<&str>, token: &str) -> String {
    let mut location = format!("/api/media/{slug}?");
    if let Some(raw) = raw_query {
        for pair in raw.split('&').filter(|p| !p.is_empty()) {
            if pair.split('=').next() == Some("token") {
                continue;
            }
            location.push_str(pair);
            location.push('&');
        }
    }
    location.push_str("token=");
    location.push_str(token);
    location
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_appends_token() {
        assert_eq!(
            redirect_location("abc", None, "tok-1"),
            "/api/media/abc?token=tok-1"
        );
        assert_eq!(
            redirect_location("abc", Some(""), "tok-1"),
            "/api/media/abc?token=tok-1"
        );
    }

    #[test]
    fn redirect_preserves_params() {
        assert_eq!(
            redirect_location("abc", Some("download=1&thumb=1"), "t"),
            "/api/media/abc?download=1&thumb=1&token=t"
        );
        // bare flags keep their shape
        assert_eq!(
            redirect_location("abc", Some("download"), "t"),
            "/api/media/abc?download&token=t"
        );
    }

    #[test]
    fn redirect_replaces_existing_token() {
        assert_eq!(
            redirect_location("abc", Some("token=old&download=1"), "new"),
            "/api/media/abc?download=1&token=new"
        );
        // a bare `token` pair is dropped too
        assert_eq!(
            redirect_location("abc", Some("token"), "new"),
            "/api/media/abc?token=new"
        );
    }
}
