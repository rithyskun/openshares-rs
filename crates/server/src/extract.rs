//! Request extractors — the axum counterparts of the TS session helpers:
//! `getSessionUserByToken` behind `requireUser`/`apiGuard`, plus cookie
//! parsing for the `os_session` httpOnly JWT.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use openshares_core::crypto::password::{SessionUser, get_session_user_by_token};

use crate::error::{ApiError, Result};
use crate::state::AppState;

/// Session cookie name — `os_session` in the TS app.
pub const SESSION_COOKIE: &str = "os_session";

/// Pull a raw cookie value out of the `Cookie` header (no jar crate —
/// the TS app parses the header the same way).
pub fn cookie_value<'a>(cookie_header: &'a str, name: &str) -> Option<&'a str> {
    cookie_header
        .split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v)
}

/// `AuthUser` — extractor that resolves the `os_session` cookie into the
/// DB-anchored [`SessionUser`]. Missing/invalid/expired → 401, mirroring
/// `unauthenticated()` in `auth.ts`. Routes that need optional auth use
/// [`MaybeUser`].
pub struct AuthUser(pub SessionUser);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(Self(
            resolve_user(parts, state)
                .await?
                .ok_or_else(|| ApiError::unauthorized("Not signed in."))?,
        ))
    }
}

/// Optional-auth variant — `Ok(MaybeUser(None))` when there's no valid
/// session rather than a rejection.
pub struct MaybeUser(pub Option<SessionUser>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(Self(resolve_user(parts, state).await?))
    }
}

/// Extension marker — a session the gateway middleware already resolved
/// (it overlaps the lookup with the rate-limit round trip). Stored as
/// `EarlySession(Option<SessionUser>)`; `resolve_user` prefers it over
/// re-querying the DB.
#[derive(Clone)]
pub struct EarlySession(pub Option<SessionUser>);

async fn resolve_user(parts: &Parts, state: &AppState) -> Result<Option<SessionUser>> {
    if let Some(early) = parts.extensions.get::<EarlySession>() {
        return Ok(early.0.clone());
    }
    let cookie_header = parts
        .headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    resolve_cookie(state, cookie_header).await
}

/// Cookie-header → `SessionUser` resolution shared by the extractor and
/// the gateway's early resolve (which passes the request's header before
/// `next.run` consumes it).
pub async fn resolve_cookie(state: &AppState, cookie_header: &str) -> Result<Option<SessionUser>> {
    let Some(jwt) = state.jwt.as_ref() else {
        return Err(ApiError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Authentication is not configured.",
        ));
    };
    let Some(token) = cookie_value(cookie_header, SESSION_COOKIE) else {
        return Ok(None);
    };
    Ok(get_session_user_by_token(&state.db, jwt, token).await?)
}
