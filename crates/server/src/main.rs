//! OpenShares API server — axum port of the Next.js `/api/*` routes and
//! the security pipeline in `src/middleware.ts`.
//!
//! `main` stays thin: build `AppState`, run migrations (visible in logs),
//! assemble the router — public/token routes merged under `/api` — and
//! wrap it in the `middleware.ts`-ordered gateway stack via
//! `middleware::apply`.

use axum::{Router, routing::get};
use openshares_server::{middleware, openapi, proxy, routes, seed, state::AppState};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let state = AppState::from_env().await?;
    state.db.migrate().await?;

    // Dev bootstrap (`DB=local ./dev.sh`) — hard-gated inside the seed.
    if std::env::var("DEV_SEED")
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
        && seed::ensure_dev_admin(&state.db).await?
    {
        tracing::info!(email = seed::DEV_EMAIL, "seeded dev admin (local db)");
    }

    let mut app = Router::new()
        .route("/healthz", get(healthz))
        .nest("/api", routes::public_router().merge(routes::api_router()))
        .fallback(proxy::fallback);

    // `SWAGGER_UI=1` — opt-in API explorer, gated by Basic-auth
    // `SWAGGER_AUTH=user:password` (fails closed: no creds → no mount).
    // Outside `/api` so the gateway's same-origin/CSRF/rate-limit
    // pipeline doesn't apply; security headers + CSP still cover it.
    if std::env::var("SWAGGER_UI").is_ok_and(|v| v.trim() == "1") {
        match middleware::docs_auth::expected_from_env() {
            Some(expected) => {
                tracing::info!("swagger ui enabled at /docs (basic auth)");
                // The axum integration registers `/docs` (redirect) +
                // `/docs/{*rest}` itself — pass the bare prefix.
                let docs = Router::<AppState>::new()
                    .merge(
                        SwaggerUi::new("/docs")
                            .url("/docs/openapi.json", openapi::ApiDoc::openapi()),
                    )
                    .layer(axum::middleware::from_fn_with_state(
                        expected,
                        middleware::docs_auth::require,
                    ));
                app = app.merge(docs);
            }
            None => {
                tracing::warn!("SWAGGER_UI=1 but SWAGGER_AUTH unset/malformed — docs not mounted");
            }
        }
    }
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
