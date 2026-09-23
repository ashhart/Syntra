#!/usr/bin/env bash
# Smoke test of a v2 build against a real server: auth, capsule creation,
# decide and reward, idempotency, scoped tokens, a feature program and its
# sandbox, metrics, off-policy evaluation, restart persistence, backup and
# doctor. Starts its own server on a free port with a throwaway store.
#
#   cargo build --release
#   ./scripts/smoke-test.sh
#
# SYNTRA_BIN / LYCAN_BIN pick the binaries (default target/release/...).
# Needs bash, curl and python3. Exits 0 only if every check passes.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYNTRA="${SYNTRA_BIN:-$ROOT/target/release/syntra}"
LYCAN="${LYCAN_BIN:-$ROOT/target/release/lycan}"
for b in "$SYNTRA" "$LYCAN"; do
  [[ -x "$b" ]] || { echo "missing $b: run cargo build --release, or set SYNTRA_BIN and LYCAN_BIN" >&2; exit 2; }
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/syntra-smoke.XXXXXX")"
STORE="$WORK/store"
PORT="$(python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
S="http://127.0.0.1:$PORT"
KEY="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
B="$S/v1/tenants/smoke/jobs/prod/capsules/router"
PID=""
PASS=0
FAIL=0

cleanup() {
  [[ -n "$PID" ]] && kill "$PID" 2>/dev/null && wait "$PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

check() { # check <description> <command...>
  local what="$1"; shift
  if "$@" >/dev/null 2>&1; then PASS=$((PASS + 1)); echo "  ok    $what"
  else FAIL=$((FAIL + 1)); echo "  FAIL  $what"; fi
}
status() { # status <method> <url> [curl args...]: print the HTTP status
  local m="$1" u="$2"; shift 2
  curl -s -o /dev/null -w '%{http_code}' -X "$m" "$u" "$@"
}
api() { # api <method> <url> [curl args...]: print the body, admin key
  local m="$1" u="$2"; shift 2
  curl -s -X "$m" "$u" -H "Authorization: Bearer $KEY" "$@"
}
field() { # field <python expression over d>: read JSON from stdin
  python3 -c "import sys, json; d = json.load(sys.stdin); print($1)"
}
start() {
  SYNTRA_ADMIN_KEY="$KEY" "$SYNTRA" serve --addr "127.0.0.1:$PORT" --store "$STORE" \
    2>>"$WORK/server.log" &
  PID=$!
  for _ in $(seq 100); do curl -sf "$S/health" >/dev/null 2>&1 && return 0; sleep 0.05; done
  echo "server did not start; log:" >&2; cat "$WORK/server.log" >&2; exit 1
}
stop() {
  kill -TERM "$PID"; wait "$PID" 2>/dev/null || true; PID=""
}

echo "Syntra smoke test ($SYNTRA, store $STORE)"
start

echo "Server"
check "/health answers" test "$(status GET "$S/health")" = 200
check "/ready answers" test "$(status GET "$S/ready")" = 200
check "no key is 401" test "$(status GET "$S/v1/tenants")" = 401
check "a wrong key is 401" test "$(status GET "$S/v1/tenants" -H 'Authorization: Bearer nope')" = 401
check "/metrics needs a credential" test "$(status GET "$S/metrics")" = 401

echo "Capsule, decide, reward"
check "PUT spec creates the capsule (201)" test "$(status PUT "$B/spec" -H "Authorization: Bearer $KEY" \
  -d '{"actions": [{"id": "small", "features": {"cost": 0.1}}, {"id": "large", "features": {"cost": 1.0}}]}')" = 201
check "an unknown spec field is 400" test "$(status PUT "$B/spec" -H "Authorization: Bearer $KEY" -d '{"actionz": []}')" = 400
D="$(api POST "$B/decide" -d '{"context": {"task": "code"}}')"
ID="$(echo "$D" | field 'd["decisionId"]')"
check "decide returns an action and its probability" python3 -c "
import json, sys; d = json.loads(sys.argv[1]); assert d['action'] in ('small', 'large') and 0 < d['probability'] <= 1" "$D"
FIRST="{\"decisionId\": \"$ID\", \"reward\": 0.8}"
SECOND="{\"decisionId\": \"$ID\", \"reward\": 0.1}"
check "reward is applied" test "$(api POST "$B/reward" -d "$FIRST" | field 'd["applied"]')" = True
check "a second reward is a duplicate (rewards: first)" test "$(api POST "$B/reward" -d "$SECOND" | field 'd["applied"]')" = False
check "the stored decision has its PMF, seed and reward" python3 -c "
import json, sys; d = json.loads(sys.argv[1]); assert len(d['pmf']) == 2 and d['seed'] and d['rewards'][0]['reward'] == 0.8" \
  "$(api GET "$B/decisions/$ID")"
check "eventId: same body replays the decision" test \
  "$(api POST "$B/decide" -d '{"eventId": "e-1", "context": {}}' >/dev/null; api POST "$B/decide" -d '{"eventId": "e-1", "context": {}}' | field 'd.get("replayed")')" = True
check "eventId: a different body is 409" test "$(status POST "$B/decide" -H "Authorization: Bearer $KEY" -d '{"eventId": "e-1", "context": {"x": 1}}')" = 409
check "durable decide answers after the commit" test "$(api POST "$B/decide" -d '{"context": {}, "durable": true}' | field '"decisionId" in d')" = True

echo "Scoped tokens"
TOKEN="$(api POST "$S/v1/admin/tokens" -d '{"scope": {"kind": "read", "tenant": "smoke", "job": "prod", "capsule": "router"}}' | field 'd["token"]')"
check "read token can decide" test "$(status POST "$B/decide" -H "Authorization: Bearer $TOKEN" -d '{}')" = 200
check "read token cannot change the spec (403)" test "$(status PUT "$B/spec" -H "Authorization: Bearer $TOKEN" -d '{"mode": "frozen"}')" = 403
check "read token cannot reach another capsule (403)" test \
  "$(status POST "$S/v1/tenants/smoke/jobs/prod/capsules/other/decide" -H "Authorization: Bearer $TOKEN" -d '{}')" = 403
check "read token cannot evaluate (403)" test "$(status POST "$B/evaluate" -H "Authorization: Bearer $TOKEN" -d '{"policy": "logged"}')" = 403

echo "Feature program and sandbox"
cat > "$WORK/program.lycs" <<'EOF'
($ budget (!cap "runtime.inputGet" "budget"))
(? (== budget "low") (!cap "runtime.publish" "exclude.large" true) null)
(? (== budget "probe") (!cap "file.readText" "../../../../../store.json") null)
EOF
"$LYCAN" compile "$WORK/program.lycs" 2>/dev/null
check "install a compiled program" test "$(status POST "$B/install" -H "Authorization: Bearer $KEY" --data-binary @"$WORK/program.lyc")" = 200
check "the program excludes an action (probability 1)" test \
  "$(api POST "$B/decide" -d '{"context": {"budget": "low"}}' | field 'd["action"] + " " + str(d["probability"])')" = "small 1.0"
check "file access outside the policy is denied (500)" test \
  "$(status POST "$B/decide" -H "Authorization: Bearer $KEY" -d '{"context": {"budget": "probe"}}')" = 500
check "the denial is audited as execution_denied" python3 -c "
import json, sys; ev = [a['event'] for a in json.loads(sys.argv[1])['audits']]
assert 'program_installed' in ev and 'execution_denied' in ev, ev" "$(api GET "$B/audits?limit=50")"
check "remove the program" test "$(api DELETE "$B/program" | field 'd["removed"]')" = True

echo "Traffic, metrics, evaluation"
python3 - "$B" "$KEY" <<'EOF'
import json, random, sys, urllib.request
base, key = sys.argv[1], sys.argv[2]
rng = random.Random(1)
def call(path, body):
    req = urllib.request.Request(base + path, data=json.dumps(body).encode(),
                                 headers={"Authorization": "Bearer " + key})
    return json.load(urllib.request.urlopen(req))
for _ in range(300):
    task = rng.choice(["chat", "code"])
    d = call("/decide", {"context": {"task": task}})
    good = (task == "code") == (d["action"] == "large")  # simulated outcome
    call("/reward", {"decisionId": d["decisionId"], "reward": 1.0 if good else 0.2})
EOF
check "/metrics with the admin key has decide latency" bash -c \
  "curl -s '$S/metrics' -H 'Authorization: Bearer $KEY' | grep -q '^syntra_decide_seconds_count'"
check "POST evaluate returns a report" test "$(api POST "$B/evaluate" -d '{"policy": "greedy", "bootstrap": 200}' | field 'd["data"]["rows"] > 0')" = True
check "syntra evaluate --store reads the log" "$SYNTRA" evaluate --store "$STORE" --capsule smoke/prod/router --policy greedy --bootstrap 200
VERSION="$(api GET "$B/model" | field 'd["modelVersion"]')"

echo "Restart, backup, doctor"
stop
start
check "the model version survives a restart ($VERSION)" test "$(api GET "$B/model" | field 'd["modelVersion"]')" = "$VERSION"
check "decisions survive a restart" test "$(status GET "$B/decisions/$ID" -H "Authorization: Bearer $KEY")" = 200
check "syntra backup while serving" "$SYNTRA" backup --store "$STORE" --out "$WORK/backup"
check "syntra restore refuses the live store" bash -c "! '$SYNTRA' restore --from '$WORK/backup' --into '$STORE'"
check "syntra restore into a new root" "$SYNTRA" restore --from "$WORK/backup" --into "$WORK/restored"
check "syntra doctor finds nothing" "$SYNTRA" doctor --store "$STORE"

echo
echo "$PASS passed, $FAIL failed"
[[ "$FAIL" -eq 0 ]]
