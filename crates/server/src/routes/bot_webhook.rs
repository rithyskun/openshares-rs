//! `POST /api/bot/webhook` — port of `src/app/api/bot/webhook/route.ts`.
//!
//! Telegram webhook entrypoint (registered via `set-webhook`). Verifies
//! the `x-telegram-bot-api-secret-token` header against `WEBHOOK_SECRET`
//! — constant-time via [`secret_eq`], and **fail closed** when the env
//! var is unset (an unset secret rejects, it never silently accepts).
//! The secret is read per request like the TS `env()` helper, so
//! rotating it needs no restart.
//!
//! A verified update is processed by `tokio::spawn` — the axum
//! equivalent of the TS `ctx.waitUntil(processUpdate(update))`: the
//! response never waits on bot work so Telegram never times out.
//! Malformed JSON still answers `{ok: true}`, same as the TS catch.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::post};
use serde_json::json;

use openshares_core::bot::{BotContext, TgUpdate, process_update};
use openshares_core::crypto::security::secret_eq;

use crate::error::{ApiError, Result};
use crate::state::AppState;

/// Telegram's secret-token header — the value given to
/// `setWebhook(secret_token=…)` when the webhook was registered.
const SECRET_HEADER: &str = "x-telegram-bot-api-secret-token";

/// Route table — merged by `routes::public_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new().route("/bot/webhook", post(bot_webhook))
}

async fn bot_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    let given = headers
        .get(SECRET_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // Fail closed: an unset secret must reject, not silently accept.
    let valid = webhook_secret().is_some_and(|expected| secret_eq(given, &expected));
    if !valid {
        return Err(ApiError::unauthorized("Invalid secret token."));
    }

    let Ok(update) = serde_json::from_slice::<TgUpdate>(&body) else {
        return ok();
    };

    // Respond immediately; process in the background. `require_tg`
    // only fails when BOT_TOKEN is unset — a 500 there mirrors the TS
    // update dying inside `processUpdate` without a bot token.
    let ctx = BotContext {
        db: state.db.clone(),
        tg: state.require_tg()?.clone(),
        app_url: state.config.app_url.clone(),
        cache: state.cache.clone(),
    };
    let _task = tokio::spawn(async move { process_update(&ctx, update).await });
    ok()
}

fn ok() -> Result<Response> {
    Ok(Json(json!({ "ok": true })).into_response())
}

/// `env("WEBHOOK_SECRET")` — trimmed-or-undefined, read lazily like the
/// TS `env()` helper.
fn webhook_secret() -> Option<String> {
    std::env::var("WEBHOOK_SECRET")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
