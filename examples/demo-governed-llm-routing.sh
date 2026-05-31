#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYNTRA="${SYNTRA_BIN:-$ROOT/target/release/syntra}"
CAPSULE="$ROOT/examples/demo_llm_model_router.lyc"

if [[ ! -x "$SYNTRA" ]]; then
  (cd "$ROOT" && cargo build --release --quiet)
fi

if [[ ! -f "$CAPSULE" ]]; then
  echo "missing demo capsule: $CAPSULE" >&2
  echo "compile it with the Lycan language CLI before running this demo" >&2
  exit 1
fi

BASE="$(mktemp -d "${TMPDIR:-/tmp}/syntra-governed-llm.XXXXXX")"
STORE="$BASE/store"
EVENTS="$BASE/shadow-decisions.jsonl"
GATES="$BASE/promotion.yaml"
REPORT="$BASE/promotion-report.md"
KEY="syntra-governed-llm-key"
PORT=$((11200 + RANDOM % 500))
ADDR="127.0.0.1:$PORT"
PID=""

cleanup() {
  if [[ -n "$PID" ]]; then
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
  fi
  if [[ "${KEEP_DEMO_OUTPUT:-0}" != "1" ]]; then
    rm -rf "$BASE"
  else
    echo
    echo "  Kept demo output: $BASE"
  fi
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

dominant_action() {
  python3 -c '
import json, sys
d=json.load(sys.stdin)
weights=d["decisions"][0]["weights"]
idx=max(range(len(weights)), key=lambda i: weights[i])
names=["cheap_fast","balanced","expensive_accurate"]
print(names[idx])
'
}

append_shadow_event() {
  local id="$1"
  local context="$2"
  local segment="$3"
  local baseline="$4"
  local candidate="$5"
  local variant="$6"

  python3 - "$EVENTS" "$id" "$context" "$segment" "$baseline" "$candidate" "$variant" <<'PY'
import json
import sys

path, event_id, context, segment, baseline, candidate, raw_variant = sys.argv[1:]
variant = int(raw_variant)

if context == "support-low-cost":
    rewards = {
        "cheap_fast": 0.82 - 0.01 * (variant % 3),
        "balanced": 0.70 - 0.005 * (variant % 2),
        "expensive_accurate": 0.75 - 0.004 * (variant % 4),
    }
    costs = {"cheap_fast": 0.01, "balanced": 0.03, "expensive_accurate": 0.08}
    latency = {
        "cheap_fast": 210 + 5 * variant,
        "balanced": 340 + 4 * variant,
        "expensive_accurate": 760 + 6 * variant,
    }
    oracle = "cheap_fast"
else:
    rewards = {
        "cheap_fast": 0.40 + 0.005 * (variant % 2),
        "balanced": 0.68 + 0.004 * (variant % 3),
        "expensive_accurate": 0.87 + 0.006 * (variant % 3),
    }
    costs = {"cheap_fast": 0.01, "balanced": 0.06, "expensive_accurate": 0.06}
    latency = {
        "cheap_fast": 250 + 5 * variant,
        "balanced": 610 + 3 * variant,
        "expensive_accurate": 620 + 3 * variant,
    }
    oracle = "expensive_accurate"

event = {
    "id": event_id,
    "contextKey": context,
    "segment": segment,
    "baselineAction": baseline,
    "candidateAction": candidate,
    "actionRewards": rewards,
    "actionCostsUsd": costs,
    "actionLatencyMs": latency,
    "oracleAction": oracle,
}
with open(path, "a", encoding="utf-8") as f:
    f.write(json.dumps(event, separators=(",", ":")) + "\n")
PY
}

SUPPORT='{"task_type":"support","customer_tier":"standard","urgency":"normal","tokens":900}'
LEGAL='{"task_type":"legal","customer_tier":"enterprise","urgency":"normal","tokens":12000}'

cat >"$GATES" <<'YAML'
min_events: 12
min_paired_events: 12
min_candidate_coverage: 1.0
min_reward_uplift: 0.08
min_reward_uplift_ci_lower: 0.05
max_cost_increase: 0.0
max_latency_p95_increase_ms: 20.0
no_segment_regression: true
YAML

echo
echo "  Syntra: Governed LLM Routing"
echo "  ----------------------------"
echo "  shadow log -> replay -> promotion report -> gate pass"
echo

echo "  1. Start Syntra"
"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >"$BASE/server.log" 2>&1 &
PID=$!
sleep 1
curl -sf "http://$ADDR/health" >/dev/null
echo "     ok: http://$ADDR"

echo "  2. Install model-router capsule"
auth_json -X POST "http://$ADDR/tenants/demo/jobs" \
  -d '{"id":"llm-routing","name":"LLM Routing","description":"Governed model choice by delayed feedback"}' >/dev/null
curl -sf -X POST "http://$ADDR/tenants/demo/jobs/llm-routing/capsules/model-router/install" \
  -H "Authorization: Bearer $KEY" \
  --data-binary "@$CAPSULE" >/dev/null
echo "     ok: demo / llm-routing / model-router"

echo "  3. Teach the router from delayed feedback"
FIRST_SUPPORT="$(decide "support-low-cost" "$SUPPORT")"
NODE_ID="$(printf '%s' "$FIRST_SUPPORT" | python3 -c 'import json,sys;print(json.load(sys.stdin)["decisions"][0]["node_id"])')"
for _ in $(seq 1 30); do
  feedback_option "support-low-cost" 0 1.0
done
for _ in $(seq 1 30); do
  feedback_option "legal-high-accuracy" 2 1.0
done
echo "     support-low-cost -> cheap_fast; legal-high-accuracy -> expensive_accurate"

echo "  4. Run Syntra in shadow mode beside the baseline"
echo "     baseline remains balanced; Syntra's dominant learned actions are logged as candidates"
: >"$EVENTS"
for i in $(seq 1 6); do
  support_candidate="$(decide "support-low-cost" "$SUPPORT" | dominant_action)"
  legal_candidate="$(decide "legal-high-accuracy" "$LEGAL" | dominant_action)"
  append_shadow_event "support-$i" "support-low-cost" "support" "balanced" "$support_candidate" "$i"
  append_shadow_event "legal-$i" "legal-high-accuracy" "legal" "balanced" "$legal_candidate" "$i"
done

python3 - "$EVENTS" <<'PY'
import collections
import json
import sys

counts = collections.Counter()
with open(sys.argv[1], encoding="utf-8") as f:
    for line in f:
        event = json.loads(line)
        counts[(event["contextKey"], event["candidateAction"])] += 1
for (context, action), count in sorted(counts.items()):
    print(f"     {context:<22} candidate={action:<20} events={count}")
if counts[("support-low-cost", "cheap_fast")] != 6:
    raise SystemExit("support-low-cost did not converge to cheap_fast")
if counts[("legal-high-accuracy", "expensive_accurate")] != 6:
    raise SystemExit("legal-high-accuracy did not converge to expensive_accurate")
PY

echo "  5. Replay the shadow log against promotion gates"
"$SYNTRA" replay \
  --events "$EVENTS" \
  --gates "$GATES" \
  --format markdown \
  --out "$REPORT" \
  --fail-on-gate >/dev/null

python3 - "$REPORT" <<'PY'
import re
import sys

report = open(sys.argv[1], encoding="utf-8").read()
print()
in_summary = False
for line in report.splitlines():
    if line.startswith("**Status:**"):
        print(f"     {line}")
    elif line == "## Summary":
        in_summary = True
    elif line == "## Promotion Checks":
        in_summary = False
    elif in_summary and re.match(r"\| (Reward uplift|Cost increase / event|p95 latency increase|Oracle match rate) \|", line):
        print(f"     {line}")
print()
print("  Recommendation: promote the LLM router from shadow mode to controlled rollout.")
PY

echo
echo "  Report: $REPORT"
echo "  Result: Syntra proved the adaptive LLM route beats the baseline under reward, cost, latency, and segment gates."
echo
