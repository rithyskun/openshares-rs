//! `/api/admin/*` — port of `src/app/api/admin/**`.
//!
//! Every route resolves `AuthUser` then enforces the TS `apiGuard(key)`
//! permission — the extractor produces the 401, [`guard`] produces the
//! rbac.ts `forbidden()` 403. Mutations verify the HMAC body signature
//! through `crate::body::{json_body, raw_body}` (`verifySignedBody`),
//! including the bodyless DELETEs whose signature still covers `""`.
//!
//! | route                          | methods               | guard            |
//! |--------------------------------|-----------------------|------------------|
//! | `/admin/access`                | GET, POST, DELETE     | `settings:manage`|
//! | `/admin/activity`              | GET                   | `admin:access`   |
//! | `/admin/cache`                 | GET, PATCH, POST      | `settings:manage`|
//! | `/admin/permissions`           | GET                   | `admin:access`   |
//! | `/admin/roles`                 | GET, POST             | `admin:access`, `role:manage` |
//! | `/admin/roles/{id}`            | PATCH, DELETE         | `role:manage`    |
//! | `/admin/settings`              | GET, PATCH            | `settings:manage`|
//! | `/admin/users`                 | GET, POST             | `admin:access`, `user:manage`  |
//! | `/admin/users/{id}`            | PATCH                 | `user:manage`    |
//! | `/admin/users/{id}/roles`      | POST, PUT, DELETE     | `user:manage`    |
//! | `/admin/vault`                 | GET                   | `admin:access`   |
//!
mod access;
mod activity;
mod cache;
mod permissions;
mod roles;
mod settings;
mod users;
mod vault;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde_json::json;

use openshares_core::crypto::password::SessionUser;
use openshares_core::rbac;

use crate::error::{ApiError, Result};
use crate::state::AppState;

/// Route table — merged by `routes::api_router` under `/api`.
pub fn router() -> Router<AppState> {
    Router::new()
        .merge(access::router())
        .merge(activity::router())
        .merge(cache::router())
        .merge(permissions::router())
        .merge(roles::router())
        .merge(settings::router())
        .merge(users::router())
        .merge(vault::router())
}

/// `apiGuard(key)` minus the session half (AuthUser already 401s):
/// `can(userId, key)` or the rbac.ts `forbidden()` body
/// `{ error: "Forbidden." }` at 403.
async fn guard(state: &AppState, user: &SessionUser, key: &str) -> Result<()> {
    if rbac::can(&state.db, user.id, key, None).await? {
        Ok(())
    } else {
        Err(ApiError::forbidden("Forbidden."))
    }
}

/// `Number(params.id)` + `Number.isInteger` — JS `Number()` accepts
/// whitespace padding, decimal forms ("3.0") and exponents ("1e3"), all
/// of which pass the integer check when integral; everything else hits
/// the route's 400. (Hex/"0x…" strings are the one divergence — JS would
/// accept them.)
fn js_id(raw: &str) -> Option<i64> {
    let n = raw.trim().parse::<f64>().ok()?;
    if n.is_finite() && n.fract() == 0.0 {
        Some(n as i64)
    } else {
        None
    }
}

/// JS `String(v)` — the coercion `PATCH /admin/roles/[id]` applies to
/// `permissions[]` entries (`body.permissions.map(k => String(k))`).
/// Objects/arrays render as JSON text rather than "[object Object]" —
/// either way they fail `isValidPermissionKey` and land in the same 400.
fn js_string(v: &serde_json::Value) -> String {
    use serde_json::Value;
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// `req.nextUrl.searchParams.get(key)` — first `key` pair wins,
/// percent-decoded (`+` → space), bare `key` yields `""`.
fn query_param(raw: Option<&str>, key: &str) -> Option<String> {
    for pair in raw?.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name == key {
            return Some(percent_decode(value));
        }
    }
    None
}

/// `decodeURIComponent` for a query value — `+` → space, `%XX` → byte;
/// malformed escapes pass through verbatim like URLSearchParams.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else if b[i] == b'%' && i + 2 < b.len() {
            match (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h << 4 | l);
                    i += 3;
                }
                _ => {
                    out.push(b[i]);
                    i += 1;
                }
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// `EMAIL_RE` — `/^[^\s@]+@[^\s@]+\.[^\s@]+$/` from the admin users
/// routes: exactly one `@`, no whitespace, and a `.` in the domain with
/// a non-empty segment on each side (greedy backtracking means leading
/// dots and double dots are legal — `.a.b` is, `a.` isn't).
fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.contains('@') || email.chars().any(char::is_whitespace) {
        return false;
    }
    match domain.rfind('.') {
        Some(i) => i >= 1 && i < domain.len() - 1,
        None => false,
    }
}

/// `^[a-z][a-z0-9_-]{1,30}$` — role names, shared by create and rename.
fn valid_role_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && (1..=30).contains(&chars.clone().count())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `^\d{5,20}$` — Telegram user ids on the allowlist.
fn valid_telegram_id(id: &str) -> bool {
    (5..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

/// `tooManyRequests()` — 429 with `Retry-After: 60` and the
/// security.ts message (not `ApiError::RateLimited`'s generic text —
/// the admin users route calls this helper verbatim).
fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [("retry-after", "60")],
        Json(json!({ "error": "Too many requests — slow down and retry shortly." })),
    )
        .into_response()
}

/// `${(n / 1048576).toFixed(1)} MB` — `mb()` in cache-cleanup.ts.
fn mb(n: u64) -> String {
    format!("{:.1} MB", n as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_id_accepts_integer_forms() {
        assert_eq!(js_id("3"), Some(3));
        assert_eq!(js_id(" 3 "), Some(3));
        assert_eq!(js_id("3.0"), Some(3));
        assert_eq!(js_id("1e3"), Some(1000));
        assert_eq!(js_id("-2"), Some(-2));
        assert_eq!(js_id("abc"), None);
        assert_eq!(js_id("3.5"), None);
        assert_eq!(js_id(""), None);
        assert_eq!(js_id("Infinity"), None);
        assert_eq!(js_id("1e999"), None);
    }

    #[test]
    fn email_rule() {
        assert!(valid_email("a@b.co"));
        assert!(valid_email("a.b@x.y.z"));
        assert!(valid_email("a@.b.c")); // leading dot ok — `[^\s@]+\.` matches ".b."
        assert!(!valid_email("a@b"));
        assert!(!valid_email("@b.co"));
        assert!(!valid_email("a b@c.co"));
        assert!(!valid_email("a@@c.co"));
        assert!(!valid_email("a@b."));
    }

    #[test]
    fn role_name_rule() {
        assert!(valid_role_name("editor-2"));
        assert!(valid_role_name(&"x".repeat(31)));
        assert!(!valid_role_name("a")); // 2-char minimum
        assert!(!valid_role_name("2lead"));
        assert!(!valid_role_name("HasCaps"));
        assert!(!valid_role_name(&"x".repeat(32)));
        assert!(!valid_role_name(""));
    }

    #[test]
    fn telegram_id_rule() {
        assert!(valid_telegram_id("12345"));
        assert!(valid_telegram_id("12345678901234567890"));
        assert!(!valid_telegram_id("1234"));
        assert!(!valid_telegram_id("123456789012345678901"));
        assert!(!valid_telegram_id("123a5"));
    }

    #[test]
    fn query_param_first_decoded() {
        assert_eq!(query_param(Some("id=42&x=1"), "id").as_deref(), Some("42"));
        assert_eq!(query_param(Some("x=1&id=42"), "id").as_deref(), Some("42"));
        assert_eq!(query_param(Some("id"), "id").as_deref(), Some(""));
        assert_eq!(
            query_param(Some("role=a+b%21"), "role").as_deref(),
            Some("a b!")
        );
        assert_eq!(query_param(Some("x=%zz"), "x").as_deref(), Some("%zz"));
        assert_eq!(query_param(None, "id"), None);
        assert_eq!(query_param(Some("x=1"), "id"), None);
    }
}
