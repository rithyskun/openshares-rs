#!/usr/bin/env bash
# Single-image supervisor — runs the two processes that make up the app:
#
#   node /web/server.js   Next standalone on 127.0.0.1:3000 (pages/SSR)
#   openshares-server     axum on :8080 — /api/* + /healthz, proxies the rest
#
# HOSTNAME/PORT are scoped to the node process only: axum also reads PORT
# (for its own :8080) and Docker already exports HOSTNAME=<container id>.
set -u

HOSTNAME=127.0.0.1 PORT=3000 node /web/server.js &
WEB_PID=$!

openshares-server &
API_PID=$!

shutdown() {
  kill -TERM "$WEB_PID" "$API_PID" 2>/dev/null || true
}
trap shutdown TERM INT

# First process to die takes the container with it — the orchestrator's
# restart policy brings the pair back up together.
wait -n "$WEB_PID" "$API_PID"
EXIT_CODE=$?
shutdown
wait "$WEB_PID" "$API_PID" 2>/dev/null
exit "$EXIT_CODE"
