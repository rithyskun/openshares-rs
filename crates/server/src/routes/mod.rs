//! `/api/*` routers — port of `src/app/api/**`.
//!
//! Two families, mirroring the TS split:
//! - [`public_router`] — token-authorized + s2s routes that never need a
//!   session: `/api/media/{slug}` GET, `/api/media/by-token/[token]`,
//!   `/api/public/*`, `/api/bot/webhook`.
//! - [`api_router`] — session-gated routes behind `AuthUser` +
//!   `rbac::can`, with HMAC-signed bodies via `crate::body::json_body`.

use axum::Router;

use crate::state::AppState;

pub mod admin;
pub mod albums;
pub mod auth;
pub mod bot_webhook;
pub mod by_token;
pub mod friends;
pub mod me;
pub mod media_api;
pub mod media_stream;
pub mod meta;
pub mod profile;
pub mod public_album;
pub mod public_share;

/// Public/token-authorized routes — no session required. Merged under
/// `/api` in main.
pub fn public_router() -> Router<AppState> {
    Router::new()
        .merge(media_stream::router())
        .merge(by_token::router())
        .merge(public_album::router())
        .merge(public_share::router())
        .merge(meta::router())
        .merge(bot_webhook::router())
}

/// Session-gated routes — every handler resolves `AuthUser` and checks
/// `rbac::can`. Merged under `/api` alongside `public_router`.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(me::router())
        .merge(profile::router())
        .merge(media_api::router())
        .merge(albums::router())
        .merge(friends::router())
        .merge(admin::router())
}
