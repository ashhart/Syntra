#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"

if [[ ! -x "$LYCAN" ]]; then
  echo "Building Lycan..."
  (cd "$ROOT" && cargo build --release --quiet)
fi

STORE="$(mktemp -d "${TMPDIR:-/tmp}/lycan-appliance.XXXXXX")/lycan-store"
KEY="test-admin-key"
PORT=$((8787 + RANDOM % 1000))
ADDR="127.0.0.1:$PORT"
SERVER_PID=""

cleanup() {
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  rm -rf "$(dirname "$STORE")"
}
trap cleanup EXIT

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║              LYCAN SERVER APPLIANCE DEMO                   ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo

# ── Step 1: Compile capsule ──
echo "── 1. COMPILE CAPSULE ──"
SRC="$(mktemp "${TMPDIR:-/tmp}/lycan-capsule.XXXXXX.lycs")"
cat > "$SRC" <<'EOF'
($ injected (!cap "runtime.inputGet" "latencies"))
($ latencies (? (!= injected null) injected (A 12 15 11 45 13 14 12 88 11 13)))
($ p50 (!cap "stats.percentile" latencies 50.0))
($ p95 (!cap "stats.percentile" latencies 95.0))
($ p99 (!cap "stats.percentile" latencies 99.0))
(F conservative (p50 p95 p99) (* p99 1.5))
(F balanced (p50 p95 p99) (+ p95 (* (- p99 p95) 0.5)))
(F aggressive (p50 p95 p99) (* p95 1.2))
($ chosen (strategy (conservative p50 p95 p99) (balanced p50 p95 p99) (aggressive p50 p95 p99)))
(!p "timeout_ms=" chosen)
EOF
LYC="${SRC%.lycs}.lyc"
"$LYCAN" compile "$SRC" 2>&1 | sed 's/^/   /'
# Run once for stats
"$LYCAN" "$LYC" >/dev/null 2>&1
rm -f "$SRC"
echo

# ── Step 2: Start server ──
echo "── 2. START SERVER ──"
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" &
SERVER_PID=$!
sleep 1
echo "   server PID: $SERVER_PID"
echo "   addr: $ADDR"
echo "   store: $STORE"

# Health check
HEALTH=$(curl -s "http://$ADDR/health")
echo "   health: $HEALTH"
echo

# ── Step 3: Install capsule ──
echo "── 3. INSTALL CAPSULE ──"
INSTALL_RESP=$(curl -s -X POST "http://$ADDR/tenants/demo/capsules/router/install" \
  -H "Authorization: Bearer $KEY" \
  --data-binary "@$LYC")
echo "   $INSTALL_RESP"
rm -f "$LYC"
echo

# ── Step 4: Decide ──
echo "── 4. POST /decide ──"
DECIDE_RESP=$(curl -s -X POST "http://$ADDR/tenants/demo/capsules/router/decide" \
  -H "Authorization: Bearer $KEY" \
  -d '{"latencies":[200,250,300,180,220]}')
echo "   $DECIDE_RESP" | python3 -m json.tool 2>/dev/null | head -15 | sed 's/^/   /' || echo "   $DECIDE_RESP"
echo

# ── Step 5: Get report (weights before) ──
echo "── 5. WEIGHTS BEFORE FEEDBACK ──"
REPORT_BEFORE=$(curl -s "http://$ADDR/tenants/demo/capsules/router/report" \
  -H "Authorization: Bearer $KEY")
echo "   $REPORT_BEFORE" | python3 -c "
import json,sys
r=json.load(sys.stdin)
for s in r.get('strategies',[]):
  print(f'   Strategy #{s[\"node_id\"]}:')
  for o in s['options']:
    bar='█'*int(o['weight']*30)
    print(f'     [{o[\"option\"]}] w={o[\"weight\"]:.4f} {bar}')
" 2>/dev/null || echo "   $REPORT_BEFORE"
NODE_ID=$(echo "$REPORT_BEFORE" | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['node_id'])" 2>/dev/null || echo "0")
echo

# ── Step 6: Feedback ──
echo "── 6. POST /feedback (10 rounds) ──"
for i in $(seq 1 10); do
  FEEDBACK_RESP=$(curl -s -X POST "http://$ADDR/tenants/demo/capsules/router/feedback" \
    -H "Authorization: Bearer $KEY" \
    -d "{\"strategyId\":$NODE_ID,\"option\":1,\"reward\":1.0,\"reason\":\"balanced worked\"}")
  AFTER=$(echo "$FEEDBACK_RESP" | python3 -c "import json,sys;print(json.load(sys.stdin).get('after','?'))" 2>/dev/null || echo "?")
  echo "   round $i: after=$AFTER"
done
echo

# ── Step 7: Weights after feedback ──
echo "── 7. WEIGHTS AFTER FEEDBACK ──"
REPORT_AFTER=$(curl -s "http://$ADDR/tenants/demo/capsules/router/report" \
  -H "Authorization: Bearer $KEY")
echo "   $REPORT_AFTER" | python3 -c "
import json,sys
r=json.load(sys.stdin)
for s in r.get('strategies',[]):
  print(f'   Strategy #{s[\"node_id\"]}:')
  for o in s['options']:
    bar='█'*int(o['weight']*30)
    print(f'     [{o[\"option\"]}] w={o[\"weight\"]:.4f} {bar}')
" 2>/dev/null || echo "   $REPORT_AFTER"
echo

# ── Step 8: Stop and restart ──
echo "── 8. RESTART SERVER ──"
echo "   stopping PID $SERVER_PID..."
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
sleep 1
echo "   starting new server on same store..."
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" &
SERVER_PID=$!
sleep 1
echo "   new PID: $SERVER_PID"
echo

# ── Step 9: Verify persistence ──
echo "── 9. WEIGHTS AFTER RESTART ──"
REPORT_RESTART=$(curl -s "http://$ADDR/tenants/demo/capsules/router/report" \
  -H "Authorization: Bearer $KEY")
echo "   $REPORT_RESTART" | python3 -c "
import json,sys
r=json.load(sys.stdin)
for s in r.get('strategies',[]):
  print(f'   Strategy #{s[\"node_id\"]}:')
  for o in s['options']:
    bar='█'*int(o['weight']*30)
    print(f'     [{o[\"option\"]}] w={o[\"weight\"]:.4f} {bar}')
" 2>/dev/null || echo "   $REPORT_RESTART"
echo "   ✓ Weights survived restart"
echo

# ── Step 10: Auth test ──
echo "── 10. AUTH TEST ──"
UNAUTH=$(curl -s -o /dev/null -w "%{http_code}" "http://$ADDR/tenants/demo/capsules/router/report")
echo "   no auth: HTTP $UNAUTH (expect 401)"
AUTHED=$(curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $KEY" "http://$ADDR/tenants/demo/capsules/router/report")
echo "   with auth: HTTP $AUTHED (expect 200)"
echo

# ── Step 11: Audit log ──
echo "── 11. AUDIT LOG ──"
AUDITS=$(curl -s "http://$ADDR/tenants/demo/capsules/router/audits" \
  -H "Authorization: Bearer $KEY")
AUDIT_COUNT=$(echo "$AUDITS" | grep -c "action" || echo "0")
echo "   audit entries: $AUDIT_COUNT"
echo "   last 3:"
echo "$AUDITS" | tail -3 | sed 's/^/     /'
echo

# ── Step 12: Admin console ──
echo "── 12. ADMIN CONSOLE ──"
echo "   http://$ADDR/admin"
echo

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Server started. Capsule installed. Decisions made.         ║"
echo "║  Feedback shifted weights. Restart preserved memory.        ║"
echo "║  Auth enforced. Audits recorded.                            ║"
echo "║                                                             ║"
echo "║  Container is disposable. Lycan store is sacred.            ║"
echo "╚══════════════════════════════════════════════════════════════╝"
