#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

STORE="$(mktemp -d "${TMPDIR:-/tmp}/lycan-regr.XXXXXX")/store"
KEY="regr-key"
PORT=$((9200 + RANDOM % 800))
ADDR="127.0.0.1:$PORT"
PID=""
PASS=0; FAIL=0

cleanup() { [[ -n "$PID" ]] && kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true; rm -rf "$(dirname "$STORE")"; }
trap cleanup EXIT

check() { if [ "$1" = "true" ]; then PASS=$((PASS+1)); echo "  PASS: $2"; else FAIL=$((FAIL+1)); echo "  FAIL: $2"; fi; }

# Compile capsule
SRC=$(mktemp "${TMPDIR:-/tmp}/lycan-regr.XXXXXX.lycs")
cat > "$SRC" <<'EOF'
($ inp (!cap "runtime.inputGet" "latencies"))
($ lat (? (!= inp null) inp (A 12 15 11 45 13)))
($ p50 (!cap "stats.percentile" lat 50.0))
($ p95 (!cap "stats.percentile" lat 95.0))
($ p99 (!cap "stats.percentile" lat 99.0))
(F con (a b c) (* c 1.5))
(F bal (a b c) (+ b (* (- c b) 0.5)))
(F agg (a b c) (* b 1.2))
($ t (strategy (con p50 p95 p99) (bal p50 p95 p99) (agg p50 p95 p99)))
(!p t)
EOF
LYC="${SRC%.lycs}.lyc"
"$LYCAN" compile "$SRC" >/dev/null 2>&1
"$LYCAN" "$LYC" >/dev/null 2>&1
rm -f "$SRC"

# Start server
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!; sleep 1

echo "Lycan API Regression Tests"
echo "=========================="
echo

# 1. Auth
echo "1. Auth"
CODE=$(curl -s -o /dev/null -w "%{http_code}" http://$ADDR/tenants)
check "$([ "$CODE" = "401" ] && echo true || echo false)" "no auth → 401 (got $CODE)"
CODE=$(curl -s -o /dev/null -w "%{http_code}" -H "Authorization: Bearer $KEY" http://$ADDR/tenants)
check "$([ "$CODE" = "200" ] && echo true || echo false)" "with auth → 200 (got $CODE)"
CODE=$(curl -s -o /dev/null -w "%{http_code}" http://$ADDR/admin)
check "$([ "$CODE" = "200" ] && echo true || echo false)" "/admin login shell no auth → 200 (got $CODE)"
CODE=$(curl -s -o /dev/null -w "%{http_code}" http://$ADDR/capabilities)
check "$([ "$CODE" = "401" ] && echo true || echo false)" "admin data no auth → 401 (got $CODE)"
echo

# 2. Install
echo "2. Install capsule"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$LYC" http://$ADDR/tenants/demo/capsules/router/install)
OK=$(echo "$R" | python3 -c "import json,sys;print(json.load(sys.stdin).get('ok',False))" 2>/dev/null)
check "$([ "$OK" = "True" ] && echo true || echo false)" "install returns ok"
rm -f "$LYC"
echo

# 3. Read-only decide
echo "3. Read-only decide"
HASH_BEFORE=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['hash'])" 2>/dev/null)
D=$(curl -s -X POST -H "Authorization: Bearer $KEY" -d '{"latencies":[100,200,300]}' http://$ADDR/tenants/demo/capsules/router/decide)
DEC_ID=$(echo "$D" | python3 -c "import json,sys;print(json.load(sys.stdin)['decisionId'])" 2>/dev/null)
LEARNED=$(echo "$D" | python3 -c "import json,sys;print(json.load(sys.stdin)['learned'])" 2>/dev/null)
check "$([ "$LEARNED" = "False" ] && echo true || echo false)" "default decide learned=false"
HASH_AFTER=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['hash'])" 2>/dev/null)
check "$([ "$HASH_BEFORE" = "$HASH_AFTER" ] && echo true || echo false)" "graph hash unchanged after read-only decide"
echo

# 4. Feedback by decisionId
echo "4. Feedback by decisionId"
FB=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d "{\"decisionId\":\"$DEC_ID\",\"reward\":1.0}" \
  http://$ADDR/tenants/demo/capsules/router/feedback)
FB_OK=$(echo "$FB" | python3 -c "import json,sys;print(json.load(sys.stdin).get('ok',False))" 2>/dev/null)
check "$([ "$FB_OK" = "True" ] && echo true || echo false)" "feedback by decisionId returns ok"
echo

# 5. Feedback with unknown decisionId
echo "5. Unknown decisionId"
FB2=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"decisionId":"dec_nonexistent","reward":1.0}' \
  http://$ADDR/tenants/demo/capsules/router/feedback)
FB2_ERR=$(echo "$FB2" | python3 -c "import json,sys;print('error' in json.load(sys.stdin))" 2>/dev/null)
check "$([ "$FB2_ERR" = "True" ] && echo true || echo false)" "unknown decisionId returns error"
echo

# 6. Oversized body
echo "6. Oversized body"
CODE=$(dd if=/dev/zero bs=1 count=5000000 2>/dev/null | curl -s -o /dev/null -w "%{http_code}" -X POST -H "Authorization: Bearer $KEY" --data-binary @- http://$ADDR/tenants/demo/capsules/router/decide)
check "$([ "$CODE" = "413" ] && echo true || echo false)" "5MB body → 413 (got $CODE)"
echo

# 7. Tenant isolation
echo "7. Tenant isolation"
# Install same capsule for tenant B
"$LYCAN" compile "$ROOT/examples/lycan-internals/demo_adaptive_routing.lycs" >/dev/null 2>&1
curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$ROOT/examples/lycan-internals/demo_adaptive_routing.lyc" http://$ADDR/tenants/other/capsules/router/install >/dev/null
# Feedback tenant demo 5 times
NODE=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['node_id'])" 2>/dev/null)
for _ in $(seq 1 5); do
  curl -s -X POST -H "Authorization: Bearer $KEY" -d "{\"strategyId\":$NODE,\"option\":1,\"reward\":1.0}" http://$ADDR/tenants/demo/capsules/router/feedback >/dev/null
done
W_DEMO=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
NODE_OTHER=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/other/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['node_id'])" 2>/dev/null)
W_OTHER=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/other/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
check "$(python3 -c "print('true' if $W_DEMO > $W_OTHER else 'false')" 2>/dev/null)" "demo weight ($W_DEMO) > other weight ($W_OTHER)"
echo

# 8. Job isolation
echo "8. Job isolation"
curl -s -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"id":"takeaway-load","name":"Takeaway Load"}' \
  http://$ADDR/tenants/demo/jobs >/dev/null
curl -s -X POST -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"id":"ticket-triage","name":"Ticket Triage"}' \
  http://$ADDR/tenants/demo/jobs >/dev/null
JOBS=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs)
HAS_JOBS=$(echo "$JOBS" | python3 -c "import json,sys; ids={j['id'] for j in json.load(sys.stdin)['jobs']}; print('true' if {'takeaway-load','ticket-triage'} <= ids else 'false')" 2>/dev/null)
check "$HAS_JOBS" "jobs list includes takeaway-load and ticket-triage"

curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$ROOT/examples/lycan-internals/demo_adaptive_routing.lyc" http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/install >/dev/null
curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$ROOT/examples/lycan-internals/demo_adaptive_routing.lyc" http://$ADDR/tenants/demo/jobs/ticket-triage/capsules/router/install >/dev/null
JD=$(curl -s -X POST -H "Authorization: Bearer $KEY" -d '{"latencies":[100,200,300]}' http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/decide)
JD_JOB=$(echo "$JD" | python3 -c "import json,sys; print(json.load(sys.stdin).get('job',''))" 2>/dev/null)
check "$([ "$JD_JOB" = "takeaway-load" ] && echo true || echo false)" "job decide response includes job"

JOB_NODE=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['node_id'])" 2>/dev/null)
JOB_W_BEFORE=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs/ticket-triage/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
for _ in $(seq 1 5); do
  curl -s -X POST -H "Authorization: Bearer $KEY" -d "{\"strategyId\":$JOB_NODE,\"option\":1,\"reward\":1.0}" http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/feedback >/dev/null
done
JOB_W_A=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
JOB_W_B=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs/ticket-triage/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
check "$(python3 -c "print('true' if $JOB_W_A > $JOB_W_B and abs($JOB_W_B - $JOB_W_BEFORE) < 0.0001 else 'false')" 2>/dev/null)" "takeaway job weight ($JOB_W_A) changed, ticket job stayed ($JOB_W_B)"

JLOG=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/decisions)
JOB_LOG_HAS_JOB=$(echo "$JLOG" | python3 -c "import json,sys; lines=[json.loads(l) for l in sys.stdin if l.strip()]; print('true' if lines and all(x.get('job')=='takeaway-load' for x in lines) else 'false')" 2>/dev/null)
check "$JOB_LOG_HAS_JOB" "job decision log includes job"
JAUDIT=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/jobs/takeaway-load/capsules/router/audits)
JOB_AUDIT_HAS_JOB=$(echo "$JAUDIT" | python3 -c "import json,sys; lines=[json.loads(l) for l in sys.stdin if l.strip()]; print('true' if lines and all(x.get('job')=='takeaway-load' for x in lines) else 'false')" 2>/dev/null)
check "$JOB_AUDIT_HAS_JOB" "job audit log includes job"
echo

# 9. JSON escaping
echo "9. JSON escaping"
D_ESC=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"msg":"test with \"quotes\" and \\backslash"}' \
  http://$ADDR/tenants/demo/capsules/router/decide)
VALID=$(echo "$D_ESC" | python3 -c "import json,sys;json.load(sys.stdin);print('true')" 2>/dev/null || echo "false")
check "$VALID" "response with special chars is valid JSON"
echo

# 10. Persistence
echo "10. Persistence"
W_BEFORE=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true
PORT2=$((PORT+1)); ADDR="127.0.0.1:$PORT2"
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!; sleep 1
W_AFTER=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/report | python3 -c "import json,sys;print(json.load(sys.stdin)['strategies'][0]['options'][1]['weight'])" 2>/dev/null)
check "$([ "$W_BEFORE" = "$W_AFTER" ] && echo true || echo false)" "weights survived restart ($W_BEFORE = $W_AFTER)"
echo

# 11. Decision log valid JSONL
echo "11. Decision log"
DLOG=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/decisions)
DCOUNT=$(echo "$DLOG" | grep -c "decisionId\|id" || echo "0")
VALID_JSONL=$(echo "$DLOG" | python3 -c "
import json,sys
ok=True
for line in sys.stdin:
    line=line.strip()
    if line:
        try: json.loads(line)
        except: ok=False; break
print('true' if ok else 'false')
" 2>/dev/null)
check "$VALID_JSONL" "decision.jsonl is valid JSONL ($DCOUNT entries)"
echo

# 12. Audit log valid JSONL
echo "12. Audit log"
ALOG=$(curl -s -H "Authorization: Bearer $KEY" http://$ADDR/tenants/demo/capsules/router/audits)
ACOUNT=$(echo "$ALOG" | wc -l | tr -d ' ')
VALID_AUDIT=$(echo "$ALOG" | python3 -c "
import json,sys
ok=True
for line in sys.stdin:
    line=line.strip()
    if line:
        try: json.loads(line)
        except: ok=False; break
print('true' if ok else 'false')
" 2>/dev/null)
check "$VALID_AUDIT" "audit.jsonl is valid JSONL ($ACOUNT entries)"
echo

echo "=========================="
echo "PASS: $PASS  FAIL: $FAIL"
if [ "$FAIL" -gt 0 ]; then echo "REGRESSION DETECTED"; exit 1; fi
echo "All checks passed."
