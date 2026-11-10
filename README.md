# openshares-rs

Rust port of [OpenShares](../openshares-main) — the personal photo/video
vault where **Telegram is the media store**. This workspace mirrors the
TypeScript codebase module-for-module; each file's doc comment names its
TS counterpart.

## Layout

```
crates/
  core/     library — config, error, db (sqlx/SQLite), models, crypto,
            telegram (Bot API client), media (cache/streaming),
            bot (update processing), rbac, audit, settings
  server/   axum binary — the /api/* surface + security middleware
  bot/      long-poll binary — dev only, never alongside a webhook
migrations → crates/core/migrations (copied verbatim from the TS repo)
```

## Build & run

**Docker (no local toolchain needed — Linux rust:1-bookworm image):**

```bash
# one-time: create the cargo cache volumes
# (registry/git/target persist between runs — target stays OUT of the repo)

MSYS_NO_PATHCONV=1 docker run --rm \
  -v "//d/CPP/openshares/openshares-rs:/app" \
  -v openshares-cargo-registry:/usr/local/cargo/registry \
  -v openshares-cargo-git:/usr/local/cargo/git \
  -v openshares-target:/app/target \
  -w /app rust:1-bookworm bash -c \
  "apt-get update -qq && apt-get install -y -qq pkg-config libssl-dev >/dev/null && \
   cargo check --workspace --all-targets"

# fmt / clippy / test — same volumes, swap the cargo command:
#   cargo fmt --all -- --check
#   cargo clippy --workspace --all-targets -- -D warnings
#   cargo test --workspace

# run the server (migrations auto-apply, /healthz on :8080)
MSYS_NO_PATHCONV=1 docker run -d --name openshares-server -p 8080:8080 \
  -v "//d/CPP/openshares/openshares-rs:/app" \
  -v openshares-target:/app/target \
  -w /app rust:1-bookworm /app/target/debug/openshares-server
```

**Native (needs MSVC Build Tools or a GNU+MinGW toolchain):**

```bash
cp .env.example .env          # set BOT_TOKEN for the bot binary
cargo run -p openshares-server   # API on :8080 (migrations auto-apply)
cargo run -p openshares-bot      # long-poll bot (dev only — no webhook!)
```

Checks (same as CI): `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`

## Port phases

| Phase | Scope | TS source | Status |
|---|---|---|---|
| 1 | Scaffold + `core::db` + migrations | `src/db/index.ts` | ✅ done |
| 2 | `core::telegram` — tgCall/send_*/getFile/stream/upload + `FsCache` basics | `src/lib/telegram.ts`, `storage.ts` | ✅ done |
| 3 | `core::bot` + poll loop — all commands, albums, bot_state, callbacks | `src/bot/handler.ts` | ✅ done |
| 4 | `core::media` — fs cache, Range streaming, TTL sweep | `src/lib/storage.ts`, `cache-cleanup.ts` | ◐ core done |
| 5 | `server` — /api routes + middleware stack | `src/app/api/**`, `src/middleware.ts` | ⬜ not started |
| 6 | `core::crypto` — scrypt, RS256 JWT, RSA-OAEP+AES-GCM, HMAC | `src/lib/{auth,jwt,envelope,signing,security}.ts` | ⬜ not started |
| 7 | Frontend integration — Next.js UI → Rust API | `src/app/**`, `src/components/**` | ⬜ deferred |

### Phase 4 — remaining

- [x] `FsCache::head/put/read/open` — object + `.meta.json` sidecars
- [x] `begin_put`/`CacheWriter` — atomic `.part` → rename tee-to-cache writes
- [x] `sweep(max_age)` — TTL eviction, `audit/` prefix always skipped
- [ ] `serve_media` helper (server crate) — Range parse → `206`/`200`;
      cache miss → `open_telegram_file` stream tee'd through `CacheWriter`
- [ ] Media file routes (`/api/media/**/file`, thumb variants)

### Phase 5 — remaining (order matters)

1. `AppState { db, tg, cache, config }` + router assembly in `server`
2. **Public share routes first** — `/s/<token>`, `/a/<token>` media +
   album JSON. Token-authorized = zero session auth needed; exercises the
   whole streaming path end-to-end.
3. Authenticated `/api/*` routes — media list/caption/delete, albums,
   friends, shares, profile, admin. **Blocked on phase 6** for ownership
   checks (JWT session → `can()`).
4. Middleware stack in `middleware.ts` order: correlation-id → security
   headers → same-origin → rate-limit → CSRF → routes.

### Phase 6 — remaining

- [ ] scrypt password hashing (`spawn_blocking` — never block the runtime)
- [ ] Sessions table + RS256 JWT (`os_session` cookie, `sid` revocation)
- [ ] RSA-OAEP + AES-256-GCM login envelope (`{kid,k,i,d}`, ±5 min `ts`)
- [ ] HMAC-SHA256 request signing (`x-signature[-ts|-nonce]`, per-session key)
- [ ] `rate_limits` fixed-window counters, `clientIp`, `secretEq`

### Phase 7 — frontend decision (made: keep Next.js)

- Next.js stays the UI; it calls the Rust `/api/*` instead of querying
  Turso directly. Pages currently do SSR DB reads (`requirePerm`,
  `db.prepare`) — those become API calls or thin BFF routes during cutover.
- `apiFetch` contract must be honored: `x-csrf-token` on mutations,
  HMAC signing when a session key exists, `x-session-expired` 401s.

### Known gaps / caveats

- `core::bot` compiles + unit-tests but hasn't been smoke-tested against
  real Telegram payloads — first live `/start` may surface DTO tweaks.
- Rust DB layer is local SQLite (sqlx); Turso/libSQL swap is a future
  `Db` backend decision (schema is identical).
- `reqwest` has no IPv4-first DNS knob — the TS `telegram.ts` hack for
  broken-IPv6 networks has no direct equivalent (hosts-file workaround).

## Conventions

- DB entities (`sqlx::FromRow`) stay in `core`; API DTOs live in `server`
  routes — never leak rows to HTTP.
- `tracing` for all logging; `unwrap`/`expect` outside tests are lint-denied.
- CPU-bound crypto goes through `tokio::task::spawn_blocking`.
- Media responses stream (`Body::from_stream`) — never buffer whole files.
- One `reqwest::Client` + one `SqlitePool` in `AppState`, never per-request.

## Deploy

`Dockerfile` builds both binaries (multi-stage, ~100 MB debian-slim
runtime). Verified: image builds, server serves `/healthz`, migrations
auto-apply into the mounted `/app/data` volume.

```bash
docker build -t openshares-rs .
docker run -d -p 8080:8080 -v openshares-data:/app/data openshares-rs

# or compose (bot behind the "bot" profile — needs BOT_TOKEN in .env)
docker compose up -d
```

Host on Fly.io / Railway / a VPS / Cloud Run — NOT Vercel serverless
(no persistent bot poller, ephemeral fs, response limits).
