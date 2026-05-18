#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Lycan Adaptive API Router Under Attack
#
# The same compiled graph survives three regimes:
#   1. providerA is best, so feedback teaches Lycan to use A.
#   2. providerA degrades under attack, feedback pushes Lycan to providerB.
#   3. providerC recovers and becomes fastest, feedback pushes Lycan to C.
#
# No source edits. No prompt edits. No retraining job.
# The binary's weights change from operational feedback.
# ============================================================================

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"

if [[ ! -x "$LYCAN" ]]; then
  echo "Building Lycan..."
  (cd "$ROOT" && cargo build --release --quiet)
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lycan-router-attack.XXXXXX")"
SRC="$WORK/router_attack.lycs"
LYC="$WORK/router_attack.lyc"
NORMAL="$WORK/phase1_normal.json"
ATTACK="$WORK/phase2_attack.json"
RECOVERY="$WORK/phase3_recovery.json"

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

cp "$ROOT/examples/lycan-internals/demo_adaptive_api_router_attack.lycs" "$SRC"

cat > "$NORMAL" <<'JSON'
{
  "phase": "normal_world_providerA_is_best",
  "providers": [
    {"name": "providerA", "latencyMs": 38,  "errorRate": 0.01},
    {"name": "providerB", "latencyMs": 126, "errorRate": 0.00},
    {"name": "providerC", "latencyMs": 240, "errorRate": 0.18}
  ]
}
JSON

cat > "$ATTACK" <<'JSON'
{
  "phase": "providerA_under_attack",
  "providers": [
    {"name": "providerA", "latencyMs": 920, "errorRate": 0.64},
    {"name": "providerB", "latencyMs": 118, "errorRate": 0.00},
    {"name": "providerC", "latencyMs": 210, "errorRate": 0.12}
  ]
}
JSON

cat > "$RECOVERY" <<'JSON'
{
  "phase": "providerC_recovers_and_becomes_fastest",
  "providers": [
    {"name": "providerA", "latencyMs": 880, "errorRate": 0.44},
    {"name": "providerB", "latencyMs": 122, "errorRate": 0.01},
    {"name": "providerC", "latencyMs": 35,  "errorRate": 0.00}
  ]
}
JSON

section() {
  echo
  echo "-- $1 --"
}

weights_from_inspect() {
  "$LYCAN" inspect "$LYC" \
    | grep '"op": "AdaptiveChoice"' \
    | head -1 \
    | sed 's/.*"weights": \[\([^]]*\)\].*/\1/'
}

node_from_inspect() {
  "$LYCAN" inspect "$LYC" \
    | grep '"op": "AdaptiveChoice"' \
    | head -1 \
    | sed 's/.*"id": \([0-9][0-9]*\).*/\1/'
}

decide() {
  local input="$1"
  "$LYCAN" decide "$LYC" --input "$input" 2>&1
}

show_decision() {
  local label="$1"
  local input="$2"
  local out
  out="$(decide "$input")"
  echo "$label"
  echo "$out" | grep -E 'phase:|providerA:|providerB:|providerC:|selected_provider:|selected_latency_ms:|selected_error_rate:' | sed 's/^/   /'
  local chosen
  local weights
  chosen="$(echo "$out" | grep '"chosen_option"' | head -1 | sed 's/.*: //;s/,.*//')"
  weights="$(echo "$out" | grep '"weights"' | head -1 | sed 's/.*\[//;s/\].*//')"
  echo "   chosen_option: ${chosen}"
  echo "   weights:       [${weights}]"
}

train_option() {
  local option="$1"
  local reward="$2"
  local rounds="$3"
  local label="$4"
  echo "   $label"
  local last=""
  for _ in $(seq 1 "$rounds"); do
    last="$("$LYCAN" feedback "$LYC" "$NODE_ID" --option "$option" --reward "$reward" 2>&1)"
  done
  echo "$last" | grep -E 'before:|after:' | sed 's/^/     /'
}

echo "=============================================================="
echo "          LYCAN ADAPTIVE API ROUTER UNDER ATTACK"
echo "=============================================================="
echo
echo "One compiled graph. Three changing worlds. Feedback changes the routing brain."

section "0. COMPILE FRESH GRAPH"
"$LYCAN" compile "$SRC" 2>&1 | sed 's/^/   /'
NODE_ID="$(node_from_inspect)"
INITIAL_WEIGHTS="$(weights_from_inspect)"
INITIAL_HASH="$(shasum -a 256 "$LYC" | awk '{print $1}')"
echo "   adaptive choice node: #$NODE_ID"
echo "   fresh weights:        [$INITIAL_WEIGHTS]"
echo "   binary hash:          ${INITIAL_HASH:0:16}..."
echo "   options: 0=providerA, 1=providerB, 2=providerC"

section "1. NORMAL WORLD: PROVIDER A IS BEST"
show_decision "   Before training:" "$NORMAL"
train_option 0 1.0 12 "Feedback: providerA succeeds repeatedly"
show_decision "   After feedback:" "$NORMAL"

section "2. ATTACK: PROVIDER A DEGRADES"
show_decision "   Before attack feedback, memory still trusts A:" "$ATTACK"
train_option 0 -1.0 8 "Feedback: providerA timeouts/errors punish option 0"
train_option 1 1.0 14 "Feedback: providerB succeeds as safe fallback"
show_decision "   After attack feedback:" "$ATTACK"

section "3. RECOVERY: PROVIDER C BECOMES FASTEST"
show_decision "   Before recovery feedback, memory still trusts B:" "$RECOVERY"
train_option 1 -0.4 6 "Feedback: providerB is stable but no longer optimal"
train_option 2 1.0 18 "Health probes + live outcomes reward providerC"
show_decision "   After recovery feedback:" "$RECOVERY"

section "4. WHAT CHANGED"
FINAL_WEIGHTS="$(weights_from_inspect)"
FINAL_HASH="$(shasum -a 256 "$LYC" | awk '{print $1}')"
echo "   fresh weights: [$INITIAL_WEIGHTS]"
echo "   final weights: [$FINAL_WEIGHTS]"
echo "   initial hash:  ${INITIAL_HASH:0:16}..."
echo "   final hash:    ${FINAL_HASH:0:16}..."
echo "   source file:   unchanged"
echo "   binary graph:  evolved through feedback"

echo
echo "=============================================================="
echo "  Done. The router learned A -> B -> C as the world changed."
echo "=============================================================="
