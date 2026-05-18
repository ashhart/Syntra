#!/usr/bin/env bash
#
# run-demo.sh — local equivalent of the Docker demo image.
#
# Boots a syntra server in --dev-mode, installs the five flagship demo
# capsules, and serves the dashboard. The Docker entrypoint
# (Syntra/docker/demo/entrypoint.sh) does the same thing in-container;
# this script runs them against your locally-built release binaries so
# you can iterate without docker build.
#
# Requires: cargo build --release run from the monorepo root, python3
# with flask + requests installed (`pip install flask requests`).
#
# macOS / Linux only.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

SYNTRA_BIN="$ROOT/target/release/syntra"
LYCAN_BIN="$ROOT/Lycan/target/release/lycan"

if [[ ! -x "$SYNTRA_BIN" ]] || [[ ! -x "$LYCAN_BIN" ]]; then
    echo "error: release binaries not built." >&2
    echo "build them first:" >&2
    echo "  cd $ROOT      && cargo build --release  # builds syntra" >&2
    echo "  cd $ROOT/Lycan && cargo build --release  # builds lycan" >&2
    echo "" >&2
    echo "expected:" >&2
    echo "  $LYCAN_BIN" >&2
    echo "  $SYNTRA_BIN" >&2
    exit 1
fi

ADDR="${SYNTRA_ADDR:-127.0.0.1:8787}"
DASHBOARD_PORT="${DASHBOARD_PORT:-8080}"
STORE="$(mktemp -d "${TMPDIR:-/tmp}/syntra-demo-store.XXXXXX")"
KEY="${LYCAN_ADMIN_KEY:-dev-key-$$}"

cleanup() {
    local rc=$?
    [[ -n "${DASH_PID:-}"    ]] && kill "$DASH_PID"    2>/dev/null || true
    [[ -n "${TRAFFIC_PID:-}" ]] && kill "$TRAFFIC_PID" 2>/dev/null || true
    [[ -n "${SYN_PID:-}"     ]] && kill "$SYN_PID"     2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$STORE"
    exit $rc
}
trap cleanup EXIT INT TERM

echo "[run-demo] store:      $STORE"
echo "[run-demo] admin key:  $KEY"
echo "[run-demo] api addr:   $ADDR"
echo "[run-demo] dashboard:  http://localhost:$DASHBOARD_PORT (after install completes)"
echo ""

# Boot syntra.
"$SYNTRA_BIN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" &
SYN_PID=$!

# Wait for /health to respond before installing capsules.
echo "[run-demo] waiting for syntra to come up..."
for _ in $(seq 1 40); do
    if curl -sf "http://$ADDR/health" >/dev/null 2>&1; then
        echo "[run-demo] syntra ready"
        break
    fi
    sleep 0.25
done

# Install the five flagship capsules. install.py is the same one
# Dockerfile.demo runs at container start — pointed at
# Syntra/examples/ instead of /syntra/demo/capsules/.
echo "[run-demo] installing demo capsules..."
PATH="$ROOT/Lycan/target/release:$PATH" \
    SYNTRA_URL="http://$ADDR" \
    LYCAN_ADMIN_KEY="$KEY" \
    SYNTRA_CAPSULES_ROOT="$ROOT/examples" \
    python3 "$ROOT/docker/demo/capsule/install.py"

# Traffic generator — drives ~1 decide+feedback/sec against the
# capsule named by $SYNTRA_DEMO_CAPSULE (predictive-autoscaling by
# default). Switching capsules in the dashboard does not switch which
# one the generator drives — restart the script with a different
# SYNTRA_DEMO_CAPSULE if you want that.
SYNTRA_URL="http://$ADDR" \
    LYCAN_ADMIN_KEY="$KEY" \
    SYNTRA_DEMO_CAPSULE="${SYNTRA_DEMO_CAPSULE:-predictive-autoscaling}" \
    python3 "$ROOT/docker/demo/traffic/generate.py" &
TRAFFIC_PID=$!

# Boot the dashboard. Foreground so this script blocks until you Ctrl-C.
echo ""
echo "[run-demo] dashboard: http://localhost:$DASHBOARD_PORT"
echo "[run-demo] API:       http://$ADDR"
echo "[run-demo] driving:   ${SYNTRA_DEMO_CAPSULE:-predictive-autoscaling}"
echo "[run-demo] Ctrl-C to stop"
echo ""

SYNTRA_URL="http://$ADDR" \
    LYCAN_ADMIN_KEY="$KEY" \
    LYCAN_STORE_ROOT="$STORE" \
    DASHBOARD_PORT="$DASHBOARD_PORT" \
    DEMO_TENANT=demo DEMO_JOB=autoscale DEMO_CAPSULE=orders \
    python3 "$ROOT/docker/demo/dashboard/app.py" &
DASH_PID=$!

wait
