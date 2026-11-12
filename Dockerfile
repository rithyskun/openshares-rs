# syntax=docker/dockerfile:1
#
# Single-source image: the Rust API (openshares-server) serves /api/* +
# /healthz on :8080 and proxies every other request to a Next.js
# standalone server on loopback (WEB_ORIGIN). One container, two
# processes — deploy/entrypoint.sh supervises them.

# ── Web build stage ──────────────────────────────────────────────────
FROM node:24-bookworm-slim AS web
WORKDIR /web
ENV NEXT_TELEMETRY_DISABLED=1

COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ .
# output: "standalone" → .next/standalone/server.js (bundled node_modules)
# + .next/static copied separately in the runtime stage.
RUN npm run build

# ── Rust build stage ─────────────────────────────────────────────────
FROM rust:1-bookworm AS builder
WORKDIR /app

# native-tls -> OpenSSL on Linux: needs pkg-config + libssl-dev to build.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN cargo build --release --workspace \
    && cp target/release/openshares-server /usr/local/bin/openshares-server \
    && cp target/release/openshares-bot /usr/local/bin/openshares-bot

# ── Runtime stage ────────────────────────────────────────────────────
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
# media cache + audit shards live under /app/data — mount a volume here
# in production (`-v openshares-data:/app/data`). Both supervised
# processes run as the unprivileged `openshares` user.
RUN useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin openshares \
    && mkdir -p /app/data \
    && chown -R openshares:openshares /app/data

COPY --from=builder /usr/local/bin/openshares-server /usr/local/bin/openshares-server
COPY --from=builder /usr/local/bin/openshares-bot /usr/local/bin/openshares-bot

# Next standalone: the node binary (dynamically linked — libstdc++6 above),
# the self-contained server.js bundle, and static assets it doesn't include.
COPY --from=web /usr/local/bin/node /usr/local/bin/node
COPY --from=web /web/.next/standalone /web/
COPY --from=web /web/.next/static /web/.next/static

COPY deploy/entrypoint.sh /usr/local/bin/openshares-entrypoint
RUN chmod +x /usr/local/bin/openshares-entrypoint

# axum proxies non-/api traffic to Next on loopback; SSR fetches the
# loopback API directly (web/.env.example — API_INTERNAL_URL).
ENV WEB_ORIGIN=http://127.0.0.1:3000 \
    API_INTERNAL_URL=http://127.0.0.1:8080

EXPOSE 8080
USER openshares
CMD ["openshares-entrypoint"]
