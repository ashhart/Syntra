#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYNTRA="$ROOT/target/release/syntra"
CAPSULE="$ROOT/examples/demo_llm_model_router.lyc"

if [[ ! -x "$SYNTRA" ]]; then
  (cd "$ROOT" && cargo build --release --quiet)
fi

if [[ ! -f "$CAPSULE" ]]; then
  echo "missing demo capsule: $CAPSULE" >&2
  echo "compile it with the Lycan language CLI before running this demo" >&2
  exit 1
fi

BASE="$(mktemp -d "${TMPDIR:-/tmp}/syntra-llm-router.XXXXXX")"
STORE="$BASE/store"
KEY="syntra-llm-router-key"
PORT=$((10100 + RANDOM % 700))
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

decide() {
  local context="$1"
  local body="$2"
  auth_json -X POST "http://$ADDR/tenants/demo/jobs/llm-routing/capsules/model-router/decide" \
    -d "{\"contextKey\":\"$context\",\"input\":$body}"
}

feedback_option() {
  local context="$1"
  local option="$2"
  local reward="$3"
  auth_json -X POST "http://$ADDR/tenants/demo/jobs/llm-routing/capsules/model-router/feedback" \
    -d "{\"strategyId\":$NODE_ID,\"option\":$option,\"reward\":$reward,\"contextKey\":\"$context\"}" >/dev/null
}

echo
echo "  Syntra: LLM Model Routing"
echo "  -------------------------"
echo "  cheap_fast vs balanced vs expensive_accurate"
echo

echo "  1. Start Syntra"
"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >"$BASE/server.log" 2>&1 &
PID=$!
sleep 1
curl -sf "http://$ADDR/health" >/dev/null
echo "     ok: http://$ADDR"

echo "  2. Install model-router capsule"
auth_json -X POST "http://$ADDR/tenants/demo/jobs" \
  -d '{"id":"llm-routing","name":"LLM Routing","description":"Model choice by delayed feedback"}' >/dev/null
curl -sf -X POST "http://$ADDR/tenants/demo/jobs/llm-routing/capsules/model-router/install" \
  -H "Authorization: Bearer $KEY" \
  --data-binary "@$CAPSULE" >/dev/null
echo "     ok: demo / llm-routing / model-router"

SUPPORT='{"task_type":"support","customer_tier":"standard","urgency":"normal","tokens":900}'
LEGAL='{"task_type":"legal","customer_tier":"enterprise","urgency":"normal","tokens":12000}'

echo "  3. First decisions, neutral weights"
FIRST_SUPPORT="$(decide "support-low-cost" "$SUPPORT")"
FIRST_LEGAL="$(decide "legal-high-accuracy" "$LEGAL")"
printf '%s\n%s\n' "$FIRST_SUPPORT" "$FIRST_LEGAL" | python3 -c '
import json, sys
for raw in sys.stdin:
    d=json.loads(raw)
    dec=d["decisions"][0]
    weights=", ".join(f"{w:.0%}" for w in dec["weights"])
    stdout="\n".join(map(str, d.get("stdout", [])))
    static=[line for line in stdout.splitlines() if "Static best route:" in line][-1]
    context=d["contextKey"]
    chosen=dec["chosen_option"]
    print(f"     {context:<22} selected={chosen} weights=[{weights}] {static}")
'

NODE_ID="$(printf '%s' "$FIRST_SUPPORT" | python3 -c 'import json,sys;print(json.load(sys.stdin)["decisions"][0]["node_id"])')"

echo "  4. Send delayed feedback"
echo "     support-low-cost rewards cheap_fast"
for _ in $(seq 1 30); do
  feedback_option "support-low-cost" 0 1.0
done
echo "     legal-high-accuracy rewards expensive_accurate"
for _ in $(seq 1 30); do
  feedback_option "legal-high-accuracy" 2 1.0
done

echo "  5. Contexts learned different winners"
AFTER_SUPPORT="$(decide "support-low-cost" "$SUPPORT")"
AFTER_LEGAL="$(decide "legal-high-accuracy" "$LEGAL")"
printf '%s\n%s\n' "$AFTER_SUPPORT" "$AFTER_LEGAL" | python3 -c '
import json, sys
ok=True
for raw in sys.stdin:
    d=json.loads(raw)
    dec=d["decisions"][0]
    weights=dec["weights"]
    label=d["contextKey"]
    bars=[]
    for idx, name in [(0, "cheap_fast"), (1, "balanced"), (2, "expensive_accurate")]:
        bars.append(f"{idx} {name:<20} {weights[idx]:5.1%} " + "█" * int(weights[idx] * 28))
    print(f"     {label}")
    for line in bars:
        print(f"       {line}")
    if label == "support-low-cost" and weights[0] < 0.70:
        ok=False
    if label == "legal-high-accuracy" and weights[2] < 0.70:
        ok=False
if not ok:
    raise SystemExit("expected context winners did not dominate")
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
VERIFY_SUPPORT="$(decide "support-low-cost" "$SUPPORT")"
VERIFY_LEGAL="$(decide "legal-high-accuracy" "$LEGAL")"
printf '%s\n%s\n' "$VERIFY_SUPPORT" "$VERIFY_LEGAL" | python3 -c '
import json, sys
for raw in sys.stdin:
    d=json.loads(raw)
    w=d["decisions"][0]["weights"]
    context=d["contextKey"]
    weight_text=", ".join(f"{x:.0%}" for x in w)
    print(f"     {context:<22} [{weight_text}]")
    if context == "support-low-cost":
        assert w[0] > 0.70
    if context == "legal-high-accuracy":
        assert w[2] > 0.70
'

echo
echo "  Result: Syntra learned separate model-routing policies from feedback, with no LLM in the hot path."
echo
