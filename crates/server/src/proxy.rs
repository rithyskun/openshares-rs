//! Page-traffic reverse proxy — the single-image deploy's last hop.
//!
//! When `WEB_ORIGIN` is set, every request that matched no `/api/*`
//! route (or `/healthz`) is forwarded to the Next.js server verbatim —
//! method, headers, body — and the upstream status/headers/body stream
//! returned. Hop-by-hop headers are stripped both ways. `/api/*`
//! requests that match NO route still 404 locally (never proxied —
//! Next has no API surface anymore).

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::error::ApiError;
use crate::state::AppState;

/// Headers stripped hop-by-hop in both directions (RFC 2616 §13.5.1).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// `fallback` handler — proxy the request to `config.web_origin`.
/// API paths that match nothing never leave the gateway.
pub async fn fallback(
    axum::extract::State(state): axum::extract::State<AppState>,
    req: Request,
) -> Result<Response, ApiError> {
    let path = req.uri().path();
    if path.starts_with("/api/") || path == "/api" {
        return Err(ApiError::not_found("Not found."));
    }
    let Some(origin) = state.config.web_origin.as_deref() else {
        return Err(ApiError::not_found("Not found."));
    };

    let method = req.method().clone();
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let url = format!("{origin}{path_and_query}");

    let mut headers = HeaderMap::new();
    for (name, value) in req.headers() {
        if !HOP_BY_HOP.contains(&name.as_str()) {
            headers.insert(name.clone(), value.clone());
        }
    }
    // Next's SSR guards trust the forwarded pair for Secure-cookie and
    // origin decisions.
    if let Some(host) = req.headers().get(header::HOST) {
        headers.insert("x-forwarded-host", host.clone());
    }
    if !headers.contains_key("x-forwarded-proto") {
        headers.insert(
            "x-forwarded-proto",
            axum::http::HeaderValue::from_static("http"),
        );
    }

    let body_bytes = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .map_err(|e| ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, e.to_string()))?;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let upstream = client
        .request(method, &url)
        .headers(headers)
        .body(body_bytes)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, url, "web proxy upstream failed");
            ApiError::new(StatusCode::BAD_GATEWAY, "Frontend unavailable.")
        })?;

    let status = StatusCode::from_u16(upstream.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut out_headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if !HOP_BY_HOP.contains(&name.as_str()) {
            out_headers.insert(name.clone(), value.clone());
        }
    }
    let stream = upstream.bytes_stream();
    Ok((status, out_headers, Body::from_stream(stream)).into_response())
}
