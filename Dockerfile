# syntax=docker/dockerfile:1

# ── Build stage ──────────────────────────────────────────────────────
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
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
# sqlite db + media cache + audit shards live under /app/data — mount a
# volume here in production (`-v openshares-data:/app/data`).
RUN mkdir -p /app/data

COPY --from=builder /usr/local/bin/openshares-server /usr/local/bin/openshares-server
COPY --from=builder /usr/local/bin/openshares-bot /usr/local/bin/openshares-bot

EXPOSE 8080
CMD ["openshares-server"]
