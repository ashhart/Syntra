#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CAPSULE="$ROOT/examples/demo_takeaway_demand.lyc"
SYNTRA_URL="${SYNTRA_URL:-http://localhost:8787}"

if [[ -z "${LYCAN_ADMIN_KEY:-}" ]]; then
  echo "Set LYCAN_ADMIN_KEY to the Syntra admin key." >&2
  exit 1
fi

auth_json() {
  curl -sf \
    -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
    -H "Content-Type: application/json" \
    "$@"
}

echo "Syntra API demo against $SYNTRA_URL"
curl -sf "$SYNTRA_URL/health"
echo

auth_json -X POST "$SYNTRA_URL/tenants/demo/jobs" \
  -d '{"id":"api-demo","name":"API Demo"}' >/dev/null

curl -sf -X POST "$SYNTRA_URL/tenants/demo/jobs/api-demo/capsules/capacity/install" \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  --data-binary "@$CAPSULE" >/dev/null

FIRST="$(auth_json -X POST "$SYNTRA_URL/tenants/demo/jobs/api-demo/capsules/capacity/decide" \
  -d '{"contextKey":"api-demo","input":{"market":"takeaway"}}')"
echo "$FIRST" | python3 -m json.tool | sed -n '1,40p'

NODE="$(echo "$FIRST" | python3 -c 'import json,sys;print(json.load(sys.stdin)["decisions"][0]["node_id"])')"
auth_json -X POST "$SYNTRA_URL/tenants/demo/jobs/api-demo/capsules/capacity/feedback" \
  -d "{\"strategyId\":$NODE,\"option\":3,\"reward\":1.0,\"contextKey\":\"api-demo\"}" >/dev/null

auth_json "$SYNTRA_URL/tenants/demo/jobs/api-demo/capsules/capacity/report" \
  | python3 -m json.tool \
  | sed -n '1,80p'
