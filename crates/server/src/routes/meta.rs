//! `GET /api/meta` — instance metadata the SSR pages used to read
//! straight from `settings`/env: `{siteName, botUsername, appUrl}`.
//! Everything in it already renders on the public `/s`/`/a` pages, so
//! it's an unauthenticated public surface (the web layer caches it for
//! 60s, matching the old `siteName()` module cache).

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use openshares_core::settings::site_name;

use crate::error::Result;
use crate::routes::albums::bot_username;
use crate::state::AppState;

/// Route table — merged by `routes::public_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new().route("/meta", get(meta))
}

async fn meta(State(state): State<AppState>) -> Result<Response> {
    Ok(Json(json!({
        "siteName": site_name(&state.db).await?,
        "botUsername": bot_username(&state).await,
        "appUrl": state.config.app_url,
    }))
    .into_response())
}
