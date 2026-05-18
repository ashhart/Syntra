#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lycan-evolve-showcase.XXXXXX")"
cleanup() { rm -rf "$WORK"; }
trap cleanup EXIT

PASS=0
FAIL=0
N=200000
EXPECTED=20000100000

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
echo "SHOWCASE 03: AUTONOMOUS EVOLUTION"
echo "============================================================"
echo "Candidate-first mutation. Valid proposal grafted. Wrong proposal rejected. Journal survives."
echo

cat > "$WORK/target.lycs" <<EOF
(F sum_loop (n)
  (\$! total 0) (\$! i 1)
  (W (<= i n) (= total (+ total i)) (= i (+ i 1)))
  total)

(\$ result (strategy (sum_loop $N)))
(!p result)
EOF

cat > "$WORK/good.json" <<EOF
{
  "name": "sum_formula",
  "source": "(F sum_formula (n) (/ (* n (+ n 1)) 2))\\n(sum_formula $N)",
  "insert_into_strategy": 22,
  "expected_output": "$EXPECTED"
}
EOF

cat > "$WORK/wrong.json" <<EOF
{
  "name": "sum_wrong",
  "source": "(F sum_wrong (n) (* n n))\\n(sum_wrong $N)",
  "insert_into_strategy": 22,
  "expected_output": "$EXPECTED"
}
EOF

"$LYCAN" compile "$WORK/target.lycs" >/dev/null
"$LYCAN" "$WORK/target.lyc" >/dev/null

BEFORE_HASH="$(shasum -a 256 "$WORK/target.lyc" | cut -d' ' -f1)"
echo "1. Good proposal: formula candidate"
GOOD_OUT="$("$LYCAN" evolve "$WORK/target.lyc" --proposal "$WORK/good.json" --min-improvement 0 2>&1 | grep -v "^$EXPECTED" || true)"
AFTER_HASH="$(shasum -a 256 "$WORK/target.lyc" | cut -d' ' -f1)"
if echo "$GOOD_OUT" | grep -q "accepted" && [[ "$BEFORE_HASH" != "$AFTER_HASH" ]]; then
  echo "   accepted through compile -> verify -> benchmark -> graft"
  echo "   hash before: ${BEFORE_HASH:0:16}..."
  echo "   hash after:  ${AFTER_HASH:0:16}..."
  GOOD_OK=true
else
  echo "$GOOD_OUT" | sed 's/^/   /'
  GOOD_OK=false
fi
check "$GOOD_OK" "valid candidate promoted and graph hash changed"

echo
echo "2. Bad proposal: wrong output candidate"
BEFORE_BAD="$(shasum -a 256 "$WORK/target.lyc" | cut -d' ' -f1)"
BAD_OUT="$("$LYCAN" evolve "$WORK/target.lyc" --proposal "$WORK/wrong.json" 2>&1 | grep -v "^$EXPECTED" || true)"
AFTER_BAD="$(shasum -a 256 "$WORK/target.lyc" | cut -d' ' -f1)"
echo "$BAD_OUT" | grep -E "rejected|wrong answer" | sed 's/^/   /' || true
check "$(echo "$BAD_OUT" | grep -q "rejected" && [[ "$BEFORE_BAD" == "$AFTER_BAD" ]] && echo true || echo false)" "wrong candidate rejected and live graph stayed byte-identical"

echo
echo "3. External journal"
JOURNAL="$WORK/target.lyc.evolution.jsonl"
EVENTS="$(python3 - "$JOURNAL" <<'PY'
import json, sys
events = []
for line in open(sys.argv[1]):
    if line.strip():
        events.append(json.loads(line)["event"])
print(",".join(events))
PY
)"
echo "   $EVENTS"
check "$(python3 - "$EVENTS" <<'PY'
import sys
events = sys.argv[1].split(",")
print("true" if "ProposalAccepted" in events and "ProposalRejected" in events else "false")
PY
)" "journal records accepted and rejected attempts"

echo
echo "PASS: $PASS  FAIL: $FAIL"
[[ "$FAIL" -eq 0 ]]
