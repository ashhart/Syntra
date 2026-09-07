#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYNTRA="$ROOT/target/release/syntra"
CAPSULE="$ROOT/examples/demo_takeaway_demand.lyc"

if [[ ! -x "$SYNTRA" ]]; then
  (cd "$ROOT" && cargo build --release --quiet)
fi

if [[ ! -f "$CAPSULE" ]]; then
  echo "missing demo capsule: $CAPSULE" >&2
  exit 1
fi

BASE="$(mktemp -d "${TMPDIR:-/tmp}/syntra-demo.XXXXXX")"
STORE="$BASE/store"
KEY="syntra-demo-key"
PORT=$((9787 + RANDOM % 500))
ADDR="127.0.0.1:$PORT"
PID=""

cleanup() {
  if [[ -n "$PID" ]]; then
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  rm -rf "$BASE"
}
trap cleanup EXIT

auth_json() {
  curl -sf \
    -H "Authorization: Bearer $KEY" \
    -H "Content-Type: application/json" \
    "$@"
}

echo
echo "  Syntra: Static Policy vs Adaptive Memory"
echo "  ---------------------------------------"
echo

echo "  1. Start appliance"
"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >"$BASE/server.log" 2>&1 &
PID=$!
sleep 1
curl -sf "http://$ADDR/health" >/dev/null
echo "     ok: http://$ADDR"

echo "  2. Create job and install compiled capsule"
auth_json -X POST "http://$ADDR/tenants/demo/jobs" \
  -d '{"id":"takeaway-load","name":"Takeaway Load","description":"Capacity policy learning demo"}' >/dev/null
curl -sf -X POST "http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/capacity/install" \
  -H "Authorization: Bearer $KEY" \
  --data-binary "@$CAPSULE" >/dev/null
echo "     ok: demo / takeaway-load / capacity"

echo "  3. First decision with neutral weights"
FIRST="$(auth_json -X POST "http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/capacity/decide" \
  -d '{"contextKey":"friday-rain-event","input":{"market":"takeaway"}}')"
echo "$FIRST" | python3 -c '
import json, sys
d=json.load(sys.stdin)
decision=d["decisions"][0]
stdout=d.get("stdout", [])
node=decision["node_id"]
weights=decision.get("weights", [])
selected=decision.get("chosen_option", decision.get("selected_option", decision.get("chosen", "?")))
best=[line for line in stdout if "Best takeaway capacity policy:" in line]
weight_text=", ".join(f"{w:.0%}" for w in weights)
best_text=best[-1] if best else "best policy unavailable"
print(f"     strategy node: {node}")
print(f"     selected: {selected}")
print(f"     weights: [{weight_text}]")
print(f"     static analysis: {best_text}")
'

NODE="$(echo "$FIRST" | python3 -c 'import json,sys;print(json.load(sys.stdin)["decisions"][0]["node_id"])')"

echo "  4. Send delayed outcome feedback"
for _ in $(seq 1 14); do
  auth_json -X POST "http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/capacity/feedback" \
    -d "{\"strategyId\":$NODE,\"option\":3,\"reward\":1.0,\"contextKey\":\"friday-rain-event\",\"reason\":\"policy 3 matched demand without waste\"}" >/dev/null
done
echo "     ok: rewarded predictive_weather_event_prescale 14 times"

echo "  5. Inspect learned weights"
REPORT="$(auth_json "http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/capacity/report")"
echo "$REPORT" | python3 -c '
import json, sys
r=json.load(sys.stdin)
s=r["strategies"][0]
weights={int(o["option"]): float(o["weight"]) for o in s["options"]}
for option, name in [
    (0, "reactive_no_prescale"),
    (1, "simple_last_week"),
    (2, "aggressive_overprovision"),
    (3, "predictive_weather_event_prescale"),
    (4, "human_manual_schedule"),
]:
    w=weights.get(option, 0.0)
    print(f"     {option} {name:<34} {w:5.1%} " + "█" * int(w * 32))
if weights.get(3, 0.0) < 0.70:
    raise SystemExit("policy 3 did not become the clear winner")
'

echo "  6. Restart and prove memory survived"
kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
PID=""
PORT2=$((PORT + 1))
ADDR="127.0.0.1:$PORT2"
"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >"$BASE/server2.log" 2>&1 &
PID=$!
sleep 1
REPORT2="$(auth_json "http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/capacity/report")"
echo "$REPORT2" | python3 -c '
import json, sys
r=json.load(sys.stdin)
w={int(o["option"]): float(o["weight"]) for o in r["strategies"][0]["options"]}
print(f"     policy 3 after restart: {w.get(3, 0.0):.1%}")
if w.get(3, 0.0) < 0.70:
    raise SystemExit("learned memory did not persist")
'

echo
echo "  Result: static policy stayed static; Syntra learned the winning policy and persisted it."
echo
