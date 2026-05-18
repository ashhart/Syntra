#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

STORE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/lycan-contexts.XXXXXX")"
STORE="$STORE_ROOT/store"
KEY="context-key"
PORT=$((9400 + RANDOM % 500))
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

api_get() {
  curl -sf -H "Authorization: Bearer $KEY" "http://$ADDR$1"
}

api_post() {
  curl -sf -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" -d "$2" "http://$ADDR$1"
}

api_put() {
  curl -sf -X PUT -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" -d "$2" "http://$ADDR$1"
}

weights_for_context() {
  local ctx="$1"
  echo "$CONTEXTS_JSON" | python3 -c '
import json, sys
ctx = sys.argv[1]
data = json.load(sys.stdin)
for item in data.get("contexts", []):
    if item.get("contextKey") == ctx:
        print(",".join(f"{w:.4f}" for w in item.get("weights", [])))
        break
' "$ctx"
}

best_for_context() {
  local ctx="$1"
  echo "$CONTEXTS_JSON" | python3 -c '
import json, sys
ctx = sys.argv[1]
data = json.load(sys.stdin)
for item in data.get("contexts", []):
    if item.get("contextKey") == ctx:
        weights = item.get("weights", [])
        print(max(range(len(weights)), key=lambda i: weights[i]) if weights else -1)
        break
' "$ctx"
}

decision_best_for_context() {
  local ctx="$1"
  local body
  body="{\"contextKey\":\"$ctx\",\"base\":120}"
  api_post "/tenants/demo/jobs/context-lab/capsules/router/decide" "$body" | python3 -c '
import json, sys
d = json.load(sys.stdin)["decisions"][0]
weights = d.get("contextWeights") or d.get("weights")
print(max(range(len(weights)), key=lambda i: weights[i]))
'
}

train_context() {
  local ctx="$1"
  local option="$2"
  local rounds="$3"
  for _ in $(seq 1 "$rounds"); do
    api_post "/tenants/demo/jobs/context-lab/capsules/router/feedback" \
      "{\"strategyId\":$NODE,\"option\":$option,\"reward\":1.0,\"contextKey\":\"$ctx\"}" >/dev/null
  done
}

SRC="$STORE_ROOT/context_router.lycs"
cat > "$SRC" <<'LYC'
($ base_in (!cap "runtime.inputGet" "base"))
($ base (? (!= base_in null) base_in 100))

(F conservative (x) (+ x 100))
(F balanced (x) x)
(F aggressive (x) (- x 100))

($ choice (strategy
  (conservative base)
  (balanced base)
  (aggressive base)))

(!p "Context memory demo decision:")
(!p choice)
LYC

LYC_FILE="${SRC%.lycs}.lyc"
"$LYCAN" compile "$SRC" >/dev/null

echo
echo "  Lycan Context Memory Demo"
echo "  ========================="
echo
echo "  Same tenant, same job, same capsule."
echo "  Three contextKey values learn three different winners."
echo

echo "1. Starting isolated Lycan API"
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!
sleep 1
curl -sf "http://$ADDR/health" >/dev/null
echo "   API: http://$ADDR"
echo

echo "2. Creating job and installing capsule"
api_post "/tenants/demo/jobs" '{"id":"context-lab","name":"Context Lab","description":"A/B/C context-memory proof"}' >/dev/null
curl -sf -X POST -H "Authorization: Bearer $KEY" --data-binary "@$LYC_FILE" \
  "http://$ADDR/tenants/demo/jobs/context-lab/capsules/router/install" >/dev/null
api_put "/tenants/demo/jobs/context-lab/capsules/router/learning" \
  '{"algorithm":"simpleWeighted","safety":{"maxWeightDeltaPerFeedback":0.15,"minExploration":0.02,"freezeLearning":false}}' >/dev/null
NODE=$(api_get "/tenants/demo/jobs/context-lab/capsules/router/report" | python3 -c 'import json,sys; print(json.load(sys.stdin)["strategies"][0]["node_id"])')
echo "   Strategy node: $NODE"
echo

CTX_A="context:A-speed"
CTX_B="context:B-margin"
CTX_C="context:C-reliability"

echo "3. Seeding A/B/C memory buckets from equal weights"
for ctx in "$CTX_A" "$CTX_B" "$CTX_C"; do
  api_post "/tenants/demo/jobs/context-lab/capsules/router/feedback" \
    "{\"strategyId\":$NODE,\"option\":0,\"reward\":0.0,\"contextKey\":\"$ctx\"}" >/dev/null
done
echo "   A, B, C created"
echo

echo "4. Training each context toward a different option"
train_context "$CTX_A" 0 12
train_context "$CTX_B" 1 12
train_context "$CTX_C" 2 12
echo "   A rewarded option 0: conservative"
echo "   B rewarded option 1: balanced"
echo "   C rewarded option 2: aggressive"
echo

echo "5. Reading /contexts"
CONTEXTS_JSON="$(api_get "/tenants/demo/jobs/context-lab/capsules/router/contexts")"
A_WEIGHTS="$(weights_for_context "$CTX_A")"
B_WEIGHTS="$(weights_for_context "$CTX_B")"
C_WEIGHTS="$(weights_for_context "$CTX_C")"
echo "   A weights: [$A_WEIGHTS]"
echo "   B weights: [$B_WEIGHTS]"
echo "   C weights: [$C_WEIGHTS]"
check "$([[ "$(best_for_context "$CTX_A")" == "0" ]] && echo true || echo false)" "Context A winner is option 0"
check "$([[ "$(best_for_context "$CTX_B")" == "1" ]] && echo true || echo false)" "Context B winner is option 1"
check "$([[ "$(best_for_context "$CTX_C")" == "2" ]] && echo true || echo false)" "Context C winner is option 2"
echo

echo "6. Proving /decide uses the context memory"
A_DEC="$(decision_best_for_context "$CTX_A")"
B_DEC="$(decision_best_for_context "$CTX_B")"
C_DEC="$(decision_best_for_context "$CTX_C")"
check "$([[ "$A_DEC" == "0" ]] && echo true || echo false)" "Decide with Context A uses option 0 weights"
check "$([[ "$B_DEC" == "1" ]] && echo true || echo false)" "Decide with Context B uses option 1 weights"
check "$([[ "$C_DEC" == "2" ]] && echo true || echo false)" "Decide with Context C uses option 2 weights"
echo

echo "7. Memory survives restart"
kill "$PID" 2>/dev/null
wait "$PID" 2>/dev/null || true
PID=""
PORT2=$((PORT + 1))
ADDR="127.0.0.1:$PORT2"
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!
sleep 1
CONTEXTS_JSON="$(api_get "/tenants/demo/jobs/context-lab/capsules/router/contexts")"
check "$([[ "$(best_for_context "$CTX_A")" == "0" && "$(best_for_context "$CTX_B")" == "1" && "$(best_for_context "$CTX_C")" == "2" ]] && echo true || echo false)" "A/B/C winners survived restart"
echo

echo "=============================="
echo "PASS: $PASS  FAIL: $FAIL"
if [[ "$FAIL" -gt 0 ]]; then
  echo "Context memory regression detected"
  exit 1
fi

echo
echo "Result: one capsule, three independent learned memories."
echo
