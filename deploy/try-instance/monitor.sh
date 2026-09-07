#!/usr/bin/env bash
#
# monitor.sh — five-minute health probe for try.syntra.io.
#
# Curl-checks https://try.syntra.io/health. On failure (non-2xx or
# network error) POSTs a JSON payload to ${MONITOR_WEBHOOK_URL} so a
# human gets pinged. Designed for cron:
#   */5 * * * * /opt/syntra-try/monitor.sh
#
# Stays quiet when /health returns OK so cron doesn't mail a stream of
# success messages.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)"

# Source .env so MONITOR_WEBHOOK_URL is available under cron.
if [[ -f "${SCRIPT_DIR}/.env" ]]; then
    set -a
    # shellcheck disable=SC1091
    source "${SCRIPT_DIR}/.env"
    set +a
fi

HEALTH_URL="${MONITOR_HEALTH_URL:-https://try.syntra.io/health}"
TIMEOUT="${MONITOR_TIMEOUT:-10}"
LOG_FILE="${MONITOR_LOG_FILE:-/var/log/syntra-monitor.log}"

mkdir -p "$(dirname "$LOG_FILE")"
touch "$LOG_FILE"

log() {
    printf '%s [monitor] %s\n' "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$*" >> "$LOG_FILE"
}

# Capture both the HTTP status code and the body so we can include
# them in the alert payload. -f makes curl exit non-zero on >=400.
HTTP_OUTPUT=""
HTTP_CODE=""
if HTTP_OUTPUT=$(curl -sS --max-time "$TIMEOUT" \
        -o /dev/null -w '%{http_code}' "$HEALTH_URL" 2>&1)
then
    HTTP_CODE="$HTTP_OUTPUT"
else
    HTTP_CODE="000"
fi

if [[ "$HTTP_CODE" =~ ^2[0-9][0-9]$ ]]; then
    # Healthy. Stay quiet (cron suppresses empty output).
    exit 0
fi

log "FAIL http_code=${HTTP_CODE} url=${HEALTH_URL}"

if [[ -z "${MONITOR_WEBHOOK_URL:-}" ]]; then
    log "no MONITOR_WEBHOOK_URL set; cannot alert"
    exit 1
fi

BODY=$(printf '{"event":"health_fail","host":"%s","url":"%s","http_code":"%s","timestamp":"%s"}' \
    "$(hostname)" "$HEALTH_URL" "$HTTP_CODE" "$(date -u +'%Y-%m-%dT%H:%M:%SZ')")

if curl -fsS -X POST -H 'Content-Type: application/json' \
    -d "$BODY" "$MONITOR_WEBHOOK_URL" >> "$LOG_FILE" 2>&1
then
    log "alert posted"
else
    log "WARN: alert post failed"
fi

exit 1
