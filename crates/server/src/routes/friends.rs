//! Session-gated friend endpoints — ports of
//! `src/app/api/friends/route.ts` (`GET|POST /api/friends`) and
//! `src/app/api/friends/[id]/route.ts` (`DELETE /api/friends/{id}`).
//!
//! `GET` is session-only (the TS route runs no `can()` check) and lists
//! the caller's `friend_links` with a display `name` assembled from
//! Telegram identity fields. `POST` is `apiGuard("friend:invite")` —
//! 403 `"Forbidden."` — then `API_LIMITER friends:{uid}`, a signed
//! `{action:"invite"}` body, and a one-time `link_codes` invite (7d)
//! deep-linked via `t.me/<bot>?start=<code>`. `DELETE` is
//! `apiGuard("friend:remove")` + signed body + `Bad id.`/`Not found.`
//! errors mirroring the TS route verbatim.

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use openshares_core::crypto::security::{Limiter, rate_limited};
use openshares_core::db::now;
use openshares_core::db::repos::friends::FriendRow;
use openshares_core::db::repos::{friends, link_codes};
use openshares_core::util::new_token;

use crate::body::{json_body, raw_body};
use crate::error::{ApiError, Result};
use crate::extract::AuthUser;
use crate::openapi::{ErrorBody, FriendsResponse, InviteBody, InviteResponse, OkBody};
use crate::routes::albums::{bot_username, too_many_requests};
use crate::state::AppState;

/// `newToken(6)` validity — `expires_at = now + 7 * 86400`.
const INVITE_TTL_SECS: i64 = 7 * 86400;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/friends", get(list_friends).post(create_invite))
        .route("/friends/{id}", delete(remove_friend))
}

/// `GET /api/friends` — the caller's friend links only
/// (`list_for_owner`), each row plus the display `name` the TS route
/// computes client-shape: `"first last" || username || telegram_id`.
#[utoipa::path(
    get,
    path = "/api/friends",
    tag = "friends",
    responses(
        (status = 200, description = "The caller's friend links", body = FriendsResponse),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn list_friends(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Response> {
    let rows = friends::list_for_owner(&state.db, user.0.id).await?;
    let friends = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "telegram_id": r.telegram_id,
                "username": r.username,
                "first_name": r.first_name,
                "last_name": r.last_name,
                "shared": r.shared,
                "name": friend_name(r),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "friends": friends })).into_response())
}

/// `[first, last].filter(Boolean).join(" ") || username || telegram_id`
/// — `filter(Boolean)` also drops empty strings; `||` falls through on
/// empty/absent values alike.
fn friend_name(r: &FriendRow) -> String {
    let joined = [
        r.first_name.as_deref().filter(|s| !s.is_empty()),
        r.last_name.as_deref().filter(|s| !s.is_empty()),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    if !joined.is_empty() {
        return joined;
    }
    r.username
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| r.telegram_id.clone())
}

/// `POST /api/friends { action: "invite" }` — mints a 6-byte link code
/// (`link_codes`, purpose `friend`, 7-day expiry) and returns the
/// `https://t.me/<bot>?start=<code>` deep link. No bot username → the
/// TS route's 503.
#[utoipa::path(
    post,
    path = "/api/friends",
    tag = "friends",
    request_body = InviteBody,
    responses(
        (status = 200, description = "One-time friend invite deep link", body = InviteResponse),
        (status = 400, description = "Unknown action", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `friend:invite`", body = ErrorBody),
        (status = 429, description = "Rate limited — `Retry-After: 60`", body = ErrorBody),
        (status = 503, description = "Bot is offline / BOT_TOKEN unset", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn create_invite(
    State(state): State<AppState>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    if !user.0.can("friend:invite", None) {
        return Err(ApiError::forbidden("Forbidden."));
    }
    if rate_limited(&state.db, Limiter::Api, &format!("friends:{uid}")).await {
        return Ok(too_many_requests());
    }

    let body: Map<String, Value> = json_body(req, Some(&user.0.sid)).await?;
    if body.get("action").and_then(|v| v.as_str()) != Some("invite") {
        return Err(ApiError::bad_request("Unknown action."));
    }
    let Some(username) = bot_username(&state).await else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Bot is offline — check BOT_TOKEN so invites can be generated.",
        ));
    };

    // `newToken(6).replace(/[-_]/g, "0")` — base64url chars, but the
    // t.me `start` param dislikes `-`/`_` so the TS route maps them to 0.
    let code = new_token(6).replace(['-', '_'], "0");
    link_codes::create(
        &state.db,
        &code,
        "friend",
        uid,
        Some(now() + INVITE_TTL_SECS),
    )
    .await?;
    Ok(
        Json(json!({ "code": code, "bot_link": format!("https://t.me/{username}?start={code}") }))
            .into_response(),
    )
}

/// `Number(id)` → `Number.isInteger` — JS `Number("")` is 0 (→ id 0 →
/// 404), `"0x10"` parses, trailing junk rejects. `n as i64` saturates
/// huge finite values → `remove` just finds no row → 404, same as TS.
fn parse_link_id(raw: &str) -> Option<i64> {
    let t = raw.trim();
    let n = if t.is_empty() {
        0.0
    } else if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok().map(|v| v as f64)?
    } else {
        t.parse::<f64>().ok()?
    };
    if n.is_finite() && n.fract() == 0.0 {
        Some(n as i64)
    } else {
        None
    }
}

/// `DELETE /api/friends/{id}` — `apiGuard("friend:remove")`, signed
/// body (content unused), id parse (`"Bad id."` 400), owner-scoped
/// delete (`"Not found."` 404 when the row isn't the caller's).
#[utoipa::path(
    delete,
    path = "/api/friends/{id}",
    tag = "friends",
    params(("id" = i64, Path, description = "`friend_links` row id")),
    responses(
        (status = 200, description = "Friend link removed", body = OkBody),
        (status = 400, description = "Bad id", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Missing `friend:remove`", body = ErrorBody),
        (status = 404, description = "Link not found / not the caller's", body = ErrorBody),
    ),
    security(("session_cookie" = []))
)]
pub(crate) async fn remove_friend(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: AuthUser,
    req: Request,
) -> Result<Response> {
    let uid = user.0.id;
    if !user.0.can("friend:remove", None) {
        return Err(ApiError::forbidden("Forbidden."));
    }
    raw_body(req, Some(&user.0.sid)).await?;

    let Some(link_id) = parse_link_id(&id) else {
        return Err(ApiError::bad_request("Bad id."));
    };
    if !friends::remove(&state.db, link_id, uid).await? {
        return Err(ApiError::not_found("Not found."));
    }
    Ok(Json(json!({ "ok": true })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        first_name: Option<&str>,
        last_name: Option<&str>,
        username: Option<&str>,
        telegram_id: &str,
    ) -> FriendRow {
        FriendRow {
            id: 1,
            telegram_id: telegram_id.to_string(),
            username: username.map(str::to_string),
            first_name: first_name.map(str::to_string),
            last_name: last_name.map(str::to_string),
            shared: 0,
        }
    }

    #[test]
    fn friend_name_fallbacks() {
        assert_eq!(
            friend_name(&row(Some("Ada"), Some("Lovelace"), Some("ada"), "42")),
            "Ada Lovelace"
        );
        assert_eq!(
            friend_name(&row(Some("Ada"), None, Some("ada"), "42")),
            "Ada"
        );
        // empty strings are `filter(Boolean)`-dropped like the TS join
        assert_eq!(
            friend_name(&row(Some(""), Some(""), Some("ada"), "42")),
            "ada"
        );
        assert_eq!(friend_name(&row(None, None, Some(""), "42")), "42");
        assert_eq!(friend_name(&row(None, None, None, "42")), "42");
    }

    #[test]
    fn link_id_number_semantics() {
        assert_eq!(parse_link_id("5"), Some(5));
        assert_eq!(parse_link_id(" 5 "), Some(5));
        assert_eq!(parse_link_id(""), Some(0)); // Number("") === 0 → delete misses → 404
        assert_eq!(parse_link_id("0x10"), Some(16));
        assert_eq!(parse_link_id("5.0"), Some(5));
        assert_eq!(parse_link_id("5.5"), None); // not an integer → Bad id.
        assert_eq!(parse_link_id("x"), None);
        assert_eq!(parse_link_id("Infinity"), None);
        assert_eq!(parse_link_id("99999999999999999999"), Some(i64::MAX)); // saturates → 404
    }
}
