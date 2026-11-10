//! Single error type for the core crate.
//!
//! Maps to the HTTP semantics the TS codebase encodes in helpers like
//! `unauthenticated()` (401), `apiGuard()` (401/403) and `tooManyRequests()`
//! (429). `server` translates variants into status codes; `bot` just logs.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("telegram api error: {0}")]
    Telegram(String),

    #[error("http client error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("crypto error: {0}")]
    Crypto(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("not found")]
    NotFound,

    #[error("too many requests")]
    RateLimited,

    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, AppError>;
