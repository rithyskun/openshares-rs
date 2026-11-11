//! `GET /api/public/share/{token}` — the `/s/<token>` page's share
//! metadata (the `SHARE_SELECT` join the old page ran inline against
//! the DB). No session/RBAC — same public trust level as
//! `/api/media/by-token/{token}`.
//!
//! Missing tokens 404; an existing-but-expired share still returns 200
//! so the page can render its "Link expired" state (the client checks
//! `expiresAt` itself). Live links get the `shares.views + 1` the old
//! page did inline per render.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use openshares_core::db::now;
use openshares_core::db::repos::shares;

use crate::error::{ApiError, Result};
use crate::openapi::{ErrorBody, PublicShareDto};
use crate::state::AppState;

/// Route table — merged by `routes::public_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new().route("/public/share/{token}", get(public_share))
}

/// `SHARE_SELECT` camelized for the page — `{slug, kind, caption,
/// width, height, duration, fileSize, availability, ownerName,
/// createdAt, expiresAt, likes}`. Expired shares still 200 (the page
/// renders "Link expired"); live shares get a `views + 1`.
#[utoipa::path(
    get,
    path = "/api/public/share/{token}",
    tag = "public",
    params(
        ("token" = String, Path, description = "'link' share token (expired still returns 200)"),
    ),
    responses(
        (status = 200, description = "Share metadata", body = PublicShareDto),
        (status = 404, description = "Link not found", body = ErrorBody),
    ),
    security(())
)]
pub(crate) async fn public_share(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Response> {
    let Some(share) = shares::get_shared_media_by_token(&state.db, &token).await? else {
        return Err(ApiError::not_found("Link not found."));
    };
    // Live rows count a view; expired rows still 200 (the page shows
    // "Link expired" and skips the media endpoints itself).
    let live = share.expires_at.is_none_or(|exp| exp > now());
    if live {
        shares::increment_views(&state.db, &token).await?;
    }
    Ok(Json(json!({
        "slug": share.slug,
        "kind": share.kind,
        "caption": share.caption,
        "width": share.width,
        "height": share.height,
        "duration": share.duration,
        "fileSize": share.file_size,
        "availability": share.availability,
        "ownerName": share.owner_name,
        "createdAt": share.created_at,
        "expiresAt": share.expires_at,
        "likes": share.likes,
    }))
    .into_response())
}
