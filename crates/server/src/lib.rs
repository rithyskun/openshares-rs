//! OpenShares API server library — the axum port of the Next.js `/api/*`
//! surface and `src/middleware.ts` security pipeline.
//!
//! Module map:
//! - `state`      — `AppState` (db, cache, config, tg, jwt) shared via `State`
//! - `error`      — `ApiError`/`ApiResult` — `AppError` → HTTP status mapping
//! - `extract`    — request extractors (`AuthUser` session guard, cookies)
//! - `media`      — `serve_media` Range streaming + tee-to-cache
//! - `middleware` — the gateway stack in `middleware.ts` order
//! - `openapi`    — `ApiDoc` + doc DTOs (mounted only when `SWAGGER_UI`)
//! - `routes`     — `/api/*` routers (public + authenticated)

// Workspace lints deny unwrap/expect in shipped code; tests may unwrap.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod body;
pub mod error;
pub mod extract;
pub mod media;
pub mod middleware;
pub mod openapi;
pub mod proxy;
pub mod routes;
pub mod state;
