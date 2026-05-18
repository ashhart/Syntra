#!/usr/bin/env bash
# Try-instance entrypoint — runs Syntra in dev-mode so anonymous
# browser visitors can hit /admin without an auth token, then installs
# the five flagship capsules.
#
# Differences from Syntra/docker/demo/entrypoint.sh:
#   * uses `syntra serve --dev-mode` (no admin key required for callers)
#   * sets a synthetic LYCAN_ADMIN_KEY just for install.py to use during
#     the initial capsule install (Syntra accepts any Authorization
#     header in dev-mode)
#   * skips the dashboard traffic generator and console — the
#     try-instance image does not ship those; operators access /admin
#     directly via the Traefik front
#
# Per-IP rate-limiting is handled by Traefik in front (see
# docker-compose.yml `try-ratelimit` middleware). The Syntra binary's
# RateLimiter is global, not per-IP.

set -euo pipefail

export LYCAN_STORE_ROOT="${LYCAN_STORE_ROOT:-/store}"
export SYNTRA_URL="${SYNTRA_URL:-http://127.0.0.1:8787}"
export SYNTRA_DEMO_CAPSULE="${SYNTRA_DEMO_CAPSULE:-predictive-autoscaling}"

# CRITICAL: syntra reads LYCAN_ADMIN_KEY from env at startup. If it's
# set, dev-mode is bypassed and the server requires that key on every
# request. To run in dev-mode, we MUST unset it before invoking
# `syntra serve`. We then re-export it for install.py only.
unset LYCAN_ADMIN_KEY

mkdir -p "$LYCAN_STORE_ROOT"

echo "[try] store:         $LYCAN_STORE_ROOT"
echo "[try] demo capsule:  $SYNTRA_DEMO_CAPSULE"
echo "[try] mode:          dev-mode (no admin key required for callers)"

# Start the Syntra server in dev-mode. Binding 0.0.0.0 is required so
# the Traefik front in docker-compose.yml can reach us; Syntra will
# log a warning that dev-mode on a non-loopback address is unsafe,
# which is by design — Traefik is the auth boundary (per-IP rate
# limiting, optional Cloudflare in front).
syntra serve \
    --addr 0.0.0.0:8787 \
    --store "$LYCAN_STORE_ROOT" \
    --dev-mode &
SYNTRA_PID=$!
trap 'kill $SYNTRA_PID 2>/dev/null || true' EXIT

echo "[try] waiting for syntra..."
for _ in $(seq 1 30); do
    if curl -sf "$SYNTRA_URL/health" > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

# install.py reads LYCAN_ADMIN_KEY for its Authorization header. In
# dev-mode the binary ignores the value, so any string works.
LYCAN_ADMIN_KEY="try-instance-install" python3 /syntra/demo/capsule/install.py

echo "[try] ready — Syntra serving on 8787"

# Keep PID 1 alive on the server process.
wait $SYNTRA_PID
