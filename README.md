# openshares-rs

Rust port of [OpenShares](../openshares-main) — the personal photo/video
vault where **Telegram is the media store**. This workspace mirrors the
TypeScript codebase module-for-module; each file's doc comment names its
TS counterpart.

## Layout

```
crates/
  core/     library — config, error, db (libsql — Turso remote or
            local SQLite file), models, crypto,
            telegram (Bot API client), media (cache/streaming),
            bot (update processing), rbac, audit, settings
  server/   axum binary — the /api/* surface + security middleware
  bot/      long-poll binary — dev only, never alongside a webhook
migrations → crates/core/migrations (copied verbatim from the TS repo)
```

## Build & run

**Native (recommended):**

```bash
# one-time toolchain install (macOS) — rustup honors rust-toolchain.toml
brew install rustup
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"   # add to ~/.zshrc
rustup default stable

cp .env.example .env             # set BOT_TOKEN for the bot binary
./dev.sh                         # API :8080 + web :3000 together (Ctrl-C stops both)

# or individually:
cargo run -p openshares-server   # API on :8080 (migrations auto-apply)
(cd web && npm run dev)          # Next dev on :3000 (proxies /api/* → :8080)
cargo run -p openshares-bot      # long-poll bot (dev only — no webhook!)
```

Checks (same as CI): `cargo fmt --all -- --check` ·
`cargo clippy --workspace --all-targets -- -D warnings` · `cargo test --workspace`

**Docker (sandboxed check or Windows without a toolchain):**

```bash
docker run --rm -v "$PWD:/app" \
  -v openshares-cargo-registry:/usr/local/cargo/registry \
  -v openshares-cargo-git:/usr/local/cargo/git \
  -v openshares-target-docker:/app/target \
  -w /app rust:1-bookworm bash -c \
  "apt-get update -qq && apt-get install -y -qq pkg-config libssl-dev >/dev/null && \
   cargo check --workspace --all-targets"
# fmt / clippy / test — same volumes, swap the cargo command
# NOTE: keep the docker target dir separate (openshares-target-docker) —
# sharing ./target between macOS and Linux corrupts incremental artifacts.
```

## Port phases

| Phase | Scope | TS source | Status |
|---|---|---|---|
| 1 | Scaffold + `core::db` + migrations | `src/db/index.ts` | ✅ done |
| 2 | `core::telegram` — tgCall/send_*/getFile/stream/upload + `FsCache` basics | `src/lib/telegram.ts`, `storage.ts` | ✅ done |
| 3 | `core::bot` + poll loop — all commands, albums, bot_state, callbacks | `src/bot/handler.ts` | ✅ done |
| 4 | `core::media` + `server::media` — cache, `serve_media` Range streaming | `src/lib/storage.ts`, `cache-cleanup.ts` | ✅ done |
| 5 | `server` — /api routes + middleware stack | `src/app/api/**`, `src/middleware.ts` | ✅ done |
| 6 | `core::crypto` — scrypt, RS256 JWT, RSA-OAEP+AES-GCM, HMAC | `src/lib/{auth,jwt,envelope,signing,security}.ts` | ✅ done |
| 7 | Frontend — `web/` Next.js SSR → Rust API, single-source deploy | `src/app/**`, `src/components/**` | ◐ in progress |

### Phase 5 — shipped

- `AppState { db, tg, cache, config, jwt }` + router assembly; axum serves
  `/api/*` + `/healthz` and proxies page traffic to Next via `WEB_ORIGIN`.
- Public/token routes: `GET /api/media/{slug}` (Range 206/200, tee-to-cache,
  dead `file_id` → `missing` → 410), `by-token` redirect, `/api/public/album`
  (album head + views on first page), `/api/public/share/{token}` +
  like routes, `/api/bot/webhook` (fail-closed secret).
- Session routes: `auth/{login,logout,rsa-key}`, `me`, `profile/{password,link}`,
  media list/move/share (link mint + telegram delivery), albums CRUD +
  multipart upload, friends, full `/api/admin/*`.
- Middleware in `middleware.ts` order: correlation-id → security headers →
  same-origin → `MW_*` rate-limit → CSRF double-submit.
- SSR-cutover endpoints that have no TS counterpart: `GET /api/meta`
  (site name / bot username / app URL), `GET /api/media/summary`
  (`{count, bytes}`), `GET /api/admin/vault` (Vault tab stats),
  `/api/me` `sessionCreatedAt`.

### Phase 6 — shipped

- scrypt password hashing (`spawn_blocking` — never blocks the runtime)
- Sessions table + RS256 JWT (`os_session` cookie `SameSite=Lax`, `sid` revocation)
- RSA-OAEP + AES-256-GCM login envelope (`{kid,k,i,d}`, ±5 min `ts`,
  mandatory when keys configured — plaintext is rejected)
- HMAC-SHA256 request signing (`x-signature[-ts|-nonce]`, per-session key,
  `verify_signed_body` before JSON parse)
- `rate_limits` fixed-window counters, `client_ip`, `secret_eq`

### Phase 7 — frontend (in progress)

- `web/` holds the Next.js UI (pages + components, SSR kept — no `src/app/api`,
  no `@/db`): `src/lib/api-server.ts` calls `API_INTERNAL_URL` with forwarded
  cookies; client code still hits same-origin `/api/*` via `apiFetch`.
- `web/next.config.mjs` → `output: "standalone"`; axum proxies non-API
  traffic to the standalone server (`WEB_ORIGIN`, default off = API-only).
- Remaining: multi-stage Dockerfile (Rust bins + `web/.next/standalone` +
  entrypoint), docker-compose orchestration, live browser smoke test.

### Known gaps / caveats

- `core::bot` compiles + unit-tests but hasn't been smoke-tested against
  real Telegram payloads — first live `/start` may surface DTO tweaks.
- DB backend: `TURSO_DB_URL`+`TURSO_AUTH_TOKEN` → remote Turso (the same
  DB the TS app uses); unset → local SQLite file at `DATABASE_URL`. Migrations
  are tracked in `schema_migrations`, the same table `setup-db.ts` wrote.
- Media/audit cache backend: `MEDIA_CACHE_BACKEND=s3` (+ `R2_ACCOUNT_ID`,
  `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET`; `R2_ENDPOINT`
  optional override) → Cloudflare R2, the `MEDIA_CACHE` binding
  counterpart. Default `fs` → `MEDIA_CACHE_DIR` on disk for local dev and
  bot scripts. MIME rides on the object's `Content-Type` attribute (R2
  `httpMetadata.contentType`), no `.meta.json` sidecars in object storage;
  `S3Cache::head` reports size only (S3 HEAD can't surface attributes
  through `object_store`) — `open()` is the mime-bearing path.
- `reqwest` has no IPv4-first DNS knob — the TS `telegram.ts` hack for
  broken-IPv6 networks has no direct equivalent (hosts-file workaround).

## Conventions

- Sub-agent delegation policy (workstreams, write scopes, done-criteria) is
  in `AGENTS.md`.
- DB entities (`serde::Deserialize` rows via `Db::query_*`) stay in `core`;
  API DTOs live in `server` routes — never leak rows to HTTP.
- `tracing` for all logging; `unwrap`/`expect` outside tests are lint-denied.
- CPU-bound crypto goes through `tokio::task::spawn_blocking`.
- Media responses stream (`Body::from_stream`) — never buffer whole files
  at the HTTP layer (object-store `open` is an in-memory `Cursor`, bounded
  by the ~20 MB Bot-API cap).
- One `reqwest::Client` + one `Db` (Turso-pooled) + one `CacheBackend`
  in `AppState`, never per-request.

## Deploy

`Dockerfile` builds the **single image**: a `web/` Next.js standalone build
+ the Rust binaries + `deploy/entrypoint.sh` supervising both processes.
Axum owns `:8080` — `/api/*` + `/healthz` locally, everything else proxied
to Next on `127.0.0.1:3000` (`WEB_ORIGIN`). SSR fetches the loopback API
(`API_INTERNAL_URL`). Set `WEB_ORIGIN=` empty for API-only mode.

```bash
docker build -t openshares-rs .
docker run -d -p 8080:8080 -v openshares-data:/app/data openshares-rs

# or compose (bot behind the "bot" profile — needs BOT_TOKEN in .env)
docker compose up -d
```

Local dev: `cargo run -p openshares-server` + `cd web && npm run dev`
(set `WEB_ORIGIN=http://127.0.0.1:3000` in `.env` to proxy pages through
the API, or open the Next dev server directly on :3000).

Host on Fly.io / Railway / a VPS / Cloud Run — NOT Vercel serverless
(no persistent bot poller, ephemeral fs, response limits).
