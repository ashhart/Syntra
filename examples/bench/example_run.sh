#!/usr/bin/env bash
# example_run.sh — start a fresh Syntra, run the bench, tear down.
#
# Prerequisites:
#   • Docker available and running
#   • `syntra` binary on PATH (for capsule authoring)
#   • Python 3.9+
#
# Usage:
#   bash example_run.sh [--concurrency N] [--duration-seconds S] [--ratio R]
#
# The script:
#   1. Pulls/starts a Syntra container (dev-mode, ephemeral).
#   2. Waits for /health to respond.
#   3. Runs bench.py with the provided arguments (plus sensible defaults).
#   4. Saves the JSON result to bench_result.json.
#   5. Stops and removes the container.
#
# Override any bench.py option by appending it after the script arguments,
# e.g.:
#   bash example_run.sh --concurrency 32 --duration-seconds 60 --ratio 3:1

set -euo pipefail

SYNTRA_URL="${SYNTRA_URL:-http://localhost:8787}"
SYNTRA_ADMIN_KEY="${SYNTRA_ADMIN_KEY:-dev-key}"
CONTAINER_NAME="syntra-bench-$$"
SYNTRA_IMAGE="${SYNTRA_IMAGE:-syntra:demo}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULT_FILE="${SCRIPT_DIR}/bench_result.json"

# Default bench parameters (can be overridden via env or extra args).
CONCURRENCY="${BENCH_CONCURRENCY:-8}"
DURATION="${BENCH_DURATION:-30}"
WARMUP="${BENCH_WARMUP:-5}"
RATIO="${BENCH_RATIO:-1:1}"
CONTEXT_TYPE="${BENCH_CONTEXT_TYPE:-discrete}"
TENANT="${BENCH_TENANT:-bench}"
JOB="${BENCH_JOB:-perf}"
CAPSULE="${BENCH_CAPSULE:-harness}"

# Parse simple overrides from argv.
while [[ $# -gt 0 ]]; do
    case "$1" in
        --concurrency)     CONCURRENCY="$2"; shift 2 ;;
        --duration-seconds) DURATION="$2"; shift 2 ;;
        --warmup-seconds)  WARMUP="$2"; shift 2 ;;
        --ratio)           RATIO="$2"; shift 2 ;;
        --context-type)    CONTEXT_TYPE="$2"; shift 2 ;;
        *)                 break ;;
    esac
done

# Remaining args are passed straight through to bench.py.
EXTRA_ARGS=("$@")

cleanup() {
    echo "[bench] stopping container ${CONTAINER_NAME} ..." >&2
    docker stop "${CONTAINER_NAME}" >/dev/null 2>&1 || true
    docker rm  "${CONTAINER_NAME}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

# -------------------------------------------------------------------------
# 1. Start Syntra container.
# -------------------------------------------------------------------------
echo "[bench] starting Syntra container (${SYNTRA_IMAGE}) ..." >&2
docker run -d \
    --name "${CONTAINER_NAME}" \
    -p 8787:8787 \
    -e SYNTRA_ADMIN_KEY="${SYNTRA_ADMIN_KEY}" \
    "${SYNTRA_IMAGE}" >/dev/null

# -------------------------------------------------------------------------
# 2. Wait for /health.
# -------------------------------------------------------------------------
echo "[bench] waiting for Syntra to become healthy ..." >&2
MAX_WAIT=60
WAITED=0
until curl -sf "${SYNTRA_URL}/health" >/dev/null 2>&1; do
    sleep 1
    WAITED=$((WAITED + 1))
    if [[ $WAITED -ge $MAX_WAIT ]]; then
        echo "ERROR: Syntra did not become healthy within ${MAX_WAIT}s" >&2
        exit 1
    fi
done
echo "[bench] Syntra is healthy." >&2

# -------------------------------------------------------------------------
# 3. Run the benchmark.
# -------------------------------------------------------------------------
echo "[bench] running benchmark ..." >&2
python3 "${SCRIPT_DIR}/bench.py" \
    --syntra-url "${SYNTRA_URL}" \
    --admin-key  "${SYNTRA_ADMIN_KEY}" \
    --tenant     "${TENANT}" \
    --job        "${JOB}" \
    --capsule    "${CAPSULE}" \
    --concurrency       "${CONCURRENCY}" \
    --duration-seconds  "${DURATION}" \
    --warmup-seconds    "${WARMUP}" \
    --ratio             "${RATIO}" \
    --context-type      "${CONTEXT_TYPE}" \
    "${EXTRA_ARGS[@]}" \
    | tee "${RESULT_FILE}"

echo "" >&2
echo "[bench] JSON result saved to ${RESULT_FILE}" >&2

# cleanup via trap
