#!/usr/bin/env bash
set -euo pipefail

# SYNTRA_DEV_MODE=1 (or a --dev-mode arg) makes syntra serve start without
# authentication. The chart's devMode toggle sets this env var.
SYNTRA_DEV_MODE="${SYNTRA_DEV_MODE:-0}"
for arg in "$@"; do
    if [[ "$arg" == "--dev-mode" ]]; then
        SYNTRA_DEV_MODE=1
    fi
done

# install.py and the dashboard read LYCAN_ADMIN_KEY at startup. In dev
# mode the server ignores auth, but the env var must still exist for
# those imports to succeed — sentinel value is fine.
if [[ "$SYNTRA_DEV_MODE" == "1" ]]; then
    export LYCAN_ADMIN_KEY="${LYCAN_ADMIN_KEY:-dev-mode-noauth}"
else
    export LYCAN_ADMIN_KEY="${LYCAN_ADMIN_KEY:-demo-key-$(date +%s)}"
fi

export LYCAN_STORE_ROOT="${LYCAN_STORE_ROOT:-/syntra/data}"
export SYNTRA_URL="${SYNTRA_URL:-http://127.0.0.1:8787}"

# Which capsule the traffic generator drives; others are still installed.
export SYNTRA_DEMO_CAPSULE="${SYNTRA_DEMO_CAPSULE:-predictive-autoscaling}"

# Boot-default dashboard path; overridden per browser tab via URL hash.
case "$SYNTRA_DEMO_CAPSULE" in
    predictive-autoscaling)
        export DEMO_TENANT=demo  DEMO_JOB=autoscale  DEMO_CAPSULE=orders ;;
    anomaly-routing)
        export DEMO_TENANT=demo  DEMO_JOB=routing    DEMO_CAPSULE=api ;;
    seasonal-fraud-threshold)
        export DEMO_TENANT=demo  DEMO_JOB=fraud      DEMO_CAPSULE=threshold ;;
    shared-state-action-embeddings)
        export DEMO_TENANT=demo  DEMO_JOB=embeddings DEMO_CAPSULE=router ;;
    hierarchical-region-routing)
        export DEMO_TENANT=demo  DEMO_JOB=region     DEMO_CAPSULE=router ;;
    *)
        echo "[demo] WARN: unknown SYNTRA_DEMO_CAPSULE=$SYNTRA_DEMO_CAPSULE, falling back to predictive-autoscaling" >&2
        export SYNTRA_DEMO_CAPSULE=predictive-autoscaling
        export DEMO_TENANT=demo  DEMO_JOB=autoscale  DEMO_CAPSULE=orders ;;
esac

mkdir -p "$LYCAN_STORE_ROOT"

if [[ "$SYNTRA_DEV_MODE" == "1" ]]; then
    echo "[demo] DEV MODE: routes unauthenticated"
else
    echo "[demo] admin key:   $LYCAN_ADMIN_KEY"
fi
echo "[demo] store:       $LYCAN_STORE_ROOT"
echo "[demo] demo capsule: $SYNTRA_DEMO_CAPSULE (dashboard default: $DEMO_TENANT/$DEMO_JOB/$DEMO_CAPSULE)"

# The syntra binary reads LYCAN_ADMIN_KEY from env *before* parsing
# flags. If both --dev-mode and a non-empty admin key are in scope,
# the admin key wins and dev mode is silently a no-op. So launch
# syntra with the env var stripped (`env -u`) in dev mode; the parent
# shell keeps the sentinel for the post-boot install/dashboard helpers.
if [[ "$SYNTRA_DEV_MODE" == "1" ]]; then
    env -u LYCAN_ADMIN_KEY syntra serve \
        --addr 0.0.0.0:8787 \
        --store "$LYCAN_STORE_ROOT" \
        --dev-mode &
else
    syntra serve \
        --addr 0.0.0.0:8787 \
        --store "$LYCAN_STORE_ROOT" \
        --admin-key "$LYCAN_ADMIN_KEY" &
fi
SYNTRA_PID=$!
trap "kill $SYNTRA_PID 2>/dev/null || true" EXIT

echo "[demo] waiting for syntra..."
for _ in $(seq 1 30); do
    if curl -sf "$SYNTRA_URL/health" > /dev/null 2>&1; then
        break
    fi
    sleep 1
done

python3 /syntra/demo/capsule/install.py

# SYNTRA_DEMO_NO_TRAFFIC=1 skips the background driver so the global RNG
# isn't consumed by interleaved /decide calls — required for reproducible
# benchmark runs against LYCAN_RNG_SEED.
if [[ "${SYNTRA_DEMO_NO_TRAFFIC:-0}" != "1" ]]; then
    python3 /syntra/demo/traffic/generate.py &
    TRAFFIC_PID=$!
    trap "kill $SYNTRA_PID $TRAFFIC_PID 2>/dev/null || true" EXIT
fi

echo "[demo] dashboard: http://localhost:8080"
exec python3 /syntra/demo/dashboard/app.py
