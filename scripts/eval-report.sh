#!/usr/bin/env bash
#
# eval-report.sh — regenerate Syntra's dated adaptive-policy evaluation report.
#
#   bash scripts/eval-report.sh
#   REPORT_DATE=2026-09-08 ROUNDS=4000 SEEDS=12 bash scripts/eval-report.sh
#
# Everything measured here is simulated traffic. No production data exists in
# this repository, so no production number is reported.
#
# Phases
#   A  regret vs built-in baselines  (syntra simulate --compare-baseline)
#   B  convergence sweep behind the shareBestArm finding
#   C  offline-policy-evaluation round-trip (examples/offline-eval: IPS + DR)
#   D  A/B harness round-trip against a live `syntra serve`
#   E  render markdown + JSON artifact into docs/evaluations/
#
# Determinism: `syntra simulate` is seeded from --seed, and the server-side
# PRNG is pinned with LYCAN_RNG_SEED. All seeds are fixed below.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DATE="${REPORT_DATE:-$(date -u +%Y-%m-%d)}"
OUT_DIR="$ROOT/docs/evaluations"
OUT_MD="$OUT_DIR/${REPORT_DATE}-adaptive-policy-baseline.md"
OUT_JSON="$OUT_DIR/${REPORT_DATE}-adaptive-policy-baseline.json"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/syntra-eval.XXXXXX")"
SERVER_PID=""
cleanup() {
  [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

ROUNDS="${ROUNDS:-4000}"
SEEDS="${SEEDS:-12}"
BASE_SEED="${BASE_SEED:-42}"
AB_ROUNDS="${AB_ROUNDS:-120}"
AB_SEEDS="${AB_SEEDS:-2}"
OPE_ROUNDS="${OPE_ROUNDS:-4000}"
OPE_EPSILON="${OPE_EPSILON:-0.20}"
OPE_BOOTSTRAP="${OPE_BOOTSTRAP:-400}"
OPE_SEED="${OPE_SEED:-20260908}"
RNG_SEED="${RNG_SEED:-7}"

SYNTRA="$ROOT/target/release/syntra"
if [[ ! -x "$SYNTRA" ]]; then
  echo "[eval-report] building release binaries…" >&2
  (cd "$ROOT" && cargo build --release --quiet --bin syntra --bin lycan)
fi

mkdir -p "$OUT_DIR"
cd "$ROOT"

# ── Phase 0 — reproducibility probe ───────────────────────────────────────
# The same command, twice, in two separate processes. Everything the report
# quotes (regret, picks, shareBest, refusal rate, meta-bandit selections,
# regret trace) must be byte-identical; `finalWeights` is compared with a
# tolerance because the learning layer's float accumulation is not a pure
# function of the seed — it drifts at last-ulp scale between processes, which
# cannot change any decision but does break byte-level identity of that field.
DET_ARGS=(simulate evals/traffic/capsules/routing-4arm-auto.yaml
          --traffic evals/traffic/stationary.yaml
          --rounds "$ROUNDS" --seeds "$SEEDS" --seed "$BASE_SEED" --format json)
"$SYNTRA" "${DET_ARGS[@]}" > "$WORK/det_1.json"
"$SYNTRA" "${DET_ARGS[@]}" > "$WORK/det_2.json"
python3 - "$WORK/det_1.json" "$WORK/det_2.json" "$WORK/determinism.json" \
        "$SYNTRA ${DET_ARGS[*]}" <<'PY'
import copy, json, sys
a_path, b_path, out_path, cmd = sys.argv[1:5]
A, B = json.load(open(a_path)), json.load(open(b_path))
weights = [[s.pop("finalWeights") for s in d["seeds"]] for d in (A, B)]
max_dw = max(abs(x - y) for ra, rb in zip(*weights) for x, y in zip(ra, rb))
identical = json.dumps(A, sort_keys=True) == json.dumps(B, sort_keys=True)
if not identical:
    sys.stderr.write("[eval-report] FATAL: two identical simulate runs "
                     "disagreed on a reported field\n")
    sys.exit(1)
json.dump({"identical": identical, "maxFinalWeightDrift": max_dw,
           "meanCumulativeRegret": json.load(open(a_path))["meanCumulativeRegret"],
           "command": cmd},
          open(out_path, "w"), indent=2)
PY

# ── Phase A — regret vs built-in baselines ────────────────────────────────
# Each (capsule, traffic) case is scored against each built-in comparator in a
# separate process, so a reader can re-run any single row.
CASES=(
  "evals/traffic/capsules/routing-4arm-auto.yaml|evals/traffic/stationary.yaml|simpleWeighted (auto)"
  "evals/traffic/capsules/routing-4arm-thompson.yaml|evals/traffic/stationary.yaml|thompson"
  "evals/traffic/capsules/routing-4arm-ucb.yaml|evals/traffic/stationary.yaml|ucb1"
  "evals/traffic/capsules/routing-4arm-auto.yaml|evals/traffic/regime-shift.yaml|simpleWeighted (auto)"
  "evals/traffic/capsules/routing-4arm-thompson.yaml|evals/traffic/regime-shift.yaml|thompson"
  "evals/traffic/capsules/routing-4arm-ucb.yaml|evals/traffic/regime-shift.yaml|ucb1"
  "evals/traffic/capsules/routing-4arm-sparse.yaml|evals/traffic/sparse-reward.yaml|ucb1 (auto, sparse_continuous)"
)
BASELINES=(random first-arm epsilon-greedy:0.1)

: > "$WORK/phase_a.jsonl"
for spec in "${CASES[@]}"; do
  IFS='|' read -r capsule traffic label <<< "$spec"
  slug="$(basename "$capsule" .yaml)__$(basename "$traffic" .yaml)"
  for bl in "${BASELINES[@]}"; do
    cmd=("$SYNTRA" simulate "$capsule" --traffic "$traffic"
         --rounds "$ROUNDS" --seeds "$SEEDS" --seed "$BASE_SEED"
         --compare-baseline "$bl" --format json)
    "${cmd[@]}" > "$WORK/a_case.json"
    jq -c --arg label "$label" --arg capsule "$capsule" --arg traffic "$traffic" \
       --arg bl "$bl" --arg cmd "${cmd[*]}" \
       --argjson rounds "$ROUNDS" --argjson seeds "$SEEDS" \
       '{config: $label, capsule: $capsule, traffic: $traffic, baseline: $bl,
         algorithm: .seeds[0].algorithm, rounds: $rounds, seeds: $seeds,
         regretMean: .meanCumulativeRegret, regretStd: .stdCumulativeRegret,
         baselineMean: .baselineComparison.meanCumulativeRegret,
         baselineStd: .baselineComparison.stdCumulativeRegret,
         perSeedRegret: [.seeds[].cumulativeRegret],
         perSeedBaseline: [.baselineComparison.perSeed[].cumulativeRegret],
         refusalRate: .meanRefusalRate,
         shareBestMean: ([.seeds[].shareBestArmLast500] | add / length),
         perContextConvergence: [.seeds[].perContextConvergence[] | {context, shareBest}],
         metaSelections: [.seeds[].metaBanditSelections[]],
         metaLeaders: [.seeds[].metaBanditLeader],
         command: $cmd}' "$WORK/a_case.json" >> "$WORK/phase_a.jsonl"
  done
done
jq -s '{runs: .}' "$WORK/phase_a.jsonl" > "$WORK/phase_a.json"

# ── Phase B — convergence sweep behind the shareBestArm finding ───────────
: > "$WORK/phase_b.jsonl"
SWEEPS=(
  "evals/traffic/capsules/routing-4arm-weighted-minex00.yaml|0.0|weighted"
  "evals/traffic/capsules/routing-4arm-auto.yaml|0.05|simpleWeighted (auto)"
  "evals/traffic/capsules/routing-4arm-weighted-minex02.yaml|0.2|weighted"
  "evals/traffic/capsules/routing-4arm-weighted-minex04.yaml|0.4|weighted"
  "evals/traffic/capsules/routing-4arm-thompson.yaml|0.05|thompson"
  "evals/traffic/capsules/routing-4arm-ucb.yaml|0.05|ucb1"
)
for spec in "${SWEEPS[@]}"; do
  IFS='|' read -r capsule minex label <<< "$spec"
  for R in 500 2000 8000 20000; do
    cmd=("$SYNTRA" simulate "$capsule" --traffic evals/traffic/stationary.yaml
         --rounds "$R" --seeds "$SEEDS" --seed "$BASE_SEED" --format json)
    "${cmd[@]}" > "$WORK/b_case.json"
    jq -c --arg label "$label" --arg capsule "$capsule" --arg minex "$minex" \
       --arg cmd "${cmd[*]}" --argjson rounds "$R" --argjson seeds "$SEEDS" \
       '{config: $label, capsule: $capsule, algorithm: .seeds[0].algorithm,
         minExploration: ($minex | tonumber), rounds: $rounds, seeds: $seeds,
         regretMean: .meanCumulativeRegret,
         regretPerRound: (.meanCumulativeRegret / $rounds),
         shareBestMean: ([.seeds[].shareBestArmLast500] | add / length),
         shareBestMin: ([.seeds[].shareBestArmLast500] | min),
         shareBestMax: ([.seeds[].shareBestArmLast500] | max),
         weights: .seeds[0].finalWeights,
         command: $cmd}' "$WORK/b_case.json" >> "$WORK/phase_b.jsonl"
  done
done
jq -s '{sweep: .}' "$WORK/phase_b.jsonl" > "$WORK/phase_b.json"

# ── Phase C — OPE round-trip (IPS + doubly robust) ────────────────────────
OPE_CSV="$WORK/ope_log.csv"
OPE_POLICY="$WORK/ope_policy.json"
OPE_META="$WORK/ope_meta.json"

# Behaviour log: seeded ε-greedy over the same arm means the simulator uses.
# Propensities are exact by construction (ε/n off-greedy, 1-ε+ε/n greedy),
# which is precisely what IPS/DR need in order to be unbiased.
python3 - "$SYNTRA" "$OPE_ROUNDS" "$OPE_EPSILON" "$OPE_SEED" \
    "$OPE_CSV" "$OPE_POLICY" "$OPE_META" <<'PY'
import csv, json, random, re, sys

syntra, rounds_s, eps_s, seed_s, csv_path, policy_path, meta_path = sys.argv[1:8]
rounds, seed, eps = int(rounds_s), int(seed_s), float(eps_s)
CAPSULE = "evals/traffic/capsules/routing-4arm-auto.yaml"
TRAFFIC = "evals/traffic/stationary.yaml"


def inline_or_block(path, key):
    text = open(path).read()
    block = re.search(rf"^{key}:\n((?:\s+-\s+\S+\n?)+)", text, re.M)
    if block:
        return [ln.strip()[1:].strip() for ln in block.group(1).strip().splitlines()]
    inline = re.search(rf"^{key}:\s*\[([^\]]*)\]", text, re.M)
    return [t.strip() for t in inline.group(1).split(",")] if inline else []


names = inline_or_block(CAPSULE, "options")
means = [float(x) for x in inline_or_block(TRAFFIC, "arms")]
ttxt = open(TRAFFIC).read()
cv = re.search(r"^\s+values:\s*\[([^\]]*)\]", ttxt, re.M)
contexts = [v.strip() for v in cv.group(1).split(",")] if cv else ["sim"]
cwd = re.search(r"^\s+weights:\s*\[([^\]]*)\]", ttxt, re.M)
cw = [float(x) for x in cwd.group(1).split(",")] if cwd else [1.0] * len(contexts)
noise = float(re.search(r"^noise_std:\s*([0-9.eE+-]+)", ttxt, re.M).group(1))
assert len(names) == len(means) == 4, (names, means)

rng = random.Random(seed)
n = len(names)
best = max(range(n), key=lambda i: means[i])
counts, sums = [0] * n, [0.0] * n
rows = []
ips_correct = ips_naive = 0.0
naive_understated = 0
for t in range(rounds):
    ctx = rng.choices(contexts, weights=cw, k=1)[0]
    greedy = max(range(n), key=lambda i: (sums[i] / counts[i]) if counts[i] else float("inf"))
    explored = rng.random() < eps
    arm = rng.randrange(n) if explored else greedy
    # The propensity column must carry the action's MARGINAL probability under
    # the behaviour policy, not the probability of the branch that produced it.
    # An exploration draw that happens to land on the greedy arm still has
    # total probability (1-eps) + eps/n; logging eps/n there inflates the IPS
    # weight of exactly those rows and biases the estimate upward. The naive
    # variant is tracked below so the distortion is measured, not asserted.
    prop = ((1.0 - eps) + eps / n) if arm == greedy else eps / n
    naive_prop = ((1.0 - eps) if not explored else eps / n)
    obs = max(-1.0, min(1.0, rng.gauss(means[arm], noise)))
    counts[arm] += 1
    sums[arm] += obs
    if arm == best:
        ips_correct += obs / prop
        ips_naive += obs / naive_prop
        naive_understated += 1 if arm == greedy and explored else 0
    rows.append({"decision_id": f"dec_{t:06d}", "context_key": ctx,
                 "action": names[arm], "propensity": round(prop, 6),
                 "reward": round(obs, 6)})

with open(csv_path, "w", newline="") as fh:
    writer = csv.DictWriter(fh, fieldnames=["decision_id", "context_key",
                                            "action", "propensity", "reward"])
    writer.writeheader()
    writer.writerows(rows)

json.dump({c: names[best] for c in contexts}, open(policy_path, "w"), indent=2)
json.dump({
    "rows": len(rows), "epsilon": eps, "seed": seed, "noiseStd": noise,
    "armNames": names, "trueMeans": dict(zip(names, means)),
    "contexts": contexts, "contextWeights": cw,
    "targetPolicy": {c: names[best] for c in contexts},
    "targetPolicyTrueValue": means[best],
    # ε-greedy value once the empirical argmax has settled on the true best arm.
    "behaviourPolicyTrueValue": (1 - eps) * means[best] + eps * sum(means) / n,
    "behaviourPicks": {names[i]: counts[i] for i in range(n)},
    # Self-check: the same log scored with the marginal propensity (what the
    # CSV carries) versus the branch-conditional propensity that a careless
    # logger writes. The gap is measured, not asserted.
    "selfCheckIpsMarginalPropensity": round(ips_correct / rounds, 6),
    "selfCheckIpsConditionalPropensity": round(ips_naive / rounds, 6),
    "selfCheckRowsWithUnderstatedPropensity": naive_understated,
    "command": (f"python3 scripts/eval-report.sh   # phase C generates this log "
                f"in-process with seed {seed}"),
}, open(meta_path, "w"), indent=2)
PY

OPE_STATIC="$WORK/ope_static.json"
python3 examples/offline-eval/evaluate.py "$OPE_CSV" \
    --mode static --policy-json "$OPE_POLICY" \
    --bootstrap "$OPE_BOOTSTRAP" --bootstrap-seed 42 --format json > "$OPE_STATIC"

# ── Phase D — A/B harness against a live server ───────────────────────────
PORT=$((9400 + RANDOM % 400))
ADDR="127.0.0.1:$PORT"
KEY="eval-key"
mkdir -p "$WORK/store"
LYCAN_RNG_SEED="$RNG_SEED" "$SYNTRA" serve --addr "$ADDR" --store "$WORK/store" \
    --admin-key "$KEY" > "$WORK/serve.log" 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 80); do
  curl -sf -H "Authorization: Bearer $KEY" "http://$ADDR/health" > /dev/null 2>&1 && break
  sleep 0.1
done

AB_DIR="$WORK/ab"
if PATH="$ROOT/target/release:$PATH" python3 examples/ab-harness/ab_harness.py \
    examples/ab-harness/example_capsule_a.yaml \
    examples/ab-harness/example_capsule_b.yaml \
    examples/ab-harness/example_traffic.yaml \
    --rounds "$AB_ROUNDS" --seeds "$AB_SEEDS" --seed-offset 1000 \
    --syntra-url "http://$ADDR" --admin-key "$KEY" \
    --tenant eval --job ab --output-dir "$AB_DIR" \
    > "$WORK/ab.stdout" 2> "$WORK/ab.stderr"; then
  AB_STATUS=ok
else
  AB_STATUS=failed
fi
kill "$SERVER_PID" 2>/dev/null || true
wait "$SERVER_PID" 2>/dev/null || true
SERVER_PID=""
if [[ "$AB_STATUS" != ok ]]; then
  echo "[eval-report] ab-harness failed:" >&2
  tail -20 "$WORK/ab.stderr" >&2 || true
  exit 1
fi

# ── Phase E — render ──────────────────────────────────────────────────────
HOST_INFO="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo 'unknown CPU')"
OS_INFO="$(uname -sr)"
COMMIT="$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo 'unknown')"

export REPORT_DATE OUT_MD OUT_JSON WORK HOST_INFO OS_INFO COMMIT \
       ROUNDS SEEDS BASE_SEED AB_ROUNDS AB_SEEDS OPE_ROUNDS OPE_EPSILON \
       OPE_BOOTSTRAP OPE_SEED RNG_SEED
python3 "$ROOT/scripts/eval-report-render.py"

echo "[eval-report] done" >&2
