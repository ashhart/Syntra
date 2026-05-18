#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

STORE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lycan-mars-showcase.XXXXXX")"
STORE="$STORE_ROOT/store"
KEY="mars-key"
PORT=$((9500 + RANDOM % 400))
ADDR="127.0.0.1:$PORT"
PID=""
PASS=0
FAIL=0

cleanup() {
  [[ -n "$PID" ]] && kill "$PID" 2>/dev/null || true
  [[ -n "$PID" ]] && wait "$PID" 2>/dev/null || true
  rm -rf "$STORE_ROOT"
}
trap cleanup EXIT

check() {
  if [[ "$1" == "true" ]]; then
    PASS=$((PASS + 1))
    echo "  PASS: $2"
  else
    FAIL=$((FAIL + 1))
    echo "  FAIL: $2"
  fi
}

echo
echo "============================================================"
echo "SHOWCASE 02: LIVE MARS MISSION DECISION"
echo "============================================================"
echo "NASA/JPL Horizons HTTPS -> Lambert solver -> strategy decision -> feedback."
echo

"$LYCAN" compile "$ROOT/examples/lycan-internals/demo_mars_horizons_api.lycs" >/dev/null
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!
sleep 1

AUTH=(-H "Authorization: Bearer $KEY")
JSON=(-H "Authorization: Bearer $KEY" -H "Content-Type: application/json")

curl -sf -X POST "${JSON[@]}" \
  -d '{"id":"mission-control","name":"Mission Control"}' \
  "http://$ADDR/tenants/showcase/jobs" >/dev/null

curl -sf -X POST "${AUTH[@]}" \
  --data-binary "@$ROOT/examples/lycan-internals/demo_mars_horizons_api.lyc" \
  "http://$ADDR/tenants/showcase/jobs/mission-control/capsules/mars/install" >/dev/null

curl -sf -X PUT "${JSON[@]}" \
  -d '{"allow_stdout":true,"allow_stdin":false,"allow_file_read":false,"allow_file_write":false,"allow_network":true,"allowed_hosts":["ssd.jpl.nasa.gov"],"deny_private_networks":true}' \
  "http://$ADDR/tenants/showcase/jobs/mission-control/capsules/mars/policy" >/dev/null

RESP="$(curl -sf -X POST "${JSON[@]}" -d '{
  "horizons": {"start": "2026-Jan-01", "stop": "2028-Jan-01", "step_days": 5.0},
  "max_c3": 12.0,
  "min_tof": 220.0,
  "max_tof": 330.0,
  "search_window_days": 500.0,
  "objective": "minimize_c3"
}' "http://$ADDR/tenants/showcase/jobs/mission-control/capsules/mars/decide")"

printf '%s' "$RESP" | python3 -c '
import json, sys
d = json.load(sys.stdin)
stdout = "\n".join(map(str, d.get("stdout", [])))
for line in stdout.splitlines():
    if any(token in line for token in ["Live NASA/JPL", "Earth records:", "Mars records:", "Date:", "TOF:", "C3:"]):
        print("  " + line)
'

OK="$(echo "$RESP" | python3 -c 'import json,sys; d=json.load(sys.stdin); s="\n".join(map(str,d.get("stdout",[]))); print("true" if d.get("ok") and "Live NASA/JPL Horizons API" in s and "C3:" in s and d.get("decisions") else "false")')"
check "$OK" "live Horizons data produced a structured Mars transfer decision"

DECISION_ID="$(echo "$RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["decisionId"])')"
BEFORE="$(echo "$RESP" | python3 -c 'import json,sys; print(max(json.load(sys.stdin)["decisions"][0]["weights"]))')"
for _ in $(seq 1 8); do
  curl -sf -X POST "${JSON[@]}" \
    -d "{\"decisionId\":\"$DECISION_ID\",\"reward\":1.0}" \
    "http://$ADDR/tenants/showcase/jobs/mission-control/capsules/mars/feedback" >/dev/null
done
AFTER="$(curl -sf "${AUTH[@]}" "http://$ADDR/tenants/showcase/jobs/mission-control/capsules/mars/report" \
  | python3 -c 'import json,sys; print(max(o["weight"] for o in json.load(sys.stdin)["strategies"][0]["options"]))')"
check "$(python3 -c "print('true' if float('$AFTER') > float('$BEFORE') else 'false')")" "mission feedback increased winning strategy confidence"

echo
echo "PASS: $PASS  FAIL: $FAIL"
[[ "$FAIL" -eq 0 ]]
