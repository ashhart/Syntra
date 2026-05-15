#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CAPSULE="$ROOT/examples/demo_takeaway_demand.lyc"
IMAGE="${SYNTRA_IMAGE:-syntra:quickstart}"
KEY="${LYCAN_ADMIN_KEY:-syntra-demo-key}"
PORT="${SYNTRA_PORT:-8787}"
CONTAINER="syntra-quickstart-$PORT"
VOLUME="syntra-quickstart-store-$PORT"

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker volume rm "$VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

auth_json() {
  curl -sf \
    -H "Authorization: Bearer $KEY" \
    -H "Content-Type: application/json" \
    "$@"
}

echo
echo "  Syntra Docker Quickstart"
echo "  ------------------------"
echo

echo "  1. Build image"
docker build -t "$IMAGE" "$ROOT" >/dev/null
echo "     ok: $IMAGE"

echo "  2. Start container"
cleanup
docker volume create "$VOLUME" >/dev/null
docker run -d \
  --name "$CONTAINER" \
  -p "$PORT:8787" \
  -e LYCAN_ADMIN_KEY="$KEY" \
  -v "$VOLUME:/var/lib/lycan" \
  "$IMAGE" >/dev/null

for i in $(seq 1 30); do
  if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null; then
    break
  fi
  sleep 0.5
  if [[ "$i" == "30" ]]; then
    docker logs "$CONTAINER"
    exit 1
  fi
done
echo "     ok: http://127.0.0.1:$PORT"

echo "  3. Auth gate"
UNAUTH="$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT/tenants")"
echo "     unauthenticated /tenants: HTTP $UNAUTH"
[[ "$UNAUTH" == "401" ]]

echo "  4. Install compiled capsule"
auth_json -X POST "http://127.0.0.1:$PORT/tenants/demo/jobs" \
  -d '{"id":"takeaway-load","name":"Takeaway Load"}' >/dev/null
curl -sf -X POST "http://127.0.0.1:$PORT/tenants/demo/jobs/takeaway-load/capsules/capacity/install" \
  -H "Authorization: Bearer $KEY" \
  --data-binary "@$CAPSULE" >/dev/null
echo "     ok: demo/takeaway-load/capacity"

echo "  5. Decide and send feedback"
FIRST="$(auth_json -X POST "http://127.0.0.1:$PORT/tenants/demo/jobs/takeaway-load/capsules/capacity/decide" \
  -d '{"contextKey":"docker-quickstart","input":{"market":"takeaway"}}')"
NODE="$(echo "$FIRST" | python3 -c 'import json,sys;print(json.load(sys.stdin)["decisions"][0]["node_id"])')"
for _ in $(seq 1 14); do
  auth_json -X POST "http://127.0.0.1:$PORT/tenants/demo/jobs/takeaway-load/capsules/capacity/feedback" \
    -d "{\"strategyId\":$NODE,\"option\":3,\"reward\":1.0,\"contextKey\":\"docker-quickstart\"}" >/dev/null
done
echo "     ok: policy 3 rewarded"

echo "  6. Verify learned memory"
WEIGHT="$(auth_json "http://127.0.0.1:$PORT/tenants/demo/jobs/takeaway-load/capsules/capacity/report" | python3 -c '
import json, sys
r=json.load(sys.stdin)
w={int(o["option"]): float(o["weight"]) for o in r["strategies"][0]["options"]}
print(f"{w[3]:.1%}")
assert w[3] > 0.70
')"
echo "     policy 3 weight: $WEIGHT"

echo "  7. Restart container with same volume"
docker rm -f "$CONTAINER" >/dev/null
docker run -d \
  --name "$CONTAINER" \
  -p "$PORT:8787" \
  -e LYCAN_ADMIN_KEY="$KEY" \
  -v "$VOLUME:/var/lib/lycan" \
  "$IMAGE" >/dev/null
sleep 1
WEIGHT2="$(auth_json "http://127.0.0.1:$PORT/tenants/demo/jobs/takeaway-load/capsules/capacity/report" | python3 -c '
import json, sys
r=json.load(sys.stdin)
w={int(o["option"]): float(o["weight"]) for o in r["strategies"][0]["options"]}
print(f"{w[3]:.1%}")
assert w[3] > 0.70
')"
echo "     after restart: $WEIGHT2"

echo
echo "  Result: Docker container is disposable; Syntra memory survived in the volume."
echo
