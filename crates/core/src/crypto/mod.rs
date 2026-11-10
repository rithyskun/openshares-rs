//! Cryptographic primitives — ports of `src/lib/{auth,jwt,envelope,signing,security}.ts`.
//!
//! Phase 6 scope (hardest, most security-critical — port last):
//!
//! - `password` — scrypt hash/verify (`scrypt` crate; node:crypto scrypt params)
//! - `jwt`      — RS256 session JWTs (`jsonwebtoken`)
//! - `envelope` — RSA-OAEP + AES-256-GCM credential envelope (`rsa`, `aes-gcm`)
//! - `signing`  — HMAC-SHA256 request signing (`hmac`, `sha2`)
//! - `security` — constant-time compare, client IP, rate-limit helpers
//!
//! CPU-bound ops (scrypt) must run via `tokio::task::spawn_blocking` —
//! never block the async runtime.
