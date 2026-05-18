#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"

if [[ ! -x "$LYCAN" ]]; then
  echo "Building Lycan..."
  (cd "$ROOT" && cargo build --release --quiet)
fi

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║           LYCAN AUTONOMOUS EVOLUTION DEMO                  ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo

LYC="$ROOT/examples/lycan-internals/demo_evolve_target.lyc"

# Clean up from previous runs
rm -f "$LYC" "${LYC}.evolution.jsonl" "${LYC}.evolve.lock"
rm -rf "${LYC}.snapshots"

# ── Step 1: Compile weak strategy ──
echo "── 1. COMPILE WEAK STRATEGY ──"
"$LYCAN" compile "$ROOT/examples/lycan-internals/demo_evolve_target.lycs" 2>&1 | sed 's/^/   /'
"$LYCAN" "$LYC" 2>/dev/null | sed 's/^/   output: /'
echo

# ── Step 2: Initial learn-report ──
echo "── 2. INITIAL LEARN REPORT ──"
"$LYCAN" learn-report "$LYC" 2>&1 | grep -E "option|avg time|weight|tried" | sed 's/^/   /'
BEFORE_NODES=$("$LYCAN" inspect "$LYC" 2>/dev/null | grep -c '"id"' || echo "?")
echo "   graph nodes: ${BEFORE_NODES}"
echo

# ── Step 3: Dry-run first ──
echo "── 3. DRY-RUN (verify without mutating) ──"
"$LYCAN" evolve "$LYC" --proposal "$ROOT/examples/proposals/good_strategy.json" --dry-run --min-improvement 0 2>&1 | grep -v "^12502500" | sed 's/^/   /'
echo

# ── Step 4: Apply good proposal ──
echo "── 4. EVOLVE: GOOD PROPOSAL ──"
BEFORE_HASH=$(shasum -a 256 "$LYC" | cut -d' ' -f1)
echo "   hash before: ${BEFORE_HASH:0:16}..."
"$LYCAN" evolve "$LYC" --proposal "$ROOT/examples/proposals/good_strategy.json" --min-improvement 0 2>&1 | grep -v "^12502500" | sed 's/^/   /'
AFTER_HASH=$(shasum -a 256 "$LYC" | cut -d' ' -f1)
echo "   hash after:  ${AFTER_HASH:0:16}..."
if [[ "$BEFORE_HASH" != "$AFTER_HASH" ]]; then
  echo "   ✓ graph mutated (proposal grafted)"
else
  echo "   ✗ graph unchanged (unexpected)"
fi
echo

# ── Step 5: Show what changed ──
echo "── 5. POST-EVOLUTION STATE ──"
"$LYCAN" learn-report "$LYC" 2>&1 | grep -E "option|avg time|weight|tried|n_options" | sed 's/^/   /'
AFTER_NODES=$("$LYCAN" inspect "$LYC" 2>/dev/null | grep -c '"id"' || echo "?")
echo "   graph nodes: ${BEFORE_NODES} → ${AFTER_NODES}"
echo

# ── Step 6: Apply wrong-output proposal (should reject) ──
echo "── 6. EVOLVE: WRONG OUTPUT (should reject) ──"
BEFORE_HASH2=$(shasum -a 256 "$LYC" | cut -d' ' -f1)
"$LYCAN" evolve "$LYC" --proposal "$ROOT/examples/proposals/wrong_output.json" 2>&1 | grep -v "^12502500" | sed 's/^/   /'
AFTER_HASH2=$(shasum -a 256 "$LYC" | cut -d' ' -f1)
if [[ "$BEFORE_HASH2" == "$AFTER_HASH2" ]]; then
  echo "   ✓ rollback: hash unchanged (byte-identical restore)"
else
  echo "   ✗ hash changed (rollback failed!)"
fi
echo

# ── Step 7: Show external journal ──
echo "── 7. EXTERNAL EVOLUTION JOURNAL ──"
echo "   ${LYC}.evolution.jsonl:"
cat "${LYC}.evolution.jsonl" | while read -r line; do
  EVENT=$(echo "$line" | grep -o '"event":"[^"]*"' | head -1 | cut -d'"' -f4)
  echo "     ${EVENT}"
done
echo "   entries: $(wc -l < "${LYC}.evolution.jsonl" | tr -d ' ')"
echo

# ── Step 8: Show snapshots ──
echo "── 8. SNAPSHOTS ──"
ls "${LYC}.snapshots/" 2>/dev/null | sed 's/^/   /' || echo "   (none)"
echo

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Lycan observed, verified, grafted, and audited.           ║"
echo "║  Good proposal accepted. Bad proposal rejected + rolled    ║"
echo "║  back. External journal survives rollback.                 ║"
echo "╚══════════════════════════════════════════════════════════════╝"
