# Quickstart

This walks through Syntra on one machine: start a server, create a capsule,
decide and reward over HTTP, give an application its own token, decide
in-process from Python, then score a change on the logged traffic and
promote it only if it passes. It takes about ten minutes. Run the commands
from the repository root in bash or zsh; you need a Rust toolchain, `curl`,
`openssl` and Python 3.10 or later.

## 0. Watch it learn (optional)

```bash
cargo run --release --bin syntra -- demo
```

This builds the server and starts it on `127.0.0.1:8787` with a fresh store
in the system's temporary directory and a random admin key. It creates the
capsule `acme/llm/router` with three model routes and sends it simulated
traffic, about 100 requests a second (`--rate` changes that). The quality
and cost of each route per task are made up. It prints the admin console's
address and the key; open `http://127.0.0.1:8787/admin`, enter the key,
and look at the decisions, the spec and the audit trail, or run an
evaluation. Every ten seconds the terminal prints the share of the best
possible expected reward the capsule earned (97% and then 98.5% in the
first two windows on the machine these docs were checked on). Stop it with
Ctrl-C before step 1, which uses the same port.

## 1. Start the server

```bash
cargo build --release
export KEY=$(openssl rand -hex 24)
./target/release/syntra serve --store ./syntra-store --admin-key "$KEY" &
sleep 1
```

`serve` listens on `127.0.0.1:8787` and creates the store directory if it
is missing. It will not start without an admin key (`--admin-key` or the
`SYNTRA_ADMIN_KEY` environment variable) unless you pass `--dev-mode`,
which leaves every route open and binds only loopback addresses. Logs are
JSON lines on stderr.

```bash
curl -s http://127.0.0.1:8787/health
# {"ok":true,"service":"Syntra"}
./target/release/syntra health
# {"ok":true,"service":"Syntra"}
```

## 2. Create a capsule

A capsule is one decision point, addressed as `tenant/job/capsule`.
`PUT .../spec` creates it, with its tenant and job, from a JSON merge patch
over the default spec:

```bash
B=http://127.0.0.1:8787/v1/tenants/acme/jobs/prod/capsules/router
curl -s -X PUT $B/spec -H "Authorization: Bearer $KEY" \
  -d '{"actions": [{"id": "small", "features": {"cost": 0.1}}, {"id": "large", "features": {"cost": 1.0}}]}' \
  | python3 -m json.tool
```

The answer (201 the first time, 200 after) is the whole spec with the
defaults filled in:

```json
{
    "actions": [
        {"features": {"cost": 0.1}, "id": "small"},
        {"features": {"cost": 1.0}, "id": "large"}
    ],
    "baselineEpsilon": 0.1,
    "exploration": {"epsilon": 0.05, "floor": 0.05, "gammaExponent": 0.5, "gammaScale": 10.0, "kind": "squarecb"},
    "learner": {"bits": 18, "importance": "none", "learningRate": 0.5, "maxImportanceWeight": 100.0},
    "mode": "learner",
    "reward": {"range": [0.0, 1.0], "waitSeconds": 600},
    "rewards": "first",
    "snapshotEvery": 1000
}
```

- `exploration`: SquareCB, and a floor that keeps every eligible action's
  probability at or above `floor / K` (here 0.025), so the log can always
  be evaluated.
- `learner`: one online least-squares model over hashed features of the
  context crossed with each action's features.
- `reward.range`: Syntra maps rewards to [0, 1] with it and clamps them.
- `rewards`: `first` keeps one reward per decision; `sum` adds every reward
  and uses idempotency keys to drop retries.
- `mode`: `learner`, `baselineExplore` (serve your incumbent action and
  explore around it) or `frozen` (serve without learning).

The server rejects unknown fields, so a typo is a 400, not a silent
default. The full schema is `DecisionSpec` in [openapi.yaml](openapi.yaml).

## 3. Decide and reward

```bash
curl -s -X POST $B/decide -H "Authorization: Bearer $KEY" \
  -d '{"context": {"task": "code", "promptTokens": 812}}'
# {"action":"small","actionIndex":0,"decisionId":"dec_1a0d00de290243e1ad3cd35","mode":"learner",
#  "modelVersion":0,"probability":0.5,"ranking":[{"id":"small","probability":0.5},{"id":"large","probability":0.5}]}
```

`probability` is the propensity, the probability the chosen action had
when it was drawn. Syntra logs it, with the whole distribution and the
seed, for every decision. Report the outcome with the `decisionId`, now or
days later:

```bash
ID=$(curl -s -X POST $B/decide -H "Authorization: Bearer $KEY" \
  -d '{"context": {"task": "code", "promptTokens": 812}}' \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["decisionId"])')
curl -s -X POST $B/reward -H "Authorization: Bearer $KEY" \
  -d "{\"decisionId\": \"$ID\", \"reward\": 0.8}"
# {"applied":true,"learned":true,"modelVersion":1,"ok":true}
curl -s -X POST $B/reward -H "Authorization: Bearer $KEY" \
  -d "{\"decisionId\": \"$ID\", \"reward\": 0.3}"
# {"applied":false,"duplicate":true,"modelVersion":1,"ok":true}
```

The second reward is a duplicate because the capsule keeps the first. The
stored decision shows everything needed to replay or evaluate it:

```bash
curl -s $B/decisions/$ID -H "Authorization: Bearer $KEY" | python3 -m json.tool
```

It holds the context, the actions, `eligible` and `pmf` (the distribution
the draw came from), `chosenIndex`, `probability`, `seed`, `modelVersion`
and the `rewards`.

Two request options are worth knowing early. `"eventId": "<your id>"` makes
a decide idempotent. The id becomes the `decisionId`, and resending the
same body returns the stored decision. With `"durable": true` the server
answers only after the record is in `syntra.db`; without it, the server
answers once the record is queued and commits it within about 2 ms.

## 4. Give your application its own token

The admin key is for operators. An application gets a token scoped to its
capsule (`read`: decide, reward, uploads and reads) or to its tenant
(`tenant_admin`):

```bash
export APP=$(curl -s -X POST http://127.0.0.1:8787/v1/admin/tokens -H "Authorization: Bearer $KEY" \
  -d '{"scope": {"kind": "read", "tenant": "acme", "job": "prod", "capsule": "router"}, "label": "router-app", "ttlSeconds": 2592000}' \
  | python3 -c 'import sys, json; print(json.load(sys.stdin)["token"])')
curl -s -X POST $B/decide -H "Authorization: Bearer $APP" -d '{"context": {"task": "chat"}}'
curl -s -X PUT $B/spec -H "Authorization: Bearer $APP" -d '{"mode": "frozen"}'
# {"error":"forbidden: scope does not allow this action"}
```

The server shows the raw token once and keeps only its SHA-256.
`GET /v1/admin/tokens` lists tokens (with `lastUsedAt`), and
`DELETE /v1/admin/tokens/{hash}` revokes one immediately.

## 5. Decide in-process from Python

The Python SDK's `LocalDecider` holds a copy of the published model and
decides in-process, in a microsecond or two, with the same code the server
runs. Build the extension and put the package on the path:

```bash
sdk/python/scripts/develop.sh
export PYTHONPATH=$PWD/sdk/python/python
```

This loop plays an application that serves about a thousand requests a
second. The rewards are simulated. The large model pays off on code and
the small one on chat.

```bash
python3 - <<'EOF'
import os, random, time
from syntra import LocalDecider

rng = random.Random(1)
with LocalDecider("http://127.0.0.1:8787", token=os.environ["APP"],
                  tenant="acme", job="prod", capsule="router") as router:
    for _ in range(3000):
        task = rng.choice(["chat", "code"])
        d = router.decide({"task": task, "promptTokens": rng.randint(50, 3000)})
        mean = 0.8 if (task == "code") == (d.action == "large") else 0.4  # simulated
        router.reward(d.decision_id, min(1.0, max(0.0, rng.gauss(mean, 0.1))))
        time.sleep(0.001)
    print("model version:", router.model_version)
EOF
```

A background thread uploads decisions and rewards and fetches the newer
model once a second; leaving the `with` block uploads what is left. The
server replays every uploaded decision against the model and seed it
names and stores it only if the choice and every probability match, then
learns from its rewards:

```bash
curl -s http://127.0.0.1:8787/metrics -H "Authorization: Bearer $KEY" | grep '^syntra_uploaded'
# syntra_uploaded_decisions_accepted_total 3000
# syntra_uploaded_decisions_rejected_total 0
for task in chat code; do
  curl -s -X POST $B/decide -H "Authorization: Bearer $APP" \
    -d "{\"context\": {\"task\": \"$task\", \"promptTokens\": 900}}" \
    | python3 -c 'import sys, json; d = json.load(sys.stdin); print(sys.argv[1], d["ranking"])' $task
done
# chat [{'id': 'small', 'probability': 0.97...}, {'id': 'large', 'probability': 0.03...}]
# code [{'id': 'large', 'probability': 0.96...}, {'id': 'small', 'probability': 0.03...}]
```

With the server unreachable, `decide` keeps working on the last model it
synced and uploads resume when the server is back. The Rust SDK is
`syntra::client::LocalDecider`; for LLM calls, `syntra.llm.ModelRouter`
wraps the decider (see [examples/llm-routing](../examples/llm-routing/)).

## 6. Evaluate a change before it serves

Every logged decision carries its propensity, so `syntra evaluate` can
estimate what another policy would have earned on the same traffic. It
reads the store without writing to it, so it is safe while the server
runs:

```bash
./target/release/syntra evaluate --store ./syntra-store --capsule acme/prod/router \
  --policy greedy --format markdown | head -3
# # Off-policy evaluation: greedy
#
# **Verdict:** No gates set. DR estimates greedy at 0.8015 (95% CI 0.7973 to 0.8052) against 0.7437
# for the logged policy, a paired lift of +0.05779 (95% CI 0.05277 to 0.06363); the lift interval
# lies above 0, so the target beats the logged policy at the 95% level.
```

Policies: `logged` (what was served), `greedy` (the argmax of a reward
model learned from the log, cross-fitted so no row is scored by a model
that saw it), `constant:<id>`, `candidate:<spec.json>` (a candidate spec as
it would serve, exploration included), `spec:<spec.json>` (the same
candidate without exploration) and `target-column`. The report has DM,
IPS, SNIPS and doubly robust (DR) estimates with bootstrap intervals, each
estimator's lift over the logged policy paired on the same rows, the
effective sample size and weight diagnostics. `syntra evaluate --help` lists every option.

Gates turn the report into a decision. With `--fail-on-gate`, a failed gate
exits 1, which is what a CI job or a deploy script checks:

```bash
cat > gates.yaml <<'EOF'
gates:
  - lift.dr.lower >= 0   # beats the logged policy at the 95% level
  - ess >= 200           # enough effective samples behind the estimate
EOF
./target/release/syntra evaluate --store ./syntra-store --capsule acme/prod/router \
  --policy greedy --gates gates.yaml --fail-on-gate --out greedy.json; echo "exit $?"
# syntra evaluate: wrote greedy.json. PASS: all 2 gates pass. ...
# exit 0
./target/release/syntra evaluate --store ./syntra-store --capsule acme/prod/router \
  --policy constant:small --gates gates.yaml --fail-on-gate --out small.json; echo "exit $?"
# syntra evaluate: wrote small.json. FAIL: 1 of 2 gates fail (lift.dr.lower >= 0: -0.1490 vs 0). ...
# exit 1
```

The same report over HTTP. Evaluating reads up to two million decisions,
so it needs a `tenant_admin` or admin key, not an application's `read`
token, and at most two evaluations run at once (a third gets 429):

```bash
curl -s -X POST $B/evaluate -H "Authorization: Bearer $KEY" \
  -d '{"policy": "greedy", "gates": ["lift.dr.lower >= 0"]}' \
  | python3 -c 'import sys, json; r = json.load(sys.stdin); print(r["gatesPassed"], r["verdict"])'
# True PASS: all 1 gates pass. DR estimates greedy at 0.8015 ...
```

## 7. Promote a spec change only if it passes

`POST .../promote` scores a spec patch as it would serve and applies it
only if every gate passes. It needs at least one gate, answers 200 with the
new spec and the report or 409 with the report, and audits both outcomes:

```bash
curl -s -X POST $B/promote -H "Authorization: Bearer $KEY" \
  -d '{"spec": {"learner": {"learningRate": 0.25}}, "gates": ["lift.dr.lower >= 0"]}' \
  -o /dev/null -w '%{http_code}\n'
# 200
curl -s -X POST $B/promote -H "Authorization: Bearer $KEY" \
  -d '{"spec": {"exploration": {"floor": 0.5}}, "gates": ["lift.dr.lower >= 0"]}' \
  -o /dev/null -w '%{http_code}\n'
# 409
curl -s "$B/audits?limit=2" -H "Authorization: Bearer $KEY" \
  | python3 -c 'import sys, json; [print(a["event"], a["detail"]["patch"]) for a in json.load(sys.stdin)["audits"]]'
# spec_promoted {'learner': {'learningRate': 0.25}}
# promotion_refused {'exploration': {'floor': 0.5}}
```

"As it would serve" means the probabilities the candidate's exploration
would put on each action of each logged decision, given the predictions of
its learner trained on the other rows. So a gate sees what exploring costs;
a floor of 0.5 spreads half the traffic evenly and is refused here. The
comparison is with the logged policy, which includes the decisions made
before the model had learned, so a candidate close to the current spec
usually passes `lift.dr.lower >= 0`. To compare a candidate with the
current spec rather than with the log, evaluate both and compare their
estimates: `{"spec": {}}` is the current spec as it serves.

## 8. Restart, back up, check

The process is disposable; the store is not. Stop the server (it drains
requests, commits the queue and snapshots every model) and start it again:

```bash
./target/release/syntra stop
wait    # the server was started with & in this shell
./target/release/syntra serve --store ./syntra-store --admin-key "$KEY" &
sleep 1
curl -s $B/model -H "Authorization: Bearer $KEY" | python3 -c 'import sys, json; print(json.load(sys.stdin)["modelVersion"])'
```

The model version is the same as before the restart, because the server
rebuilds a capsule's model from its latest snapshot plus the rewards logged
after it. Take a consistent copy while the server runs, and check the
store:

```bash
./target/release/syntra backup --store ./syntra-store --out ./syntra-backup
# {"eventStore":true,"files":5,"ok":true,"out":"./syntra-backup"}
./target/release/syntra doctor --store ./syntra-store
# {"capsules":1,"errors":0,"summary":true,"warnings":0}
```

When you are done, `./target/release/syntra stop`.

## Next

- [operating.md](operating.md): the store, durability, backups, metrics,
  tokens, rate limits and troubleshooting.
- [deployment.md](deployment.md): Docker, the Helm chart, TLS.
- [api.md](api.md) and [openapi.yaml](openapi.yaml): every route.
- [examples/llm-routing](../examples/llm-routing/): model routing with
  `syntra.llm.ModelRouter`, evaluated at the end.
- [migrating/personalizer.md](migrating/personalizer.md): moving an Azure
  Personalizer loop onto Syntra.
- [concepts.md](concepts.md): contextual bandits, propensities and
  off-policy evaluation, in plain terms.
