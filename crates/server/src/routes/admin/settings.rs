//! `GET|PATCH /api/admin/settings` — port of
//! `src/app/api/admin/settings/route.ts`. The TS route ships only the
//! PATCH (`{ siteName }` rename via `setSiteName`); the GET returns
//! `{ siteName }` — the value the admin Access tab edits — under the
//! same `settings:manage` guard.

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use openshares_core::audit::audit;
use openshares_core::settings::{set_site_name, site_name};

use crate::body::json_body;
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::openapi::{ErrorBody, SettingsPatched, SettingsResponse};
use crate::state::AppState;

use super::guard;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/admin/settings", get(get_settings).patch(patch_settings))
}

/// GET — `{ siteName }` (the current display name).
#[utoipa::path(
    get,
    path = "/api/admin/settings",
    tag = "admin",
    responses(
        (status = 200, description = "Current site name", body = SettingsResponse),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn get_settings(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    let name = site_name(&state.db).await?;
    Ok(Json(json!({ "siteName": name })).into_response())
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct SettingsBody {
    #[serde(rename = "siteName")]
    site_name: Option<String>,
}

/// PATCH `{ siteName }` — rename the whole app.
#[utoipa::path(
    patch,
    path = "/api/admin/settings",
    tag = "admin",
    request_body = SettingsBody,
    responses(
        (status = 200, description = "Site renamed", body = SettingsPatched),
        (status = 400, description = "Missing/non-string siteName", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn patch_settings(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    let body = json_body::<SettingsBody>(req, Some(user.0.sid.as_str())).await?;
    // `typeof body.siteName !== "string"` → 400 (non-strings deserialize
    // into None the same way a missing key does).
    let Some(name) = body.site_name else {
        return Err(ApiError::bad_request("Missing siteName."));
    };
    let site_name = set_site_name(&state.db, &name).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "settings.site_name.update",
        Some("settings"),
        Some("site_name"),
        Some(&site_name),
    )
    .await;
    Ok(Json(json!({ "ok": true, "siteName": site_name })).into_response())
}
