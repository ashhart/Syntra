#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"

if [[ ! -x "$LYCAN" ]]; then
  (cd "$ROOT" && cargo build --release --quiet 2>/dev/null)
fi

STORE="$(mktemp -d "${TMPDIR:-/tmp}/lycan-qs.XXXXXX")/store"
KEY="demo-key"
PORT=$((8787 + RANDOM % 1000))
ADDR="127.0.0.1:$PORT"
PID=""

cleanup() { [[ -n "$PID" ]] && kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true; rm -rf "$(dirname "$STORE")"; } 2>/dev/null
trap cleanup EXIT

# Compile capsule
SRC=$(mktemp "${TMPDIR:-/tmp}/lycan-qs.XXXXXX.lycs")
cat > "$SRC" <<'EOF'
($ inp (!cap "runtime.inputGet" "latencies"))
($ lat (? (!= inp null) inp (A 12 15 11 45 13 14 12 88)))
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

echo
echo "  Lycan Quickstart"
echo "  ────────────────"
echo

# 1
echo "  1. Starting Lycan"
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!
sleep 1
echo "     ✓ http://$ADDR"

# 2
echo "  2. Installing capsule"
curl -sf -X POST "http://$ADDR/tenants/demo/capsules/router/install" \
  -H "Authorization: Bearer $KEY" --data-binary "@$LYC" >/dev/null
rm -f "$LYC"
echo "     ✓ demo/router"

# 3
RESP=$(curl -sf -X POST "http://$ADDR/tenants/demo/capsules/router/decide" \
  -H "Authorization: Bearer $KEY" -d '{"latencies":[200,250,300]}')
W=$(echo "$RESP" | python3 -c "import json,sys;d=json.load(sys.stdin)['decisions'][0];print(f'[{d[\"weights\"][0]:.0%}, {d[\"weights\"][1]:.0%}, {d[\"weights\"][2]:.0%}]')" 2>/dev/null)
echo "  3. First decision: weights $W"

# 4
NODE=$(echo "$RESP" | python3 -c "import json,sys;print(json.load(sys.stdin)['decisions'][0]['node_id'])" 2>/dev/null)
echo "  4. Sending feedback (10 rounds)"
for _ in $(seq 1 10); do
  curl -sf -X POST "http://$ADDR/tenants/demo/capsules/router/feedback" \
    -H "Authorization: Bearer $KEY" \
    -d "{\"strategyId\":$NODE,\"option\":1,\"reward\":1.0}" >/dev/null
done
echo "     ✓ balanced rewarded"

# 5
RESP2=$(curl -sf -X POST "http://$ADDR/tenants/demo/capsules/router/decide" \
  -H "Authorization: Bearer $KEY" -d '{"latencies":[200,250,300]}')
W2=$(echo "$RESP2" | python3 -c "import json,sys;d=json.load(sys.stdin)['decisions'][0];print(f'[{d[\"weights\"][0]:.0%}, {d[\"weights\"][1]:.0%}, {d[\"weights\"][2]:.0%}]')" 2>/dev/null)
echo "  5. After feedback: weights $W2"

# 6
echo "  6. Restarting server"
kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true
sleep 1
PORT2=$((PORT + 1))
ADDR="127.0.0.1:$PORT2"
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!
sleep 1
echo "     ✓ restarted on :$PORT2"

# 7
RPT=$(curl -sf "http://$ADDR/tenants/demo/capsules/router/report" \
  -H "Authorization: Bearer $KEY")
W3=$(echo "$RPT" | python3 -c "
import json,sys
s=json.load(sys.stdin)['strategies'][0]
ws=', '.join(f'{o[\"weight\"]:.0%}' for o in s['options'])
print(f'[{ws}]')
" 2>/dev/null)
echo "  7. After restart: weights $W3"

# 8
echo "  8. Admin: http://$ADDR/admin"

echo
echo "  Container is disposable. Memory survived."
echo
