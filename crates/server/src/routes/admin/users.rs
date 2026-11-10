//! `/api/admin/users/**` — port of
//! `src/app/api/admin/users/route.ts`,
//! `src/app/api/admin/users/[id]/route.ts` and
//! `src/app/api/admin/users/[id]/roles/route.ts`: the user list with
//! role names, account creation, profile edits / suspension, and
//! user_roles assign/remove — `user:manage` for mutations,
//! `admin:access` for the list, plus the rbac.ts last-admin invariant.
//!
//! PUT on `/admin/users/{id}/roles` is accepted as an alias for the TS
//! POST (role assign); DELETE `?role=` removes, exactly like the TS
//! route the console calls.

use std::collections::HashMap;

use axum::extract::{Path, RawQuery, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use openshares_core::audit::audit;
use openshares_core::crypto::password::{SessionUser, hash_password_async};
use openshares_core::crypto::security::{Limiter, rate_limited};
use openshares_core::db::Db;
use openshares_core::db::repos::{roles as roles_repo, sessions, users as users_repo};
use openshares_core::error::Result as CoreResult;

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::state::AppState;

use super::{guard, js_id, js_string, query_param, too_many_requests, valid_email};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/users", get(list_users).post(create_user))
        .route("/admin/users/{id}", patch(patch_user))
        .route(
            "/admin/users/{id}/roles",
            post(assign_role).put(assign_role).delete(remove_role),
        )
}

/// `violatesLastAdmin(userId, isAdminUser(userId))` — the pending change
/// strips admin power iff the target currently holds `*`; it violates
/// the invariant when they're the last active `*` holder.
async fn violates_last_admin(db: &Db, user_id: i64) -> CoreResult<bool> {
    let admins = roles_repo::admin_user_ids(db, None).await?;
    Ok(admins.contains(&user_id) && admins.len() <= 1)
}

/// GET — `{ users: [...roles: string[]], roles: [{id, name, builtin}] }`.
/// The roles list is `ORDER BY name` here (the roles route uses
/// `builtin DESC, name`).
async fn list_users(State(state): State<AppState>, user: AuthUser) -> Result<Response> {
    guard(&state, &user.0, "admin:access").await?;

    let all = users_repo::list_all(&state.db).await?;
    let role_rows = users_repo::all_user_role_names(&state.db).await?;
    let mut role_list = roles_repo::list(&state.db).await?;
    role_list.sort_by(|a, b| a.name.cmp(&b.name));

    let mut roles_by_user: HashMap<i64, Vec<String>> = HashMap::new();
    for r in role_rows {
        roles_by_user.entry(r.user_id).or_default().push(r.name);
    }

    let users_json: Vec<_> = all
        .iter()
        .map(|u| {
            json!({
                "id": u.id,
                "email": u.email,
                "name": u.name,
                "telegram_id": u.telegram_id,
                "disabled_at": u.disabled_at,
                "created_at": u.created_at,
                "roles": roles_by_user.get(&u.id).cloned().unwrap_or_default(),
            })
        })
        .collect();
    let roles_json: Vec<_> = role_list
        .iter()
        .map(|r| json!({ "id": r.id, "name": r.name, "builtin": r.builtin }))
        .collect();
    Ok(Json(json!({ "users": users_json, "roles": roles_json })).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct NewUserBody {
    name: Option<String>,
    email: Option<String>,
    password: Option<String>,
    roles: Option<Value>,
}

/// POST `{ name, email?, password?, roles? }` — create an account.
/// `API_LIMITER`-rated like the TS route (`admin-users:<actor>`).
async fn create_user(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "user:manage").await?;
    if rate_limited(
        &state.db,
        Limiter::Api,
        &format!("admin-users:{}", user.0.id),
    )
    .await
    {
        return Ok(too_many_requests());
    }
    let body = json_body::<NewUserBody>(req, Some(user.0.sid.as_str())).await?;

    // `(body.name ?? "").trim().slice(0, 200)` — JS slice is UTF-16
    // units; chars() matches on the BMP, which is all a name should hold.
    let name: String = body
        .name
        .unwrap_or_default()
        .trim()
        .chars()
        .take(200)
        .collect();
    // `(body.email ?? "").trim().toLowerCase() || null`
    let email = {
        let e = body.email.unwrap_or_default().trim().to_lowercase();
        if e.is_empty() { None } else { Some(e) }
    };
    // `typeof body.password === "string" ? body.password : ""` — a
    // non-string deserializes to None the same way.
    let password = body.password.unwrap_or_default();

    if name.is_empty() {
        return Err(ApiError::bad_request("Name is required."));
    }
    if let Some(e) = &email {
        if !valid_email(e) {
            return Err(ApiError::bad_request("Invalid email address."));
        }
    }
    if !password.is_empty() && email.is_none() {
        return Err(ApiError::bad_request(
            "A password needs an email to sign in with.",
        ));
    }
    if !password.is_empty() && password.chars().count() < 8 {
        return Err(ApiError::bad_request(
            "Password must be at least 8 characters.",
        ));
    }
    if let Some(e) = &email {
        if users_repo::email_taken(&state.db, e, None).await? {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "That email is already in use.",
            ));
        }
    }

    let password_hash = if password.is_empty() {
        None
    } else {
        Some(hash_password_async(password).await?)
    };
    let user_id = users_repo::create(
        &state.db,
        &users_repo::NewUser {
            email: email.as_deref(),
            name: &name,
            password_hash: password_hash.as_deref(),
        },
    )
    .await?;

    // `Array.isArray(body.roles)` → up to 20 names, sliced to 100 chars,
    // resolved by name, `INSERT OR IGNORE`.
    if let Some(names) = body.roles.as_ref().and_then(|v| v.as_array()) {
        for raw in names.iter().take(20) {
            let role_name: String = js_string(raw).chars().take(100).collect();
            if let Some(role) = roles_repo::get_by_name(&state.db, &role_name).await? {
                users_repo::assign_role(&state.db, user_id, role.id).await?;
            }
        }
    }

    let detail = match &email {
        Some(e) => format!("{name} <{e}>"),
        None => name.clone(),
    };
    audit(
        &state.cache,
        Some(user.0.id),
        "user.create",
        Some("user"),
        Some(&user_id.to_string()),
        Some(&detail),
    )
    .await;
    Ok(Json(json!({ "ok": true, "id": user_id })).into_response())
}

/// PATCH `/admin/users/{id}` — `{ disabled: boolean }` suspends or
/// unsuspends (killing sessions); `{ name?, email?, password? }` edits
/// the profile (a password reset also kills sessions). Profile fields
/// win when both shapes are present, matching the TS `!== undefined`
/// key-presence checks.
async fn patch_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "user:manage").await?;
    // TS verifies the body signature before the `Number.isInteger` check.
    let body = json_body::<Map<String, Value>>(req, Some(user.0.sid.as_str())).await?;
    let Some(target_id) = js_id(&id) else {
        return Err(ApiError::bad_request("Bad request."));
    };
    if users_repo::get_by_id(&state.db, target_id).await?.is_none() {
        return Err(ApiError::not_found("User not found."));
    }

    // ── Profile edit — `name|email|password !== undefined` ──────────
    if body.contains_key("name") || body.contains_key("email") || body.contains_key("password") {
        return patch_user_profile(&state, &user.0, target_id, &body).await;
    }

    // ── Suspend / unsuspend ──────────────────────────────────────────
    let Some(disabled) = body.get("disabled").and_then(|v| v.as_bool()) else {
        return Err(ApiError::bad_request("Bad request."));
    };
    if target_id == user.0.id && disabled {
        return Err(ApiError::bad_request("You can't suspend your own account."));
    }
    if disabled && violates_last_admin(&state.db, target_id).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "That would leave the vault without an admin.",
        ));
    }
    if disabled {
        users_repo::disable(&state.db, target_id).await?;
        // Kill live sessions immediately — the disabled_at guard already
        // blocks the next request; this just cleans up.
        sessions::delete_all_for_user(&state.db, target_id).await?;
    } else {
        users_repo::enable(&state.db, target_id).await?;
    }
    let action = if disabled {
        "user.suspend"
    } else {
        "user.unsuspend"
    };
    audit(
        &state.cache,
        Some(user.0.id),
        action,
        Some("user"),
        Some(&target_id.to_string()),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// The `{ name?, email?, password? }` half of PATCH — `changed` feeds
/// the `user.update` audit detail (`changed.join(",")`).
async fn patch_user_profile(
    state: &AppState,
    actor: &SessionUser,
    target_id: i64,
    body: &Map<String, Value>,
) -> Result<Response> {
    let mut changed: Vec<&'static str> = Vec::new();

    if let Some(v) = body.get("name") {
        let Some(raw) = v.as_str() else {
            // TS crashes on `body.name.trim()` for non-strings; 400 is
            // the same refusal, minus the stack trace.
            return Err(ApiError::bad_request("Bad request."));
        };
        let name: String = raw.trim().chars().take(200).collect();
        if name.is_empty() {
            return Err(ApiError::bad_request("Name can't be empty."));
        }
        users_repo::update_name(&state.db, target_id, &name).await?;
        changed.push("name");
    }

    if let Some(v) = body.get("email") {
        // `body.email === null ? null : trim/lowercase || null`
        let email: Option<String> = if v.is_null() {
            None
        } else {
            let Some(raw) = v.as_str() else {
                return Err(ApiError::bad_request("Bad request."));
            };
            let e = raw.trim().to_lowercase();
            if e.is_empty() { None } else { Some(e) }
        };
        if let Some(e) = &email {
            if !valid_email(e) {
                return Err(ApiError::bad_request("Invalid email address."));
            }
            if users_repo::email_taken(&state.db, e, Some(target_id)).await? {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "That email is already in use.",
                ));
            }
        }
        users_repo::update_email(&state.db, target_id, email.as_deref()).await?;
        changed.push("email");
    }

    if let Some(v) = body.get("password") {
        let Some(pw) = v.as_str() else {
            return Err(ApiError::bad_request("Bad request."));
        };
        // `body.password !== ""` — an empty string is a no-op, not a
        // reset.
        if !pw.is_empty() {
            if pw.chars().count() < 8 {
                return Err(ApiError::bad_request(
                    "Password must be at least 8 characters.",
                ));
            }
            let hash = hash_password_async(pw.to_string()).await?;
            users_repo::update_password_hash(&state.db, target_id, &hash).await?;
            // New credential → existing sessions must re-authenticate.
            sessions::delete_all_for_user(&state.db, target_id).await?;
            changed.push("password");
        }
    }

    if !changed.is_empty() {
        audit(
            &state.cache,
            Some(actor.id),
            "user.update",
            Some("user"),
            Some(&target_id.to_string()),
            Some(&changed.join(",")),
        )
        .await;
    }
    Ok(Json(json!({ "ok": true })).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct AssignBody {
    role: Option<String>,
}

/// POST (or PUT) `/admin/users/{id}/roles` `{ role }` —
/// `INSERT OR IGNORE INTO user_roles`.
async fn assign_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "user:manage").await?;
    let body = json_body::<AssignBody>(req, Some(user.0.sid.as_str())).await?;
    let role_name = body.role.unwrap_or_default().trim().to_string();
    let Some(target_id) = js_id(&id) else {
        return Err(ApiError::bad_request("Bad request."));
    };
    if role_name.is_empty() {
        return Err(ApiError::bad_request("Bad request."));
    }
    let Some(role) = roles_repo::get_by_name(&state.db, &role_name).await? else {
        return Err(ApiError::bad_request("Unknown role."));
    };
    if users_repo::get_by_id(&state.db, target_id).await?.is_none() {
        return Err(ApiError::not_found("User not found."));
    }
    users_repo::assign_role(&state.db, target_id, role.id).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "user.role.assign",
        Some("user"),
        Some(&target_id.to_string()),
        Some(&role_name),
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}

/// DELETE `/admin/users/{id}/roles?role=<name>` — remove a role,
/// last-admin guarded. Like the TS route, the target user isn't
/// loaded — removing a role from a nonexistent id is a harmless no-op
/// after the invariant check.
async fn remove_role(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "user:manage").await?;
    raw_body(req, Some(user.0.sid.as_str())).await?;

    let role_name = query_param(raw.as_deref(), "role")
        .unwrap_or_default()
        .trim()
        .to_string();
    let Some(target_id) = js_id(&id) else {
        return Err(ApiError::bad_request("Bad request."));
    };
    if role_name.is_empty() {
        return Err(ApiError::bad_request("Bad request."));
    }
    let Some(role) = roles_repo::get_by_name(&state.db, &role_name).await? else {
        return Err(ApiError::bad_request("Unknown role."));
    };

    // Removing a '*' role could strip admin power — enforce the invariant.
    if violates_last_admin(&state.db, target_id).await? {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "That would leave the vault without an admin.",
        ));
    }
    users_repo::remove_role(&state.db, target_id, role.id).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "user.role.remove",
        Some("user"),
        Some(&target_id.to_string()),
        Some(&role_name),
    )
    .await;
    Ok(Json(json!({ "ok": true })).into_response())
}
