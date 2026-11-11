//! `GET|POST|DELETE /api/admin/access` — port of
//! `src/app/api/admin/access/route.ts`: the DB-managed Telegram
//! allowlist (`allowedTelegramIds`/`setAllowedTelegramIds` in
//! settings.ts — DB row wins, seeded from `ALLOWED_TELEGRAM_IDS` on
//! first read). All three methods are `settings:manage`-gated.

use std::collections::BTreeSet;

use axum::extract::{RawQuery, Request, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use openshares_core::audit::audit;
use openshares_core::db::Db;
use openshares_core::error::Result as CoreResult;
use openshares_core::settings::{get_setting, set_setting};

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::openapi::{AdminAccessList, AdminAccessMut, ErrorBody};
use crate::state::AppState;

use super::{guard, query_param, valid_telegram_id};

/// `ALLOWLIST_KEY` in settings.ts.
const ALLOWLIST_KEY: &str = "allowed_telegram_ids";

pub(super) fn router() -> Router<AppState> {
    Router::new().route(
        "/admin/access",
        get(get_access).post(add_access).delete(remove_access),
    )
}

/// `parseAllowlist` — comma-separated, trimmed, non-empty. A `BTreeSet`
/// also gives the `[...ids].sort()` output order for free (digits sort
/// the same byte-wise as UTF-16).
fn parse_allowlist(value: &str) -> BTreeSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// `allowedTelegramIds` — the stored row; on first read the row is
/// seeded from the `ALLOWED_TELEGRAM_IDS` env var (env.ts `env()`:
/// trimmed-or-undefined) and persisted, so later edits never re-seed.
async fn allowed_telegram_ids(db: &Db) -> CoreResult<BTreeSet<String>> {
    if let Some(stored) = get_setting(db, ALLOWLIST_KEY).await? {
        return Ok(parse_allowlist(&stored));
    }
    let seeded = std::env::var("ALLOWED_TELEGRAM_IDS")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(|v| parse_allowlist(&v))
        .unwrap_or_default();
    set_allowed_telegram_ids(db, &seeded).await?;
    Ok(seeded)
}

/// `setAllowedTelegramIds` — `[...ids].join(",")` upsert.
async fn set_allowed_telegram_ids(db: &Db, ids: &BTreeSet<String>) -> CoreResult<()> {
    let joined = ids.iter().cloned().collect::<Vec<_>>().join(",");
    set_setting(db, ALLOWLIST_KEY, &joined).await
}

/// GET — `{ telegram_ids: [...ids].sort() }`. The DB row wins; on first
/// read it's seeded from `ALLOWED_TELEGRAM_IDS`.
#[utoipa::path(
    get,
    path = "/api/admin/access",
    tag = "admin",
    responses(
        (status = 200, description = "Sorted Telegram-id allowlist", body = AdminAccessList),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn get_access(State(state): State<AppState>, user: AuthUser) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    let ids = allowed_telegram_ids(&state.db).await?;
    Ok(Json(json!({ "telegram_ids": ids.iter().collect::<Vec<_>>() })).into_response())
}

#[derive(Debug, Default, Deserialize, utoipa::ToSchema)]
struct AddBody {
    /// 5–20 digits.
    telegram_id: Option<String>,
}

/// POST `{ telegram_id }` — allow a Telegram id to sign up.
#[utoipa::path(
    post,
    path = "/api/admin/access",
    tag = "admin",
    request_body = AddBody,
    responses(
        (status = 200, description = "Id added — full allowlist returned", body = AdminAccessMut),
        (status = 400, description = "telegram_id not 5–20 digits", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn add_access(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    let body = json_body::<AddBody>(req, Some(user.0.sid.as_str())).await?;
    let id = body.telegram_id.unwrap_or_default().trim().to_string();
    if !valid_telegram_id(&id) {
        return Err(ApiError::bad_request("Telegram id must be 5–20 digits."));
    }
    let mut ids = allowed_telegram_ids(&state.db).await?;
    ids.insert(id.clone());
    set_allowed_telegram_ids(&state.db, &ids).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "allowlist.add",
        Some("telegram_id"),
        Some(&id),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true, "telegram_ids": ids.iter().collect::<Vec<_>>() })).into_response())
}

/// DELETE `?id=<telegram_id>` — remove from the allowlist.
#[utoipa::path(
    delete,
    path = "/api/admin/access",
    tag = "admin",
    params(("id" = String, Query, description = "Telegram id to remove")),
    responses(
        (status = 200, description = "Id removed — full allowlist returned", body = AdminAccessMut),
        (status = 400, description = "Missing id", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `settings:manage`", body = ErrorBody),
        (status = 404, description = "Id not on the allowlist", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn remove_access(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
    req: Request,
) -> Result<Response> {
    guard(&state, &user.0, "settings:manage").await?;
    raw_body(req, Some(user.0.sid.as_str())).await?;

    let id = query_param(raw.as_deref(), "id")
        .unwrap_or_default()
        .trim()
        .to_string();
    if id.is_empty() {
        return Err(ApiError::bad_request("Missing id."));
    }
    let mut ids = allowed_telegram_ids(&state.db).await?;
    if !ids.contains(&id) {
        return Err(ApiError::not_found("Not on the allowlist."));
    }
    ids.remove(&id);
    set_allowed_telegram_ids(&state.db, &ids).await?;
    audit(
        &state.cache,
        Some(user.0.id),
        "allowlist.remove",
        Some("telegram_id"),
        Some(&id),
        None,
    )
    .await;
    Ok(Json(json!({ "ok": true, "telegram_ids": ids.iter().collect::<Vec<_>>() })).into_response())
}
