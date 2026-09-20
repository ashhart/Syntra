#!/usr/bin/env bash
# SYNTRA DEMO — one runtime, five proofs.
#
# Proof 1  DERIVES PHYSICS: the runtime rediscovers the Feigenbaum
#          constant and the edge of chaos from the dynamics themselves.
# Proof 2  RUNS A MISSION: live NASA/JPL HORIZONS ephemeris -> Lambert
#          solver -> transfer-window decision under a C3 constraint ->
#          delayed feedback. Falls back to an embedded Keplerian
#          ephemeris when offline.
# Proof 3  RUNS THE ECONOMICS: governed LLM routing — train, shadow,
#          replay against promotion gates — then a live measurement of
#          what a decision actually costs on this machine.
# Proof 4  RUNS THE TRIAL: a response-adaptive clinical trial. Every
#          patient is a decision, every outcome is delayed feedback, the
#          learned per-subgroup weights ARE the randomization schedule.
# Proof 5  REFUSES TO LIE: Erdos #160 is open. It computes what is
#          computable and files the rest as an obligation it will not
#          claim.
# Live   A dashboard: watch the policy learn from delayed feedback only,
#        then flip the ground truth and watch it re-adapt.
#
# Every claim is measured at runtime and written to an append-only
# decision log. The receipt block at the end hashes those logs.
#
# Usage:  scripts/demo.sh            (ends with the live dashboard)
#         scripts/demo.sh --no-live  (headless, CI-safe)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
SYNTRA="$ROOT/target/release/syntra"
[[ -x "$LYCAN" && -x "$SYNTRA" ]] || (cd "$ROOT" && cargo build --release --quiet)

LIVE=1
[[ "${1:-}" == "--no-live" ]] && LIVE=0

WORK="$(mktemp -d "${TMPDIR:-/tmp}/syntra-demo.XXXXXX")"
STORE="$WORK/store"
KEY="demo-key"
PORT=$((9600 + RANDOM % 300))
ADDR="127.0.0.1:$PORT"
DASH_PORT=$((8900 + RANDOM % 90))
SRV=""
DASH=""
T0=$(date +%s)

# Deterministic server-side sampling: every decision, allocation, and
# headline in this demo reproduces run to run. (See src/server/mod.rs.)
export LYCAN_RNG_SEED=7

cleanup() {
  [[ -n "$DASH" ]] && kill "$DASH" 2>/dev/null || true
  [[ -n "$SRV" ]] && kill "$SRV" 2>/dev/null || true
  wait "$SRV" 2>/dev/null || true
  wait "$DASH" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

AUTH=(-H "Authorization: Bearer $KEY")
JSON=(-H "Authorization: Bearer $KEY" -H "Content-Type: application/json")

hr() { printf '%.0s─' $(seq 1 74); echo; }

echo
echo "╔══════════════════════════════════════════════════════════════════╗"
echo "║   SYNTRA DEMO — one runtime, five proofs                         ║"
echo "╚══════════════════════════════════════════════════════════════════╝"
echo
echo "  No LLM in the loop; these demos execute the Lycan runtime."
echo "  Mars uses live ephemerides when available; routing and trial"
echo "  outcomes are synthetic fixtures, not measured customer results."
echo "  The receipt block hashes the API decision logs from this run."

# ─────────────────────────────────────────────────────────────────────
echo
hr
echo "PROOF 1 — DERIVES PHYSICS"
hr
echo "A decision runtime derives the Feigenbaum constant (δ ≈ 4.669) and"
echo "the edge of chaos (r ≈ 3.56995) from the dynamics themselves."
echo "It was not told the answer. No lookup table. No LLM. 30 lines of Lycan."
echo
"$LYCAN" "$ROOT/examples/lycan-internals/demo_edge_of_chaos.lycs" \
  | grep -E "Feigenbaum|Lyapunov|Trajectory|Known value|Error" \
  | sed 's/^/  /'

# ─────────────────────────────────────────────────────────────────────
echo
hr
echo "PROOF 2 — RUNS A MISSION (LIVE NASA/JPL HORIZONS)"
hr
echo "Live ephemeris -> Lambert solver -> transfer-window decision under"
echo "a C3 constraint -> delayed feedback -> the persisted policy improves."
echo

"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$KEY" >/dev/null 2>&1 &
SRV=$!
for _ in $(seq 1 20); do
  curl -sf -o /dev/null "http://$ADDR/health" && break
  sleep 0.3
done

curl -sf -X POST "${JSON[@]}" \
  -d '{"id":"mission-control","name":"Mission Control"}' \
  "http://$ADDR/tenants/demo/jobs" >/dev/null

"$LYCAN" compile "$ROOT/examples/lycan-internals/demo_mars_horizons_api.lycs" >/dev/null 2>&1
curl -sf -X POST "${AUTH[@]}" \
  --data-binary "@$ROOT/examples/lycan-internals/demo_mars_horizons_api.lyc" \
  "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/install" >/dev/null
curl -sf -X PUT "${JSON[@]}" \
  -d '{"allow_stdout":true,"allow_stdin":false,"allow_file_read":false,"allow_file_write":false,"allow_network":true,"allowed_hosts":["ssd.jpl.nasa.gov"],"deny_private_networks":true}' \
  "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/policy" >/dev/null

MARS_BODY='{
  "horizons": {"start": "2026-Jan-01", "stop": "2028-Jan-01", "step_days": 5.0},
  "max_c3": 12.0, "min_tof": 220.0, "max_tof": 330.0,
  "search_window_days": 500.0, "objective": "minimize_c3"
}'
RESP="$(curl -sf -X POST "${JSON[@]}" -d "$MARS_BODY" \
  "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/decide" 2>/dev/null || true)"
LIVE_OK="$(printf '%s' "$RESP" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    s = "\n".join(map(str, d.get("stdout", [])))
    print("true" if d.get("ok") and "Live NASA/JPL Horizons API" in s and "C3:" in s else "false")
except Exception:
    print("false")')"

if [[ "$LIVE_OK" == "true" ]]; then
  echo "  LIVE: fetched real Earth + Mars ephemerides from ssd.jpl.nasa.gov"
  printf '%s' "$RESP" | python3 -c '
import json, sys
d = json.load(sys.stdin)
for line in "\n".join(map(str, d.get("stdout", []))).splitlines():
    if any(t in line for t in ["Earth records:", "Mars records:", "Date:", "TOF:", "C3:", "PASS"]):
        print("  " + line)'
else
  echo "  OFFLINE: no HORIZONS access — falling back to embedded Keplerian ephemeris"
  "$LYCAN" compile "$ROOT/examples/lycan-internals/demo_mars_transfer.lycs" >/dev/null 2>&1
  curl -sf -X POST "${AUTH[@]}" \
    --data-binary "@$ROOT/examples/lycan-internals/demo_mars_transfer.lyc" \
    "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/install" >/dev/null
  curl -sf -X PUT "${JSON[@]}" \
    -d '{"allow_stdout":true,"allow_stdin":false,"allow_file_read":false,"allow_file_write":false,"allow_network":false}' \
    "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/policy" >/dev/null
  RESP="$(curl -sf -X POST "${JSON[@]}" -d '{}' \
    "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/decide")"
  printf '%s' "$RESP" | python3 -c '
import json, sys
d = json.load(sys.stdin)
for line in "\n".join(map(str, d.get("stdout", []))).splitlines():
    if any(t in line for t in ["window", "TOF", "C3", "PASS"]):
        print("  " + line)'
fi

DECISION_ID="$(printf '%s' "$RESP" | python3 -c 'import json,sys; print(json.load(sys.stdin)["decisionId"])')"
BEFORE="$(printf '%s' "$RESP" | python3 -c 'import json,sys; print(max(json.load(sys.stdin)["decisions"][0]["weights"]))')"
for _ in $(seq 1 8); do
  curl -sf -X POST "${JSON[@]}" \
    -d "{\"decisionId\":\"$DECISION_ID\",\"reward\":1.0}" \
    "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/feedback" >/dev/null
done
AFTER="$(curl -sf "${AUTH[@]}" \
  "http://$ADDR/tenants/demo/jobs/mission-control/capsules/mars/report" \
  | python3 -c 'import json,sys; print(max(o["weight"] for o in json.load(sys.stdin)["strategies"][0]["options"]))')"
if python3 -c "exit(0 if float('$AFTER') > float('$BEFORE') else 1)"; then
  echo "  FEEDBACK: winning strategy confidence $BEFORE -> $AFTER after 8 rewards"
else
  echo "  FEEDBACK: confidence $BEFORE -> $AFTER (no change — see report)"
fi

# ─────────────────────────────────────────────────────────────────────
echo
hr
echo "PROOF 3 — RUNS THE ECONOMICS (GOVERNED LLM ROUTING)"
hr
echo "Train the router, shadow it beside the incumbent, replay the shadow"
echo "log against promotion gates, promote only if every gate passes."
echo "Then measure what a decision costs on this machine."
echo
"$ROOT/examples/demo-governed-llm-routing.sh" 2>&1 \
  | grep -E "Status:|Reward uplift|Cost increase|p95 latency|Oracle match|Recommendation" \
  | sed 's/^/  /'

echo
echo "  BAKE-OFF — the same router, live on this machine:"
"$LYCAN" compile "$ROOT/examples/llm-routing/demo_llm_model_router.lycs" >/dev/null 2>&1 \
  || true
ROUTER_LYC="$ROOT/examples/demo_llm_model_router.lyc"
curl -sf -X POST "${JSON[@]}" \
  -d '{"id":"llm-routing","name":"LLM Routing"}' \
  "http://$ADDR/tenants/demo/jobs" >/dev/null
curl -sf -X POST "${AUTH[@]}" --data-binary "@$ROUTER_LYC" \
  "http://$ADDR/tenants/demo/jobs/llm-routing/capsules/model-router/install" >/dev/null

python3 - "$ADDR" "$KEY" <<'PY' | sed 's/^/  /'
import json, sys, time, urllib.request

api, key = sys.argv[1], sys.argv[2]
base = f"http://{api}/tenants/demo/jobs/llm-routing/capsules/model-router"

def post(path, body):
    req = urllib.request.Request(
        base + path, json.dumps(body).encode(),
        {"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
        method="POST")
    t0 = time.perf_counter()
    with urllib.request.urlopen(req, timeout=15) as r:
        return time.perf_counter() - t0, json.load(r)

SUPPORT = {"task_type": "support", "customer_tier": "standard",
           "urgency": "normal", "tokens": 900}
LEGAL = {"task_type": "legal", "customer_tier": "enterprise",
         "urgency": "normal", "tokens": 12000}

# Teach two opposite winners, one per context.
_, first = post("/decide", {"contextKey": "support-low-cost", "input": SUPPORT})
node_id = first["decisions"][0]["node_id"]
for _ in range(30):
    post("/feedback", {"strategyId": node_id, "option": 0,
                       "reward": 1.0, "contextKey": "support-low-cost"})
for _ in range(30):
    post("/feedback", {"strategyId": node_id, "option": 2,
                       "reward": 1.0, "contextKey": "legal-high-accuracy"})

winners = {}
for ctx, inp in [("support-low-cost", SUPPORT), ("legal-high-accuracy", LEGAL)]:
    _, d = post("/decide", {"contextKey": ctx, "input": inp})
    winners[ctx] = d["decisions"][0]["chosen_option"]

# 200 decisions, timed individually.
lat = []
for i in range(200):
    ctx, inp = ("support-low-cost", SUPPORT) if i % 2 else ("legal-high-accuracy", LEGAL)
    dt, _ = post("/decide", {"contextKey": ctx, "input": inp})
    lat.append(dt * 1000.0)

lat.sort()
def pct(p):
    return lat[min(len(lat) - 1, int(p / 100.0 * len(lat)))]

names = ["cheap_fast", "balanced", "expensive_accurate"]
print("trained winners (from delayed feedback only):")
for ctx, opt in winners.items():
    print(f"  {ctx:<22} -> {names[opt]}")
print(f"200 decisions measured client-side, this run, this machine:")
print(f"  p50 {pct(50):.2f} ms   p95 {pct(95):.2f} ms   p99 {pct(99):.2f} ms")
print("  tokens consumed: 0.  cost per decision: $0.0000 (self-hosted).")
print("  the same decision via an external LLM costs tokens, seconds,")
print("  and an availability dependency — verify against current prices.")
PY

# ─────────────────────────────────────────────────────────────────────
echo
hr
echo "PROOF 4 — RUNS THE TRIAL (RESPONSE-ADAPTIVE ALLOCATION)"
hr
echo "An adaptive clinical trial. Every enrolled patient is a /decide,"
echo "every outcome is delayed /feedback, and the learned per-subgroup"
echo "weights ARE the randomization schedule. A fixed 1:1:1 control runs"
echo "in parallel on the same hidden response rates. Seed fixed; rerun"
echo "this and the numbers reproduce."
echo
"$LYCAN" compile "$ROOT/examples/lycan-internals/demo_adaptive_trial.lycs" >/dev/null 2>&1
curl -sf -X POST "${JSON[@]}" \
  -d '{"id":"trial","name":"Adaptive Trial"}' \
  "http://$ADDR/tenants/demo/jobs" >/dev/null
curl -sf -X POST "${AUTH[@]}" --data-binary "@$ROOT/examples/lycan-internals/demo_adaptive_trial.lyc" \
  "http://$ADDR/tenants/demo/jobs/trial/capsules/trial/install" >/dev/null
curl -sf -X PUT "${JSON[@]}" \
  -d '{"allow_stdout":true,"allow_stdin":false,"allow_file_read":false,"allow_file_write":false,"allow_network":false}' \
  "http://$ADDR/tenants/demo/jobs/trial/capsules/trial/policy" >/dev/null

python3 "$ROOT/scripts/demo-trial.py" \
  --api "http://$ADDR" --key "$KEY" \
  --tenant demo --job trial --capsule trial \
  --headless --patients 240 --seed 7 2>&1 \
  | grep -E "winner|WINNER|identified|responses:|responded|alloc|enrolled|subgroup|=====" \
  | sed 's/^/  /'

# ─────────────────────────────────────────────────────────────────────
echo
hr
echo "PROOF 5 — REFUSES TO LIE (ERDOS #160 IS OPEN)"
hr
echo "Ask it to solve an open math problem. It computes what is"
echo "computable, then refuses to overclaim the asymptotic."
echo
"$SYNTRA" proof-lab erdos160 2>/dev/null \
  | python3 -c '
import json, sys
d = json.load(sys.stdin)
print("  computed:", d.get("summary", "?"))
for f in d.get("finite_search", [])[-3:]:
    print("  " + f.get("status", "?"))
for w in d.get("warnings", [])[-1:]:
    print("  refuses:", w[:110] + "…")'

# ─────────────────────────────────────────────────────────────────────
echo
hr
echo "RECEIPTS"
hr
python3 - "$STORE" <<'PY' | sed 's/^/  /'
import hashlib, pathlib, sys

store = pathlib.Path(sys.argv[1])
dlogs = sorted(store.rglob("decision.jsonl"))
flogs = sorted(store.rglob("feedback.jsonl"))
decisions = sum(sum(1 for _ in p.open()) for p in dlogs)
feedbacks = sum(sum(1 for _ in p.open()) for p in flogs)
h = hashlib.sha256()
for p in dlogs:
    h.update(p.read_bytes())
print(f"capsules that decided:  {len(dlogs)}")
print(f"decision log entries:   {decisions}")
print(f"feedback log entries:   {feedbacks}")
print(f"decision-log fingerprint (sha256, append-only, replayable):")
print(f"  {h.hexdigest()}")
PY

# ─────────────────────────────────────────────────────────────────────
if [[ "$LIVE" -eq 1 ]]; then
  echo
  hr
  echo "LIVE — WATCH THE POLICY LEARN"
  hr
  "$LYCAN" compile "$ROOT/examples/lycan-internals/demo_live_bandit.lycs" >/dev/null 2>&1
  curl -sf -X POST "${AUTH[@]}" \
    --data-binary "@$ROOT/examples/lycan-internals/demo_live_bandit.lyc" \
    "http://$ADDR/tenants/demo/jobs/live/capsules/bandit/install" >/dev/null
  curl -sf -X PUT "${JSON[@]}" \
    -d '{"allow_stdout":true,"allow_stdin":false,"allow_file_read":false,"allow_file_write":false,"allow_network":false}' \
    "http://$ADDR/tenants/demo/jobs/live/capsules/bandit/policy" >/dev/null

  python3 "$ROOT/scripts/demo-live.py" \
    --api "http://$ADDR" --key "$KEY" \
    --tenant demo --job live --capsule bandit \
    --port "$DASH_PORT" >/dev/null 2>&1 &
  DASH=$!
  sleep 1
  echo "  dashboard: http://127.0.0.1:$DASH_PORT"
  if command -v open >/dev/null 2>&1; then
    open "http://127.0.0.1:$DASH_PORT" || true
  fi
  echo "  Three arms. The runtime gets delayed feedback only — it never"
  echo "  sees the ground truth. Watch the weights converge, then click"
  echo "  FLIP BEST ARM and watch the policy pivot."
  echo
  read -r -p "  Press Enter to stop the demo… " _
fi

# ─────────────────────────────────────────────────────────────────────
T1=$(date +%s)
echo
hr
echo "DEMO COMPLETE — $((T1 - T0))s"
hr
echo "  Proof 1   edge of chaos derived, not hardcoded"
echo "  Proof 2   Mars decision from $( [[ "$LIVE_OK" == "true" ]] && echo "live NASA/JPL HORIZONS" || echo "embedded ephemeris" )"
echo "  Proof 3   governed routing + 200 decisions measured this run"
echo "  Proof 4   adaptive trial: allocation learned from outcomes"
echo "  Proof 5   open problem: computed, then refused to overclaim"
if [[ "$LIVE" -eq 1 ]]; then
  echo "  Live      policy learned under delayed feedback"
fi
echo
echo "  Want the trial as its own live dashboard:"
echo "    python3 scripts/demo-trial.py --api http://$ADDR --key $KEY"
