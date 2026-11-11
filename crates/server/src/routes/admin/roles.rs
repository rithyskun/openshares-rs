//! `/api/admin/roles/**` — port of
//! `src/app/api/admin/roles/route.ts` and
//! `src/app/api/admin/roles/[id]/route.ts`: the roles list (permission
//! keys + member counts), custom-role create, rename /
//! permission-set replace, and delete — with the rbac.ts invariants
//! (`*` only on the built-in `admin` role; never orphan the vault
//! without an active `*` holder).

use std::collections::HashMap;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use openshares_core::audit::audit;
use openshares_core::db::repos::roles as roles_repo;
use openshares_core::rbac;

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::openapi::{ErrorBody, OkBody, PatchRoleBody, RoleCreated, RolesResponse};
use crate::state::AppState;

use super::{guard, js_id, js_string, valid_role_name};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/roles", get(list_roles).post(create_role))
        .route("/admin/roles/{id}", patch(patch_role).delete(delete_role))
}

/// GET — roles with permission keys + member counts, `ORDER BY builtin
/// DESC, name` (built-ins first). `{...r, permissions, memberCount}`
/// like the TS route — the role row itself is only `id/name/builtin`.
#[utoipa::path(
    get,
    path = "/api/admin/roles",
    tag = "admin",
    responses(
        (status = 200, description = "Roles + permission keys + member counts", body = RolesResponse),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `admin:access`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn list_roles(State(state): State<AppState>, user: AuthUser) -> Result<Response> {
    guard(&state, &user.0, "admin:access").await?;

    let role_rows = roles_repo::list(&state.db).await?;
    let perm_rows = roles_repo::all_permissions(&state.db).await?;
    let member_rows = roles_repo::member_counts(&state.db).await?;

    let mut perm_map: HashMap<i64, Vec<String>> = HashMap::new();
    for p in perm_rows {
        perm_map
            .entry(p.role_id)
            .or_default()
            .push(p.permission_key);
    }
    let member_map: HashMap<i64, i64> = member_rows.into_iter().map(|m| (m.role_id, m.n)).collect();

    let roles_json: Vec<_> = role_rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "builtin": r.builtin,
                "permissions": perm_map.get(&r.id).cloned().unwrap_or_default(),
                "memberCount": member_map.get(&r.id).copied().unwrap_or(0),
            })
        })
        .collect();
    Ok(Json(json!({ "roles": roles_json })).into_response())
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct CreateBody {
    /// `^[a-z][a-z0-9_-]{1,30}$`.
    name: Option<String>,
}

/// POST `{ name }` — create a custom role (`builtin = 0`).
#[utoipa::path(
    post,
    path = "/api/admin/roles",
    tag = "admin",
    request_body = CreateBody,
    responses(
        (status = 200, description = "Role created", body = RoleCreated),
        (status = 400, description = "Invalid role name", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `role:manage`", body = ErrorBody),
        (status = 409, description = "Name already exists", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn create_role(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "role:manage").await?;
    let body = json_body::<CreateBody>(req, Some(user.0.sid.as_str())).await?;
    let name = body.name.unwrap_or_default().trim().to_lowercase();
    if !valid_role_name(&name) {
        return Err(ApiError::bad_request(
            "Role name: 2–30 chars, lowercase letters/digits/-/_ starting with a letter.",
        ));
    }
    if roles_repo::name_taken(&state.db, &name, None).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "A role with that name already exists.",
        ));
    }
    let id = roles_repo::create(&state.db, &name).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "role.create",
        Some("role"),
        Some(&name),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true, "id": id, "name": name })).into_response())
}

/// PATCH `/admin/roles/{id}` — `{ name }` renames (custom roles only),
/// `{ permissions: string[] }` replaces the permission set. `*` is only
/// allowed on the built-in `admin` role, and removing `*` must not
/// leave the vault without an admin. Like the TS route, `role.rename`
/// is audited after `role.permissions.update`, and the permissions
/// audit names the role as loaded (pre-rename).
/// `PATCH /admin/roles/{id}` — `{ name }` renames (custom roles only),
/// `{ permissions: string[] }` replaces the permission set. `*` is only
/// allowed on the built-in `admin` role; removing it must not leave the
/// vault without an admin.
#[utoipa::path(
    patch,
    path = "/api/admin/roles/{id}",
    tag = "admin",
    params(("id" = i64, Path, description = "Role id")),
    request_body = PatchRoleBody,
    responses(
        (status = 200, description = "Role updated", body = OkBody),
        (status = 400, description = "Bad id / invalid name / unknown permission keys / `*` on a non-admin role", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `role:manage`", body = ErrorBody),
        (status = 404, description = "Role not found", body = ErrorBody),
        (status = 409, description = "Name taken / would orphan the vault without an admin", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn patch_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "role:manage").await?;
    let Some(role_id) = js_id(&id) else {
        return Err(ApiError::bad_request("Bad id."));
    };
    let Some(role) = roles_repo::get_by_id(&state.db, role_id).await? else {
        return Err(ApiError::not_found("Not found."));
    };
    let body = json_body::<Map<String, Value>>(req, Some(user.0.sid.as_str())).await?;

    if let Some(name_value) = body.get("name") {
        // `typeof body.name === "string"` — non-strings are ignored.
        if let Some(raw) = name_value.as_str() {
            if role.builtin != 0 {
                return Err(ApiError::bad_request("Built-in roles can't be renamed."));
            }
            let name = raw.trim().to_lowercase();
            if !valid_role_name(&name) {
                return Err(ApiError::bad_request("Invalid role name."));
            }
            if roles_repo::name_taken(&state.db, &name, Some(role_id)).await? {
                return Err(ApiError::new(StatusCode::CONFLICT, "Name taken."));
            }
            roles_repo::rename(&state.db, role_id, &name).await?;
        }
    }

    if let Some(perms_value) = body.get("permissions") {
        // `Array.isArray(body.permissions)` — non-arrays are ignored.
        if let Some(list) = perms_value.as_array() {
            // `[...new Set(...)]` — dedupe, first occurrence wins.
            let mut keys: Vec<String> = Vec::new();
            for item in list {
                let key = js_string(item).trim().to_string();
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            let bad: Vec<&str> = keys
                .iter()
                .filter(|k| !rbac::is_valid_permission_key(k))
                .map(String::as_str)
                .collect();
            if !bad.is_empty() {
                return Err(ApiError::bad_request(format!(
                    "Unknown permission keys: {}",
                    bad.join(", ")
                )));
            }
            if keys.iter().any(|k| k == "*") && !(role.builtin != 0 && role.name == "admin") {
                return Err(ApiError::bad_request(
                    "The * wildcard is reserved for the admin role.",
                ));
            }
            if !keys.iter().any(|k| k == "*")
                && roles_repo::has_wildcard(&state.db, role_id).await?
            {
                let remaining = roles_repo::admin_user_ids(&state.db, Some(role_id)).await?;
                if remaining.is_empty() {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "That would leave the vault without an admin.",
                    ));
                }
            }
            roles_repo::replace_permissions(&state.db, role_id, &keys).await?;
            audit(
                &state.cache,
                Some(user.0.id),
                "role.permissions.update",
                Some("role"),
                Some(&role.name),
                Some(&keys.join(",")),
            )
            .await;
        }
    }

    if let Some(name_value) = body.get("name")
        && let Some(raw) = name_value.as_str()
    {
        let name = raw.trim().to_lowercase();
        audit(
            &state.cache,
            Some(user.0.id),
            "role.rename",
            Some("role"),
            Some(&name),
            None,
        )
        .await;
    }

    Ok(Json(json!({ "ok": true })).into_response())
}

/// DELETE `/admin/roles/{id}` — custom roles only, last-admin guarded.
/// Signed empty body.
#[utoipa::path(
    delete,
    path = "/api/admin/roles/{id}",
    tag = "admin",
    params(("id" = i64, Path, description = "Role id")),
    responses(
        (status = 200, description = "Role deleted", body = OkBody),
        (status = 400, description = "Bad id / built-in role", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `role:manage`", body = ErrorBody),
        (status = 404, description = "Role not found", body = ErrorBody),
        (status = 409, description = "Would orphan the vault without an admin", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn delete_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "role:manage").await?;
    raw_body(req, Some(user.0.sid.as_str())).await?;

    let Some(role_id) = js_id(&id) else {
        return Err(ApiError::bad_request("Bad id."));
    };
    let Some(role) = roles_repo::get_by_id(&state.db, role_id).await? else {
        return Err(ApiError::not_found("Not found."));
    };
    if role.builtin != 0 {
        return Err(ApiError::bad_request("Built-in roles can't be deleted."));
    }
    if roles_repo::has_wildcard(&state.db, role_id).await? {
        let remaining = roles_repo::admin_user_ids(&state.db, Some(role_id)).await?;
        if remaining.is_empty() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "That would leave the vault without an admin.",
            ));
        }
    }
    roles_repo::delete_by_id(&state.db, role_id).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "role.delete",
        Some("role"),
        Some(&role.name),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}
