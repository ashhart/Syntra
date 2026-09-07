#!/usr/bin/env bash
#
# reset.sh — daily wipe for the try.syntra.io shared demo.
#
# Stops the syntra container, deletes the named volume that holds all
# capsule state (the four flagship capsules' learned weights, decision
# log, refusal counters, etc.), recreates the empty volume, and brings
# the stack back up. The syntra entrypoint re-runs install.py on boot,
# which re-installs the four flagship capsules from the baked-in
# /syntra/demo/capsules/ directory.
#
# Designed for cron at 00:00 UTC daily:
#   0 0 * * * /opt/syntra-try/reset.sh
#
# Logs every run to /var/log/syntra-reset.log and pings
# ${RESET_WEBHOOK_URL} (if set) on completion.

set -euo pipefail

# Locate the compose project root (the directory this script lives in).
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)"
cd "$SCRIPT_DIR"

LOG_FILE="${RESET_LOG_FILE:-/var/log/syntra-reset.log}"
VOLUME_NAME="${SYNTRA_VOLUME_NAME:-syntra-store}"

# Source .env if present so RESET_WEBHOOK_URL etc. are available even
# when run from cron (cron starts with a minimal environment).
if [[ -f "${SCRIPT_DIR}/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "${SCRIPT_DIR}/.env"
    set +a
fi

log() {
    printf '%s [reset] %s\n' "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$*" >> "$LOG_FILE"
}

mkdir -p "$(dirname "$LOG_FILE")"
touch "$LOG_FILE"

log "begin"

# Stop the syntra service. We leave traefik running so the landing
# page stays reachable during the wipe (the API will 502 for the
# ~10 s window, which is fine for a daily wipe at 00:00 UTC).
log "stopping syntra container"
docker compose stop syntra >> "$LOG_FILE" 2>&1

# Remove the syntra-store volume. We have to remove the container
# first (stop is not enough to release the volume reference) before
# the rm will succeed, so down --no-deps the syntra service.
log "removing syntra container so volume can be recreated"
docker compose rm -f syntra >> "$LOG_FILE" 2>&1

log "wiping named volume ${VOLUME_NAME}"
docker volume rm "$VOLUME_NAME" >> "$LOG_FILE" 2>&1 || log "volume already absent"
docker volume create "$VOLUME_NAME" >> "$LOG_FILE" 2>&1

# Bring the syntra service back up; the entrypoint re-installs the
# four flagship capsules into the fresh volume.
log "starting syntra container"
docker compose up -d syntra >> "$LOG_FILE" 2>&1

# Wait for /health (max ~60s) so the webhook fires only after the
# container is actually serving.
ATTEMPTS=0
MAX_ATTEMPTS=30
until docker compose exec -T syntra curl -fsS http://127.0.0.1:8787/health > /dev/null 2>&1; do
    ATTEMPTS=$((ATTEMPTS + 1))
    if (( ATTEMPTS >= MAX_ATTEMPTS )); then
        log "WARN: syntra did not respond to /health within ${MAX_ATTEMPTS}s"
        break
    fi
    sleep 2
done

log "complete (attempts=${ATTEMPTS})"

# Optional webhook ping.
if [[ -n "${RESET_WEBHOOK_URL:-}" ]]; then
    BODY=$(printf '{"event":"reset","host":"%s","timestamp":"%s","attempts":%d}' \
        "$(hostname)" "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$ATTEMPTS")
    if curl -fsS -X POST -H 'Content-Type: application/json' \
        -d "$BODY" "$RESET_WEBHOOK_URL" >> "$LOG_FILE" 2>&1
    then
        log "webhook posted"
    else
        log "WARN: webhook post failed"
    fi
fi

log "end"
