//! `GET /api/admin/permissions` — port of
//! `src/app/api/admin/permissions/route.ts`: the code permission
//! catalog (`PERMISSIONS` in rbac.ts), grouped by `resource` (the part
//! before the first `:`) in catalog order — the TS `Map` keeps
//! insertion order, so resources appear in first-seen sequence.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use openshares_core::rbac::PERMISSIONS;

use crate::error::Result;
use crate::extract::AuthUser;
use crate::openapi::{ErrorBody, PermissionsResponse};
use crate::state::AppState;

use super::guard;

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/admin/permissions", get(get_permissions))
}

/// `{permissions: [{resource, keys}]}` — the RBAC catalog grouped by
/// resource in first-seen (catalog) order.
#[utoipa::path(
    get,
    path = "/api/admin/permissions",
    tag = "admin",
    responses(
        (status = 200, description = "Permission catalog grouped by resource", body = PermissionsResponse),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `admin:access`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn get_permissions(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Response> {
    guard(&state, &user.0, "admin:access").await?;

    // In-order grouping — resources appear in first-seen catalog order.
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for key in PERMISSIONS {
        let resource = key.split(':').next().unwrap_or(*key);
        match groups.iter_mut().find(|(r, _)| r == resource) {
            Some((_, keys)) => keys.push((*key).to_string()),
            None => groups.push((resource.to_string(), vec![(*key).to_string()])),
        }
    }
    let permissions: Vec<_> = groups
        .into_iter()
        .map(|(resource, keys)| json!({ "resource": resource, "keys": keys }))
        .collect();
    Ok(Json(json!({ "permissions": permissions })).into_response())
}
