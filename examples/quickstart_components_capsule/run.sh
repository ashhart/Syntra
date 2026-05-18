#!/usr/bin/env bash
# End-to-end demo of the components-form reward flow.
#
# Author a YAML capsule, start syntra serve, install the .lyc and reward_spec,
# run a few decide+feedback rounds, fetch the final report.
#
# Requires: syntra binary on PATH (cargo build, then run from this dir, or
# point SYNTRA at the binary path) and `curl`. Optional: jq for pretty output.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SYNTRA="${SYNTRA:-$HERE/../../target/debug/syntra}"
ADDR="${ADDR:-127.0.0.1:18799}"
ADMIN_KEY="${ADMIN_KEY:-quickstart-key}"
WORK="${WORK:-$(mktemp -d -t syntra-quickstart-XXXXXX)}"
STORE="$WORK/store"
CAPSULE_DIR="$WORK/capsule"

TENANT="${TENANT:-acme}"
JOB="${JOB:-default}"
CAPSULE="${CAPSULE:-router}"

echo "syntra:     $SYNTRA"
echo "addr:       $ADDR"
echo "work dir:   $WORK"
echo

if [[ ! -x "$SYNTRA" ]]; then
  echo "syntra binary not found at $SYNTRA"
  echo "build with: (cd $HERE/../.. && cargo build)"
  exit 1
fi

echo "[1/6] author capsule from spec.yaml"
"$SYNTRA" author "$HERE/spec.yaml" --out-dir "$CAPSULE_DIR" >/dev/null
echo "      wrote: $(ls "$CAPSULE_DIR")"
echo

echo "[2/6] start syntra serve in the background"
mkdir -p "$STORE"
"$SYNTRA" serve --addr "$ADDR" --store "$STORE" --admin-key "$ADMIN_KEY" \
  > "$WORK/syntra.log" 2>&1 &
SYNTRA_PID=$!
trap 'kill $SYNTRA_PID 2>/dev/null || true; rm -rf "$WORK"' EXIT

for _ in $(seq 1 50); do
  if curl -sf "http://$ADDR/health" >/dev/null; then
    echo "      server up (pid=$SYNTRA_PID)"
    break
  fi
  sleep 0.1
done

BASE="http://$ADDR/tenants/$TENANT/jobs/$JOB/capsules/$CAPSULE"
AUTH=(-H "Authorization: Bearer $ADMIN_KEY")

echo
echo "[3/6] install program.lyc"
curl -sf "${AUTH[@]}" \
  -X POST "$BASE/install" \
  --data-binary "@$CAPSULE_DIR/program.lyc" \
  -H "Content-Type: application/octet-stream" | head -c 200
echo

echo
echo "[4/6] install reward_spec.json"
curl -sf "${AUTH[@]}" \
  -X PUT "$BASE/reward_spec" \
  --data-binary "@$CAPSULE_DIR/reward_spec.json" \
  -H "Content-Type: application/json" | head -c 200
echo

echo
echo "[5/6] decide + feedback with components form (5 rounds)"
for i in 1 2 3 4 5; do
  DECIDE=$(curl -sf "${AUTH[@]}" -X POST "$BASE/decide" \
    -H "Content-Type: application/json" \
    -d '{"inputs":{"task_type":"summary","customer_tier":"gold"}}')
  DID=$(printf '%s' "$DECIDE" | sed -n 's/.*"decisionId":"\([^"]*\)".*/\1/p')
  CHOSEN=$(printf '%s' "$DECIDE" | sed -n 's/.*"chosen_option":\([0-9]*\).*/\1/p')

  # Simulated outcome: a slightly noisy good outcome.
  QUALITY=$(awk -v s=$i 'BEGIN{srand(s+11); printf "%.3f", 0.7 + 0.2*rand()}')
  LATENCY=$(awk -v s=$i 'BEGIN{srand(s+33); printf "%.0f", 800 + 600*rand()}')
  COST=$(awk -v s=$i 'BEGIN{srand(s+55); printf "%.4f", 0.005 + 0.02*rand()}')

  FB=$(curl -sf "${AUTH[@]}" -X POST "$BASE/feedback" \
    -H "Content-Type: application/json" \
    -d "{\"decisionId\":\"$DID\",\"components\":{\"quality\":$QUALITY,\"latency_ms\":$LATENCY,\"cost_usd\":$COST}}")
  REWARD=$(printf '%s' "$FB" | sed -n 's/.*"reward":\([-0-9.]*\).*/\1/p')
  printf "  round %d  chose=%s  quality=%s  latency=%s  cost=%s  reward=%s\n" \
    "$i" "$CHOSEN" "$QUALITY" "$LATENCY" "$COST" "$REWARD"
done

echo
echo "[6/6] final report"
curl -sf "${AUTH[@]}" "$BASE/report" | head -c 400
echo
echo
echo "done. Logs at $WORK/syntra.log"
