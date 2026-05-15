#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

STORE="$(mktemp -d "${TMPDIR:-/tmp}/lycan-sandbox.XXXXXX")/store"
KEY="sandbox-key"
PORT=$((9300 + RANDOM % 700))
ADDR="127.0.0.1:$PORT"
PID=""
PASS=0; FAIL=0

cleanup() { [[ -n "$PID" ]] && kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null || true; rm -rf "$(dirname "$STORE")"; }
trap cleanup EXIT
check() { if [ "$1" = "true" ]; then PASS=$((PASS+1)); echo "  PASS: $2"; else FAIL=$((FAIL+1)); echo "  FAIL: $2"; fi; }

# Compile capsule that tries file.readText and file.exists
SRC=$(mktemp "${TMPDIR:-/tmp}/lycan-sb.XXXXXX.lycs")
cat > "$SRC" <<'EOF'
($ path (!cap "runtime.inputGet" "path"))
($ mode (!cap "runtime.inputGet" "mode"))
(? (== mode "read")
  (!p (!cap "file.readText" path))
  (? (== mode "exists")
    (!p (!cap "file.exists" path))
    (? (== mode "http")
      (!p (!cap "http.get" path))
      (!p "unknown mode"))))
EOF
LYC="${SRC%.lycs}.lyc"
"$LYCAN" compile "$SRC" >/dev/null 2>&1
rm -f "$SRC"

# Start server
"$LYCAN" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
PID=$!; sleep 1

# Install capsule
curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$LYC" \
  http://$ADDR/tenants/demo/capsules/sandbox/install >/dev/null
rm -f "$LYC"

# Write a test file inside capsule working dir and enable file_read in policy
CAPSULE_DIR="$STORE/tenants/demo/jobs/default/capsules/sandbox"
mkdir -p "$CAPSULE_DIR/data"
echo "sandbox allowed" > "$CAPSULE_DIR/data/test.txt"
# Update policy to allow file_read (for sandbox-scoped access tests)
curl -s -X PUT -H "Authorization: Bearer $KEY" \
  -d '{"allow_stdout":true,"allow_stdin":false,"allow_file_read":true,"allow_file_write":false,"allow_network":true,"allowed_hosts":[],"deny_private_networks":true}' \
  http://$ADDR/tenants/demo/capsules/sandbox/policy >/dev/null

echo "Lycan Sandbox Tests"
echo "==================="
echo

# Helper: check if response contains sandbox denial (in error or result field)
denied() { echo "$1" | python3 -c "
import json,sys
r=json.load(sys.stdin)
s=str(r)
print('true' if any(k in s for k in ['denied','sandbox','absolute','traversal','allowed_hosts','not in allowed','no allowed_hosts']) else 'false')
" 2>/dev/null || echo "false"; }

# 1. Absolute path denied
echo "1. File sandbox: absolute path"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"path":"/etc/passwd","mode":"read"}' \
  http://$ADDR/tenants/demo/capsules/sandbox/decide 2>&1)
check "$(denied "$R")" "file.readText /etc/passwd denied"

# 2. Path traversal denied
echo "2. File sandbox: traversal"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"path":"../../etc/passwd","mode":"read"}' \
  http://$ADDR/tenants/demo/capsules/sandbox/decide 2>&1)
check "$(denied "$R")" "file.readText ../../etc/passwd denied"

# 3. Relative path inside capsule allowed (no error = success, result is null because !p returns null)
echo "3. File sandbox: relative path"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"path":"data/test.txt","mode":"read"}' \
  http://$ADDR/tenants/demo/capsules/sandbox/decide 2>&1)
HAS_OK=$(echo "$R" | python3 -c "import json,sys;r=json.loads(sys.stdin.read());print(r.get('ok',False))" 2>/dev/null)
HAS_ERR=$(echo "$R" | python3 -c "import json,sys;print('error' in sys.stdin.read())" 2>/dev/null)
check "$([ "$HAS_OK" = "True" ] && echo true || echo false)" "file.readText data/test.txt allowed (ok=$HAS_OK err=$HAS_ERR)"

# 4. file.exists relative path (returns ok without sandbox error)
echo "4. File sandbox: exists relative"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"path":"data/test.txt","mode":"exists"}' \
  http://$ADDR/tenants/demo/capsules/sandbox/decide 2>&1)
HAS_OK=$(echo "$R" | python3 -c "import json,sys;r=json.loads(sys.stdin.read());print(r.get('ok',False))" 2>/dev/null)
check "$([ "$HAS_OK" = "True" ] && echo true || echo false)" "file.exists data/test.txt allowed (ok=$HAS_OK)"

# 5. HTTP to localhost denied
echo "5. Network sandbox: localhost"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"path":"http://localhost:8080/secret","mode":"http"}' \
  http://$ADDR/tenants/demo/capsules/sandbox/decide 2>&1)
check "$(denied "$R")" "http.get localhost denied"

# 6. HTTP to metadata IP denied
echo "6. Network sandbox: metadata"
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"path":"http://169.254.169.254/latest/meta-data/","mode":"http"}' \
  http://$ADDR/tenants/demo/capsules/sandbox/decide 2>&1)
check "$(denied "$R")" "http.get 169.254.169.254 denied"

# 7. Existing demos still work
echo "7. Regression: quickstart capsule"
"$LYCAN" compile "$ROOT/examples/demo_adaptive_routing.lycs" >/dev/null 2>&1
curl -s -X POST -H "Authorization: Bearer $KEY" --data-binary @"$ROOT/examples/demo_adaptive_routing.lyc" \
  http://$ADDR/tenants/demo/capsules/router/install >/dev/null
R=$(curl -s -X POST -H "Authorization: Bearer $KEY" \
  -d '{"latencies":[100,200,300]}' \
  http://$ADDR/tenants/demo/capsules/router/decide 2>&1)
OK=$(echo "$R" | python3 -c "import json,sys;print(json.load(sys.stdin).get('ok',False))" 2>/dev/null)
check "$([ "$OK" = "True" ] && echo true || echo false)" "adaptive routing capsule still works"

echo
echo "==================="
echo "PASS: $PASS  FAIL: $FAIL"
if [ "$FAIL" -gt 0 ]; then echo "SANDBOX ISSUE DETECTED"; exit 1; fi
echo "All sandbox checks passed."
