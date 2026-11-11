//! openshares-core — shared domain logic for the OpenShares Rust port.
//!
//! Mirrors `src/` of openshares-main (Next.js/TypeScript):
//!
//! | module      | TS counterpart                          |
//! |-------------|-----------------------------------------|
//! | `config`    | `src/lib/env.ts`                        |
//! | `error`     | error conventions across `src/lib/*.ts`  |
//! | `db`        | `src/db/index.ts` (Turso/libSQL → sqlx) |
//! | `models`    | row/DTO types                           |
//! | `crypto`    | `src/lib/{auth,jwt,envelope,signing,security}.ts` |
//! | `telegram`  | `src/lib/telegram.ts`                   |
//! | `media`     | `src/lib/storage.ts` + media streaming  |
//! | `bot`       | `src/bot/handler.ts`                    |
//! | `rbac`      | `src/lib/rbac.ts`                       |
//! | `audit`     | `src/lib/audit.ts`                      |
//! | `settings`  | `src/lib/settings.ts`                   |
//!
//! This crate intentionally has NO web-framework dependency — `server`
//! (axum) and `bot` (long-poll) are thin binaries over it.

// Workspace lints deny unwrap/expect in shipped code; tests may unwrap.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit;
pub mod bot;
pub mod config;
pub mod crypto;
pub mod db;
pub mod error;
pub mod media;
pub mod models;
pub mod rbac;
pub mod settings;
pub mod telegram;
pub mod util;
