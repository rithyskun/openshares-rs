//! `GET /api/admin/vault` — the Vault tab's DB index stats, ported
//! from the `admin/page.tsx` server-component query block (which used
//! to read the DB directly). `admin:access`-gated like Activity.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use openshares_core::db::repos::{albums, media, shares, users};

use crate::error::Result;
use crate::extract::AuthUser;
use crate::routes::admin::guard;
use crate::state::AppState;

/// `LIMIT 5` on the top-owners query (admin/page.tsx).
const TOP_OWNERS: i64 = 5;

/// Route table — merged by `admin::router`.
pub fn router() -> Router<AppState> {
    Router::new().route("/admin/vault", get(vault))
}

/// `VaultStats` — `{media:{count,bytes}, byKind, byAvailability,
/// albums:{count,publicCount}, shares:{count,activeLinks},
/// users:{count,active}, topOwners:[{name,n,bytes}]}`.
async fn vault(State(state): State<AppState>, AuthUser(user): AuthUser) -> Result<Response> {
    guard(&state, &user, "admin:access").await?;

    let (m, kinds, avail, a, s, u, owners) = (
        media::totals(&state.db).await?,
        media::counts_by_kind(&state.db).await?,
        media::counts_by_availability(&state.db).await?,
        albums::totals(&state.db).await?,
        shares::totals(&state.db).await?,
        users::totals(&state.db).await?,
        media::top_owners(&state.db, TOP_OWNERS).await?,
    );

    let by_kind: Map<String, Value> = kinds.into_iter().map(|(k, n)| (k, json!(n))).collect();
    let by_availability: Map<String, Value> =
        avail.into_iter().map(|(k, n)| (k, json!(n))).collect();

    Ok(Json(json!({
        "media": { "count": m.n, "bytes": m.bytes },
        "byKind": by_kind,
        "byAvailability": by_availability,
        "albums": { "count": a.n, "publicCount": a.public_n },
        "shares": { "count": s.n, "activeLinks": s.active_links },
        "users": { "count": u.n, "active": u.active_n },
        "topOwners": owners
            .into_iter()
            .map(|(name, n, bytes)| json!({ "name": name, "n": n, "bytes": bytes }))
            .collect::<Vec<_>>(),
    }))
    .into_response())
}
