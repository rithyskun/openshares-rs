#!/usr/bin/env bash
# Dev launcher — runs the Rust API (:8080) and the Next dev server (:3000)
# together, and tears both down on exit (Ctrl-C or either process dying).
#
#   ./dev.sh            → API :8080 + web :3000
#
# Both entry points work once running:
#   http://localhost:3000  → Next dev directly (rewrites /api/* → :8080)
#   http://localhost:8080  → the prod topology (axum proxies pages → :3000)
set -euo pipefail
cd "$(dirname "$0")"

# brew's rustup is keg-only — make cargo visible for this script.
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

# Prod-parity gateway: axum proxies non-/api page traffic to Next.
export WEB_ORIGIN="${WEB_ORIGIN:-http://127.0.0.1:3000}"

# Free the dev ports — a previous run's children can linger holding them.
# TERM first, escalate to KILL if the socket doesn't release. (Must
# filter -sTCP:LISTEN — a bare `lsof -i :PORT` also matches TIME_WAIT
# remnants and browser-side sockets, which would kill the wrong pid.)
port_pids() { lsof -ti ":$1" -sTCP:LISTEN 2>/dev/null; }
free_port() {
  local port="$1" pids
  pids=$(port_pids "$port" || true)
  [ -n "$pids" ] || return 0
  echo "▸ :$port held by pid $(echo "$pids" | tr '\n' ' ')— killing"
  echo "$pids" | xargs kill 2>/dev/null || true
  for _ in $(seq 1 10); do
    port_pids "$port" >/dev/null 2>&1 || return 0
    sleep 1
  done
  port_pids "$port" | xargs kill -9 2>/dev/null || true
  for _ in $(seq 1 5); do
    port_pids "$port" >/dev/null 2>&1 || return 0
    sleep 1
  done
  echo "✗ :$port did not free up" >&2
  exit 1
}
free_port 8080
free_port 3000

SERVER_PID=
WEB_PID=

cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "$WEB_PID" ] && kill "$WEB_PID" 2>/dev/null
  # `cargo run` and `npm run dev` both leave the real listeners as
  # grandchildren — free both ports regardless of process depth.
  pkill -f "target/debug/openshares-server" 2>/dev/null || true
  lsof -ti :3000 -sTCP:LISTEN 2>/dev/null | xargs kill 2>/dev/null || true
}
trap cleanup INT TERM EXIT

cargo run -p openshares-server &
SERVER_PID=$!

# Hold Next until the API answers — SSR hits while cargo is still
# compiling/binding would 500 with ECONNREFUSED (exactly the failure
# this script exists to prevent).
echo "▸ waiting for api :8080 ..."
for _ in $(seq 1 90); do
  jobs -pr | grep -qx "$SERVER_PID" || break
  curl -sf -o /dev/null http://127.0.0.1:8080/healthz && break
  sleep 1
done
if ! jobs -pr | grep -qx "$SERVER_PID"; then
  echo "✗ api exited before binding :8080" >&2
  exit 1
fi
if ! curl -sf -o /dev/null http://127.0.0.1:8080/healthz; then
  echo "✗ api did not answer :8080 within 90s" >&2
  exit 1
fi
echo "✓ api ready"

(cd web && npm run dev) &
WEB_PID=$!

# macOS bash is 3.2 — no `wait -n`. `jobs -r` lists running jobs only
# (finished children drop out even before reaping), so poll until either
# child exits, then fall through to the EXIT trap to stop the other.
while jobs -pr | grep -qx "$SERVER_PID" && jobs -pr | grep -qx "$WEB_PID"; do
  sleep 1
done
