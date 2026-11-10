//! OpenShares API server — axum port of the Next.js `/api/*` routes and
//! the security pipeline in `src/middleware.ts`.
//!
//! `main` stays thin: build `AppState`, run migrations (visible in logs),
//! assemble the router — public/token routes merged under `/api` — and
//! wrap it in the `middleware.ts`-ordered gateway stack via
//! `middleware::apply`.

use axum::{Router, routing::get};
use openshares_server::{middleware, proxy, routes, state::AppState};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let state = AppState::from_env().await?;
    state.db.migrate().await?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .nest("/api", routes::public_router().merge(routes::api_router()))
        .fallback(proxy::fallback);
    let app = middleware::apply(app, state.clone());
    let app = app.with_state(state.clone());

    let addr: SocketAddr = format!("0.0.0.0:{}", state.config.port).parse()?;
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
