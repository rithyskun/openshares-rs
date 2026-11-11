//! `GET /api/admin/activity` — port of
//! `src/app/api/admin/activity/route.ts`: the newest 100 audit entries
//! from the JSONL shards in the blob cache (`auditEntries(100)`).

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use openshares_core::audit::audit_entries;

use crate::error::Result;
use crate::extract::AuthUser;
use crate::openapi::{ActivityResponse, ErrorBody};
use crate::state::AppState;

use super::guard;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/admin/activity", get(get_activity))
}

/// `{activity: [...]}` — newest 100 audit entries across the monthly
/// JSONL shards.
#[utoipa::path(
    get,
    path = "/api/admin/activity",
    tag = "admin",
    responses(
        (status = 200, description = "Newest 100 audit entries", body = ActivityResponse),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `admin:access`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn get_activity(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Response> {
    guard(&state, &user.0, "admin:access").await?;
    let rows = audit_entries(&state.db, &state.cache, 100).await?;
    Ok(Json(json!({ "activity": rows })).into_response())
}
