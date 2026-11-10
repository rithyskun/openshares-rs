//! Database row types — one `sqlx::FromRow` struct per table in
//! `crates/core/migrations/`, added alongside its repository.
//!
//! API request/response DTOs are defined in `server` routes; DB entities
//! never cross the HTTP boundary — the same DTO/entity split as the
//! TypeScript codebase.
