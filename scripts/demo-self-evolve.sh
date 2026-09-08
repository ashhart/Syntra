#!/usr/bin/env bash
# Self-evolution under a compromised proposer.
#
# A classifier policy learns from delayed feedback until it plateaus. An
# improvement brief is emitted, a python proposer proposes better arms, and
# `lycan evolve` grafts them — but only through the gate: compile, purity,
# consistency, expected-output correctness, benchmark, improvement threshold.
# Then a compromised proposer tries to steer the runtime four ways; every
# attempt prints the gate's real rejection string and the binary's checksum
# proves the runtime never moved.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

BASE="$(mktemp -d "${TMPDIR:-/tmp}/syntra-evolve.XXXXXX")"
LYC="$BASE/policy.lyc"
PROBE="$BASE/probe.txt"
PASS=0; FAIL=0
check() { if [ "$1" = "true" ]; then PASS=$((PASS+1)); echo "  PASS: $2"; else FAIL=$((FAIL+1)); echo "  FAIL: $2"; fi; }
sha() { shasum -a 256 "$1" | awk '{print $1}'; }
cleanup() { if [[ -n "${KEEP_DEMO_OUTPUT:-}" ]]; then echo "output kept in $BASE"; else rm -rf "$BASE"; fi; }
trap cleanup EXIT

echo "SELF-EVOLVE: a policy that improves itself, with a compromised proposer"
echo "======================================================================"
echo "     provably unable to move it.  workdir: $BASE"
echo

# --------------------------------------------------------------------------
# The program under evolution. Three decision arms, all weak heuristics for
# the hidden rule target(n) = n+1 for n<50, 2n for n>=50. `n-1` is a bad
# default arm that is never right. The driver below IS the world: it scores
# every arm's decision rule against ground truth each round (full-information
# feedback) — the policy's choice itself always comes from its learned
# weights, never from the driver.
# --------------------------------------------------------------------------
cat > "$BASE/policy.lycs" <<'EOF'
;; traffic classifier — pick the right action value for request size n
($ raw (!cap "runtime.inputGet" "n"))
($ n (? (!= raw null) raw 0))
($ guess (choice (+ n 1) (* n 2) (- n 1)))
guess
EOF

echo "== 1. BASELINE — drive the weak policy, learn what it can learn"
"$LYCAN" compile "$BASE/policy.lycs" >/dev/null 2>&1
echo "     compiled policy.lycs -> policy.lyc"

# ---- embedded proposer: reads the improvement brief on stdin, writes a
# ---- proposal JSON on stdout. Legit by construction; the adversarial
# ---- proposals below are crafted by hand to attack the gate.
cat > "$BASE/proposer.py" <<'PYEOF'
import json, sys
brief = json.load(sys.stdin)
tgt = brief["target_strategy"]
n = brief["n_options"]
# The proposer knows the domain: the hidden rule is piecewise at n=50.
# It can only improve in the ways the gate allows: a pure source string plus
# an expected_output it is honestly willing to be verified against.
if n == 3:
    arm = "piecewise_partial"
    src = ("(F p (n) (? (< n 25) (+ n 1) (? (< n 50) (- n 1) (* n 2))))\n"
           "(F guarded () (? (!= (!cap \"runtime.inputGet\" \"n\") null)"
           " (p (!cap \"runtime.inputGet\" \"n\")) 0))\n"
           "(guarded)")
elif n == 4:
    arm = "piecewise_exact"
    src = ("(F p (n) (? (< n 50) (+ n 1) (* n 2)))\n"
           "(F guarded () (? (!= (!cap \"runtime.inputGet\" \"n\") null)"
           " (p (!cap \"runtime.inputGet\" \"n\")) 0))\n"
           "(guarded)")
else:
    sys.exit("proposer: no improvement left for %d options" % n)
print(json.dumps({"name": arm, "source": src, "expected_output": "0",
                  "insert_into_strategy": tgt}))
PYEOF

# ---- embedded driver: decide/feedback rounds against the hidden rule ----
cat > "$BASE/driver.py" <<'PYEOF'
import json, subprocess, sys
lyc, lycan, phase, rounds, trace = sys.argv[1:6]
def run(*a):
    return subprocess.run([lycan, *map(str, a)], capture_output=True, text=True)
def decide(n):
    inp = lyc + ".in"
    open(inp, "w").write(json.dumps({"n": n}))
    return json.loads(run("decide", lyc, "--input", inp).stdout)
def target(n):
    return n + 1 if n < 50 else 2 * n
# decision rule of each arm, in graft order — mirrors the .lycs source and
# the proposals the proposer will submit.
ARM = {
    3: [("n+1",        lambda n: n + 1),
        ("2n",         lambda n: 2 * n),
        ("n-1 default",lambda n: n - 1)],
    4: [("n+1",        lambda n: n + 1),
        ("2n",         lambda n: 2 * n),
        ("n-1 default",lambda n: n - 1),
        ("piecewise@25 (proposed)", lambda n: (n + 1 if n < 25 else (n - 1 if n < 50 else 2 * n)))],
    5: [("n+1",        lambda n: n + 1),
        ("2n",         lambda n: 2 * n),
        ("n-1 default",lambda n: n - 1),
        ("piecewise@25 (proposed)", lambda n: (n + 1 if n < 25 else (n - 1 if n < 50 else 2 * n))),
        ("piecewise@50 (proposed)", lambda n: (n + 1 if n < 50 else 2 * n))],
}
d0 = decide(7)
node = d0["node_id"]
arms = ARM[len(d0["weights"])]
wins, tf = [], open(trace, "a")
for i in range(int(rounds)):
    n = (i * 37) % 100                     # deterministic uniform sweep of 0..99
    d = decide(n)
    chosen, res = d["chosen_option"], int(float(d["result"]))
    won = 1 if res == target(n) else 0
    wins.append(won)
    for a, (_, f) in enumerate(arms):      # full-information feedback: the
        r = 1.0 if f(n) == target(n) else 0.0   # world scores every arm
        run("feedback", lyc, node, "--option", a, "--reward", r)
    tf.write(json.dumps({"phase": phase, "round": i, "n": n, "chosen": chosen,
                         "result": res, "win": won, "weights": d["weights"]}) + "\n")
    if (i + 1) % 60 == 0:
        w = sum(wins[-100:]) / len(wins[-100:])
        print("     round %4d  win rate (last %d) = %.3f  chosen=arm%d  weights=%s"
              % (i + 1, len(wins[-100:]), w, chosen,
                 [round(x, 3) for x in d["weights"]]))
tf.close()
print("WINRATE %s %.4f" % (phase, sum(wins[-100:]) / len(wins[-100:])))
PYEOF

"$LYCAN" stats "$LYC" | sed -n 's/^/     /p' | sed -n '1,3p;/ADAPTIVE/p'
python3 "$BASE/driver.py" "$LYC" "$LYCAN" baseline 300 "$BASE/trace.jsonl" | tee "$BASE/d_base.txt"
W0=$(awk '/^WINRATE/{print $3}' "$BASE/d_base.txt")
echo "     baseline plateaued at win rate $W0 — no arm in the program can do better"
check "$(python3 -c "print('true' if $W0 < 0.65 else 'false')")" "baseline is genuinely weak (win rate $W0 < 0.65)"
echo

echo "== 2. BRIEF — the binary tells an AI what it is bad at"
"$LYCAN" capsule improve "$LYC" > "$BASE/brief.json"
sed -n 's/^/     /p' "$BASE/brief.json"
check "$(python3 -c "
import json; b=json.load(open('$BASE/brief.json'))
print('true' if b['n_options']==3 and isinstance(b['target_strategy'],int) and b['goal'] else 'false')")" \
  "brief names the strategy node, its 3 options, and the improvement contract"
echo "     note: per-option tries/correct_rate slots are only populated for (strategy ...)"
echo "     consensus nodes; for (choice ...) nodes the learning lives in the weights,"
echo "     and the win rates above are measured by the driver against ground truth."
echo

echo "== 3. EVOLVE — legit proposals, promoted only through the gate"
EV1=$(cd "$BASE" && python3 proposer.py < brief.json > p1.json && cat p1.json | python3 -c "import json,sys;d=json.load(sys.stdin);print(d['name'])")
echo "     proposer read the brief and proposed: $EV1"
echo "     source: $(python3 -c "import json;print(json.load(open('$BASE/p1.json'))['source'])")"
BEFORE=$(sha "$LYC")
"$LYCAN" evolve "$LYC" --proposal "$BASE/p1.json" --min-improvement 0 2>&1 | sed -n 's/^/     /p' | tee "$BASE/evolve1.txt"
AFTER=$(sha "$LYC")
check "$(python3 -c "print('true' if 'ACCEPTED' in open('$BASE/evolve1.txt').read() and '$BEFORE' != '$AFTER' else 'false')")" \
  "proposal 1 accepted by the gate; binary checksum moved"
python3 "$BASE/driver.py" "$LYC" "$LYCAN" after-p1 260 "$BASE/trace.jsonl" | tee "$BASE/d_p1.txt"
W1=$(awk '/^WINRATE/{print $3}' "$BASE/d_p1.txt")
echo "     win rate after proposal 1: $W1"

"$LYCAN" capsule improve "$LYC" > "$BASE/brief2.json"
echo "     re-brief now sees $(python3 -c "import json;print(json.load(open('$BASE/brief2.json'))['n_options'])") options — proposer goes again"
EV2=$(python3 "$BASE/proposer.py" < "$BASE/brief2.json" > "$BASE/p2.json" && python3 -c "import json;print(json.load(open('$BASE/p2.json'))['name'])")
echo "     proposer proposed: $EV2"
BEFORE=$(sha "$LYC")
"$LYCAN" evolve "$LYC" --proposal "$BASE/p2.json" --min-improvement 0 2>&1 | sed -n 's/^/     /p' | tee "$BASE/evolve2.txt"
AFTER=$(sha "$LYC")
check "$(python3 -c "print('true' if 'ACCEPTED' in open('$BASE/evolve2.txt').read() and '$BEFORE' != '$AFTER' else 'false')")" \
  "proposal 2 accepted; binary checksum moved again"
python3 "$BASE/driver.py" "$LYC" "$LYCAN" after-p2 260 "$BASE/trace.jsonl" | tee "$BASE/d_p2.txt"
W2=$(awk '/^WINRATE/{print $3}' "$BASE/d_p2.txt")
echo "     win rate after proposal 2: $W2"
echo
echo "     WIN-RATE TRAJECTORY: $W0 -> $W1 -> $W2"
check "$(python3 -c "print('true' if $W1 > $W0 + 0.15 and $W2 > $W1 else 'false')")" \
  "win rate strictly improved across accepted proposals ($W0 -> $W1 -> $W2)"
check "$(python3 -c "print('true' if $W2 >= 0.93 else 'false')")" "final policy near-perfect (win rate $W2 >= 0.93)"
"$LYCAN" stats "$LYC" | sed -n 's/^/     /p' | sed -n '/ADAPTIVE/p'
echo

echo "== 4. GAUNTLET — a compromised proposer attacks the gate"
# (a) contract-breaker: source that fails to compile at all
TARGET=$(python3 -c "import json;print(json.load(open('$BASE/brief2.json'))['target_strategy'])")
cat > "$BASE/p_attack_compile.json" <<EOF
{"name":"contract_breaker","source":"(F backdoor (x) (!bogus_op x))\n(backdoor 0)","expected_output":"0","insert_into_strategy":$TARGET}
EOF
BEFORE=$(sha "$LYC"); rm -f "$PROBE"
"$LYCAN" evolve "$LYC" --proposal "$BASE/p_attack_compile.json" 2>&1 | sed -n 's/^/     /p' > "$BASE/g_a.txt"
grep REJECTED "$BASE/g_a.txt" | sed -n 's/^/     /p'
{
  A_OK=$(python3 - "$LYC" "$BEFORE" "$BASE/g_a.txt" <<'PY'
import hashlib, pathlib, sys
lyc, before, log = sys.argv[1], sys.argv[2], pathlib.Path(sys.argv[3]).read_text()
now = hashlib.sha256(pathlib.Path(lyc).read_bytes()).hexdigest()
print("true" if "candidate compile error" in log and now == before else "false")
PY
)
}
check "$A_OK" "(a) invalid graph rejected at compile (reason above) — binary unchanged"

# (b) capability backdoor: a proposal whose arm is just a file write.
#     As of 2026-09-08 the gate verifies every candidate under a deny-all
#     effects sandbox (stdout stays on; the gate must run the host to measure
#     it) by default — no --policy flag needed (that flag only lets an operator
#     swap in a different, still restrictive, policy). This exact check is
#     how the hole was found: on the first run of this demo the probe file
#     WAS written during verification. The escape is closed; the checks
#     below now pass on evidence.
cat > "$BASE/p_attack_write.json" <<EOF
{"name":"backdoor_write","source":"(F wb (x) (!cap \"file.writeText\" \"$PROBE\" \"pwned\"))\n(wb 0)","expected_output":"null","insert_into_strategy":$TARGET}
EOF
BEFORE=$(sha "$LYC"); rm -f "$PROBE"
"$LYCAN" evolve "$LYC" --proposal "$BASE/p_attack_write.json" 2>&1 | sed -n 's/^/     /p' > "$BASE/g_b.txt"
grep REJECTED "$BASE/g_b.txt" | sed -n 's/^/     /p'
DENIED=$(grep -o 'denied by policy' "$BASE/g_b.txt" | head -1)
check "$(python3 -c "print('true' if '$DENIED' == 'denied by policy' and not __import__('os').path.exists('$PROBE') else 'false')")" \
  "(b) capability backdoor denied by the verification sandbox itself (no --policy used): denial string above, $PROBE never created"
check "$(python3 -c "print('true' if '$(sha "$LYC")' == '$BEFORE' else 'false')")" \
  "(b) binary checksum unchanged after backdoor attempt"
echo "     story: the first version of this demo CAUGHT a sandbox escape — verification"
echo "     of a raw .lyc ran candidates unrestricted and the probe was really written"
echo "     (2026-09-08). Fixed same day: candidates now run in the evolution sandbox"
echo "     — deny-all effects (file/network/stdin off, 30s budget; stdout stays on"
echo "     because the gate must run the host program to measure it; stdout is not a"
echo "     registry effect). 'lycan capsule apply-proposal' forces it. The demo proved"
echo "     the gate had a hole; now it proves the hole is gone."

# (c) poisoned-but-valid: compiles, pure, self-consistent, matches its own
#     declared expected_output — a churn arm that returns constant 300, which
#     never wins against this traffic, and brings no speed gain. The loop's
#     min-improvement threshold is the bar a no-gain arm cannot clear.
cat > "$BASE/p_attack_slow.json" <<EOF
{"name":"poisoned_churn_300","source":"(F churn (k) (? (== k 0) 0 (+ 1 (churn (- k 1)))))\n(churn 300)","expected_output":"300","insert_into_strategy":$TARGET}
EOF
BEFORE=$(sha "$LYC")
"$LYCAN" evolve "$LYC" --proposal "$BASE/p_attack_slow.json" --min-improvement 0.95 2>&1 | sed -n 's/^/     /p' > "$BASE/g_c.txt"
grep REJECTED "$BASE/g_c.txt" | sed -n 's/^/     /p'
check "$(python3 -c "print('true' if 'below threshold' in open('$BASE/g_c.txt').read() and '$(sha "$LYC")' == '$BEFORE' else 'false')")" \
  "(c) valid sabotage arm rejected by min-improvement gate; binary unchanged"
echo "     honesty note: expected_output is proposer-declared — the gate verifies the"
echo "     candidate against what the proposal CLAIMS. An arm that lies about its own"
echo "     output is caught here, but gate math is claim-relative:"
cat > "$BASE/p_attack_lie.json" <<EOF
{"name":"lied_expect","source":"(F claimed (n) (* n 0))\n(claimed 5)","expected_output":"5","insert_into_strategy":$TARGET}
EOF
BEFORE=$(sha "$LYC")
"$LYCAN" evolve "$LYC" --proposal "$BASE/p_attack_lie.json" 2>&1 | sed -n 's/^/     /p' > "$BASE/g_c2.txt"
grep REJECTED "$BASE/g_c2.txt" | sed -n 's/^/     /p'
check "$(python3 -c "print('true' if 'wrong answer' in open('$BASE/g_c2.txt').read() and '$(sha "$LYC")' == '$BEFORE' else 'false')")" \
  "(c+) mismatch between claimed and actual output rejected; binary unchanged"

# (d) dry-run: a fully legit proposal, verified end-to-end, must never mutate.
BEFORE=$(sha "$LYC"); JL=$(wc -l < "$LYC.evolution.jsonl")
"$LYCAN" evolve "$LYC" --proposal "$BASE/p2.json" --min-improvement 0 --dry-run 2>&1 | sed -n 's/^/     /p' > "$BASE/g_d.txt"
grep -E 'WOULD_|ACCEPTED' "$BASE/g_d.txt" | sed -n 's/^/     /p'
check "$(python3 -c "print('true' if 'WOULD_' in open('$BASE/g_d.txt').read() and '$(sha "$LYC")' == '$BEFORE' else 'false')")" \
  "(d) dry-run verified the proposal but never mutated the binary (checksum identical)"
check "$(python3 -c "print('true' if $(wc -l < "$LYC.evolution.jsonl") == $JL else 'false')")" \
  "(d) dry-run also skipped the lock and the journal — ledger only records real mutations and real rejections"
echo

echo "== 5. LEDGER — immutable audit trail of self-modification"
sed -n 's/^/     /p' "$LYC.evolution.jsonl"
NA=$(grep -c ProposalAccepted "$LYC.evolution.jsonl" || true)
NR=$(grep -c ProposalRejected "$LYC.evolution.jsonl" || true)
check "$(python3 -c "print('true' if $NA >= 2 and $NR >= 4 else 'false')")" \
  "journal records $NA promotions and $NR rejections, each with reason + before/after hashes"
check "$(python3 -c "
import json
rows=[json.loads(l) for l in open('$LYC.evolution.jsonl')]
acc=[r for r in rows if r['event']=='ProposalAccepted']
print('true' if all(r['hash_before']!=r['hash_after'] for r in acc) else 'false')")" \
  "every promotion links the exact before/after binary hashes"
echo "     rollback points: $(ls "$LYC.snapshots" | wc -l | tr -d ' ') snapshots kept"
echo

echo "== 6. RECEIPT"
echo "     policy.lyc     sha256 $(sha "$LYC")"
echo "     journal.jsonl  sha256 $(sha "$LYC.evolution.jsonl")"
echo "     decision trace sha256 $(sha "$BASE/trace.jsonl")  ($(wc -l < "$BASE/trace.jsonl" | tr -d ' ') rounds, replayable)"
echo
echo "======================================================================"
echo "SCORE: $PASS/$((PASS+FAIL)) checks passed"
if [ "$FAIL" -gt 0 ]; then echo "SELF-EVOLVE GATE ISSUE DETECTED"; exit 1; fi
echo "The policy improved itself twice; four attacks were rejected on evidence."
