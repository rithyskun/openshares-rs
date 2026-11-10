//! HTTP error mapping — the axum half of `unauthenticated()`,
//! `apiGuard()` 401/403, `tooManyRequests()` 429 and the route-level
//! `NextResponse.json({error}, {status})` convention in the TS codebase.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use openshares_core::error::AppError;

/// Error body the frontend expects: `{ "error": "<message>" }`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, msg)
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, msg)
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, msg)
    }

    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
}

impl From<AppError> for ApiError {
    fn from(e: AppError) -> Self {
        match e {
            AppError::Unauthorized => Self::unauthorized("Not signed in."),
            AppError::Forbidden => Self::forbidden("You do not have permission to do that."),
            AppError::NotFound => Self::not_found("Not found."),
            AppError::RateLimited => Self::new(StatusCode::TOO_MANY_REQUESTS, "Too many requests."),
            other => {
                // Never leak internals — log the detail, return a generic 500.
                tracing::error!(error = %other, "unhandled error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "Internal server error.")
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}
