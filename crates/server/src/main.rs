//! OpenShares API server — axum port of the Next.js `/api/*` routes and
//! the security pipeline in `src/middleware.ts`.
//!
//! Phase 5 scope: `AppState`, router assembly mirroring `src/app/api/**`,
//! and tower middleware layers stacked in the SAME ORDER as middleware.ts:
//! correlation-id → security headers → same-origin → rate-limit → CSRF →
//! routes. The bot webhook (`/api/bot/webhook`) calls
//! `core::bot::process_update`, exactly like the TS route does.

use axum::{Router, routing::get};
use openshares_core::config::Config;
use openshares_core::db::Db;
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let db = Db::connect(&config.database_url).await?;
    db.migrate().await?;

    // TODO(phase-5): build AppState { db, config, tg client } and nest the
    // /api router with the middleware stack described above.
    let app = Router::new().route("/healthz", get(healthz));

    let addr: SocketAddr = format!("0.0.0.0:{}", config.port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "openshares-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received, draining");
}
