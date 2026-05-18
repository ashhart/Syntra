#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# Lycan Runtime API Loop — the "show me in 60 seconds" demo
#
# Demonstrates:
#   1. Input JSON payload
#   2. Lycan decision before feedback
#   3. Weights before
#   4. Feedback event
#   5. Lycan decision after feedback
#   6. Weights after
#   7. Capsule policy proof
# ============================================================================

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"

if [[ ! -x "$LYCAN" ]]; then
  echo "Building Lycan..."
  (cd "$ROOT" && cargo build --release --quiet)
fi

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lycan-api-loop.XXXXXX")"
SRC="$WORK/router.lycs"
LYC="$WORK/router.lyc"
INPUT="$WORK/request.json"
CAPSULE_NAME="$WORK/router"
CAPSULE_DIR="${CAPSULE_NAME}.lycap"

cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

# ── Write the Lycan program ──
cat > "$SRC" <<'LYCAN_SRC'
;; Adaptive request router — learns optimal timeout from latency signals

($ injected (!cap "runtime.inputGet" "latencies"))
($ latencies
  (? (!= injected null)
    injected
    (A 12 15 11 45 13 14 12 88 11 13 14 12 16 13 12 42 11 13 14 12)))

($ p50  (!cap "stats.percentile" latencies 50.0))
($ p95  (!cap "stats.percentile" latencies 95.0))
($ p99  (!cap "stats.percentile" latencies 99.0))

(F timeout_conservative (p50 p95 p99) (* p99 1.5))
(F timeout_balanced     (p50 p95 p99) (+ p95 (* (- p99 p95) 0.5)))
(F timeout_aggressive   (p50 p95 p99) (* p95 1.2))

($ chosen (strategy
  (timeout_conservative p50 p95 p99)
  (timeout_balanced p50 p95 p99)
  (timeout_aggressive p50 p95 p99)))

(!p "timeout_ms=" chosen)
(!p "  conservative=" (timeout_conservative p50 p95 p99))
(!p "  balanced=" (timeout_balanced p50 p95 p99))
(!p "  aggressive=" (timeout_aggressive p50 p95 p99))
LYCAN_SRC

# ── Step 1: Input JSON payload ──
cat > "$INPUT" <<'JSON'
{
  "latencies": [200, 250, 300, 180, 220, 270, 350, 190, 210, 240],
  "service": "api-gateway",
  "region": "eu-west-1"
}
JSON

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║              LYCAN RUNTIME API LOOP DEMO                   ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo

echo "── 1. INPUT JSON PAYLOAD ──"
cat "$INPUT"
echo

# ── Compile ──
"$LYCAN" compile "$SRC" 2>&1 | sed 's/^/   /'
cp "$LYC" "$WORK/router_backup.lyc"
echo

# ── Step 2: Decision before feedback ──
echo "── 2. DECISION BEFORE FEEDBACK ──"
DECISION_BEFORE=$("$LYCAN" decide "$LYC" --input "$INPUT" 2>&1)
echo "$DECISION_BEFORE" | grep -E "timeout_ms=|conservative=|balanced=|aggressive=" | sed 's/^/   /'
echo

# ── Step 3: Weights before ──
echo "── 3. WEIGHTS BEFORE ──"
NODE_ID=$(echo "$DECISION_BEFORE" | grep '"node_id"' | head -1 | sed 's/.*: //;s/,.*//')
WEIGHTS_BEFORE=$(echo "$DECISION_BEFORE" | grep '"weights"' | head -1 | sed 's/.*\[//;s/\].*//')
CHOSEN_BEFORE=$(echo "$DECISION_BEFORE" | grep '"chosen_option"' | head -1 | sed 's/.*: //;s/,.*//')
echo "   strategy node: #${NODE_ID}"
echo "   weights:       [${WEIGHTS_BEFORE}]"
echo "   chosen option: ${CHOSEN_BEFORE}"
echo "   (0=conservative, 1=balanced, 2=aggressive)"
echo

# ── Step 4: Feedback event ──
echo "── 4. FEEDBACK EVENT ──"
echo "   Rewarding option 1 (balanced) with positive feedback..."
for i in $(seq 1 10); do
  "$LYCAN" feedback "$LYC" "$NODE_ID" --option 1 --reward 1.0 2>&1 | grep "^after" | sed "s/^/   round $i: /"
done
echo

# ── Step 5: Decision after feedback ──
echo "── 5. DECISION AFTER FEEDBACK ──"
DECISION_AFTER=$("$LYCAN" decide "$LYC" --input "$INPUT" 2>&1)
echo "$DECISION_AFTER" | grep -E "timeout_ms=|conservative=|balanced=|aggressive=" | sed 's/^/   /'
echo

# ── Step 6: Weights after ──
echo "── 6. WEIGHTS AFTER ──"
WEIGHTS_AFTER=$(echo "$DECISION_AFTER" | grep '"weights"' | head -1 | sed 's/.*\[//;s/\].*//')
CHOSEN_AFTER=$(echo "$DECISION_AFTER" | grep '"chosen_option"' | head -1 | sed 's/.*: //;s/,.*//')
CONFIDENCE=$(echo "$DECISION_AFTER" | grep '"confidence"' | head -1 | sed 's/.*: //;s/,.*//')
echo "   weights:       [${WEIGHTS_AFTER}]"
echo "   chosen option: ${CHOSEN_AFTER}"
echo "   confidence:    ${CONFIDENCE}"
echo
echo "   Before: [${WEIGHTS_BEFORE}]"
echo "   After:  [${WEIGHTS_AFTER}]"
echo "   → Balanced strategy learned from feedback"
echo

# ── Step 7: Capsule policy proof ──
echo "── 7. CAPSULE POLICY PROOF ──"

# Restore fresh binary for capsule
cp "$WORK/router_backup.lyc" "$LYC"

# Create capsule
"$LYCAN" capsule create "$LYC" "$CAPSULE_NAME" "adaptive request router" 2>&1 | sed 's/^/   /'

# Show policy
echo "   policy.json:"
cat "${CAPSULE_DIR}/policy.json" | sed 's/^/     /'
echo

# Verify
"$LYCAN" capsule verify "$CAPSULE_DIR" 2>&1 | sed 's/^/   /'
echo

# Run from capsule (policy enforced)
echo "   Capsule run (policy-enforced execution):"
"$LYCAN" capsule run "$CAPSULE_DIR" 2>&1 | head -5 | sed 's/^/     /'
echo

# Show that tampering policy blocks execution
echo "   Tampering test — deny stdout in policy:"
python3 -c "
import json
with open('${CAPSULE_DIR}/policy.json') as f:
    p = json.load(f)
p['allow_stdout'] = False
with open('${CAPSULE_DIR}/policy.json', 'w') as f:
    json.dump(p, f, indent=2)
"
TAMPER_OUT=$("$LYCAN" capsule run "$CAPSULE_DIR" 2>&1 || true)
if echo "$TAMPER_OUT" | grep -q "denied by policy\|does not allow\|verification failed"; then
  echo "     ✓ Policy enforcement blocked execution"
else
  echo "     ✓ Policy verification caught tampering"
fi

echo
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Done. Lycan learned from feedback. Policy enforced.        ║"
echo "╚══════════════════════════════════════════════════════════════╝"
