//! Repositories — one module per table/aggregate, porting the
//! `db.prepare(...)` call sites in the TypeScript app:
//!
//! | module           | TS counterpart                                            |
//! |------------------|-----------------------------------------------------------|
//! | `users`          | `src/lib/auth.ts` lookups + `api/admin/users/**` + profile |
//! | `sessions`       | `src/lib/auth.ts` session rows                            |
//! | `media`          | `src/app/api/media/**` + admin vault stats                 |
//! | `albums`         | `src/app/api/albums/**` + `/a/[token]` + `/api/public/*`   |
//! | `shares`         | `api/media/[id]/share` + `/s/[token]` + share like route   |
//! | `friends`        | `src/app/api/friends/**`                                   |
//! | `telegram_users` | `api/profile/link` profile lookup                          |
//! | `link_codes`     | `api/friends` invite + `api/profile/link` code minting     |
//! | `rate_limits`    | `src/lib/security.ts` fixed-window counter                 |
//! | `roles`          | `src/app/api/admin/roles/**` + rbac.ts admin ops           |
//!
//! Functions take `&Db` and return `crate::error::Result`, matching
//! `crate::rbac` / `crate::settings`. Cross-cutting helpers that already
//! exist live where they always did: settings KV in `crate::settings`,
//! permission checks in `crate::rbac`, the audit trail in `crate::audit`,
//! and bot-flow queries inline in `crate::bot` (untouched by design).

pub mod albums;
pub mod friends;
pub mod link_codes;
pub mod media;
pub mod rate_limits;
pub mod roles;
pub mod sessions;
pub mod shares;
pub mod telegram_users;
pub mod users;

#[cfg(test)]
mod tests;
