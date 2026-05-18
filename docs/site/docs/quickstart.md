# Quickstart (30 minutes)

This is the shortest path from "no Syntra" to "a capsule that is making
adaptive decisions for me." It runs the F1 demo image (self-contained,
no local checkout required), drives one capsule through `/install`,
`/decide`, and `/feedback`, then shows you where to look to see the
learner moving.

You will need:

- Docker (Linux / macOS / Windows / WSL2 all fine)
- `curl` and `jq` for inspecting responses
- ~30 minutes

By the end you will have run a real capsule, seen a real `decisionId`,
posted a real reward, and watched the bandit's weights shift in
response.

## 1. Run the demo container

Pull and run the published demo image:

```bash
docker run --rm \
  -p 8787:8787 \
  -p 8080:8080 \
  ghcr.io/ashhart/syntra:demo
```

The image is built and pushed by `.github/workflows/publish-demo-image.yml`
on each push to `main` (multi-arch: `linux/amd64`, `linux/arm64`). An
immutable per-commit tag `ghcr.io/ashhart/syntra:demo-<sha>` is
published alongside the moving `:demo` tag if you need to pin.

### Alternative: build from source

For offline use, behind-the-firewall environments, or local development
against an in-flight commit:

```bash
git clone https://github.com/ashhart/Syntra.git
cd Syntra
docker build -t syntra:demo -f Syntra/docker/Dockerfile.demo .

docker run --rm \
  -p 8787:8787 \
  -p 8080:8080 \
  syntra:demo
```

The from-source build takes one Rust compile (~3–5 minutes).

Two ports open:

- **`:8787`** — the Syntra HTTP API
- **`:8080`** — the live dashboard (`http://localhost:8080`)

Inside the first ~60 seconds the lifecycle on the dashboard flips from
**Warmup** to **Active**. Inside the first ~5 minutes the meta-bandit
panel shows trial counts climbing across all seven candidate
algorithms.

Leave it running. The rest of the steps target this container.

## 2. Verify health and grab the admin key

The demo image generates a fresh admin key each time it boots and
prints it on the first lines of the log:

```
[demo] admin key:   demo-key-1747000000
[demo] store:       /syntra/data
[demo] demo capsule: predictive-autoscaling (...)
```

Copy the `demo-key-...` value and export it:

```bash
curl -s http://localhost:8787/health
# → {"ok": true, "service": "Syntra"}

# Replace with the actual key printed in your container's logs.
export SYNTRA_ADMIN_KEY="demo-key-1747000000"
```

For a non-demo install, you set the key yourself by passing
`SYNTRA_ADMIN_KEY=<your-secret>` to the container (the demo
entrypoint respects that env var). The server refuses to start
without an admin key unless you pass `--dev-mode`, which is intended
for local development only.

## 3. Install a fresh capsule

The demo image already has one capsule running, but let's install a
second one so you control the lifecycle. We will use the predictive
autoscaling demo — it shows kernels, features, and the strategy node
all working together.

```bash
# Assuming you have a Syntra checkout. If not, clone it first:
# git clone https://github.com/ashhart/Syntra.git && cd Syntra
cd examples/predictive-autoscaling

# Install the compiled .lyc graph.
curl -X POST \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/install" \
  -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  --data-binary @program.lyc

# Attach the learning config (feature context + refusal).
curl -X PUT \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/learning" \
  -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  --data-binary @learning.json
```

You should see two `{"ok": true, ...}` responses.

## 4. Ask for a decision

```bash
curl -s -X POST \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/decide" \
  -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "load_history":        [82, 88, 95, 110, 132, 158, 174, 188, 201, 215],
    "current_instances":   3,
    "target_per_instance": 100,
    "min_instances":       1,
    "max_instances":       20,
    "features": {
      "hour":              14.0,
      "current_instances": 3,
      "load_trend":        0.6
    }
  }' | jq .
```

The response carries:

- `decisionId` — opaque token, you pass this back to `/feedback`
- `decisions[0].chosen_option` — the **zero-based index** of the option
  picked (0 = `hold`, 1 = `forecast_match`, 2 = `forecast_headroom`,
  3 = `p95_safe`)
- `decisions[0].weights` — current strategy weights over the four
  options; during Warmup these are uniform (`0.25` each)
- `stdout` — what the capsule's program printed; you can see the
  forecast number, the per-policy candidate counts, and the
  human-readable decision string
- `warmup` — lifecycle state. For the first ~30 feedback rounds this
  will say `state: "warmup"`.

Save the `decisionId` to a shell variable — you will use it in the next
step:

```bash
DECISION_ID=$(curl -s -X POST \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/decide" \
  -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "load_history":        [82, 88, 95, 110, 132, 158, 174, 188, 201, 215],
    "current_instances":   3,
    "target_per_instance": 100,
    "min_instances":       1,
    "max_instances":       20,
    "features": {"hour":14.0,"current_instances":3,"load_trend":0.6}
  }' | jq -r .decisionId)

echo "$DECISION_ID"
```

## 5. Post feedback

When the outcome of that decision resolves in your real service — the
SLA window ended, the chargeback came in, the LLM response was rated —
you `POST` it back to `/feedback` against the `decisionId`. For the
walkthrough we synthesize a reward:

```bash
curl -s -X POST \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/feedback" \
  -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d "{
    \"decisionId\": \"$DECISION_ID\",
    \"rewardComponents\": {
      \"sla_met\":         1.0,
      \"cost_efficiency\": 0.7
    }
  }" | jq .
```

The response shows the weights **before** and **after** the feedback
was applied:

```json
{
  "ok": true,
  "nodeId": 71,
  "option": 2,
  "reward": 0.79,
  "before": [0.25, 0.25, 0.25, 0.25],
  "after":  [0.25, 0.25, 0.27, 0.23]
}
```

That is the bandit moving. One round, one nudge. The `rewardComponents`
shape is normalized by the capsule's reward spec (`sla_met * 0.7 +
cost_efficiency * 0.3`) and recorded with the decision.

## 6. Watch the lifecycle move

Repeat steps 4 and 5 in a loop — about 30 rounds is the warmup target.
The fast way:

```bash
for i in $(seq 1 35); do
  DID=$(curl -s -X POST \
    "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/decide" \
    -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
    -H "Content-Type: application/json" \
    -d '{"load_history":[80,90,110,140,180,220],
         "current_instances":3,"target_per_instance":100,
         "min_instances":1,"max_instances":20,
         "features":{"hour":14.0,"current_instances":3,"load_trend":0.6}}' \
    | jq -r .decisionId)

  curl -s -X POST \
    "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/feedback" \
    -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
    -H "Content-Type: application/json" \
    -d "{\"decisionId\":\"$DID\",\"reward\":$((RANDOM % 100))e-2}" \
    > /dev/null
done
```

Now check the lifecycle and learned state:

```bash
# Quick view: strategy weights + per-option counts.
curl -s -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/report" | jq .

# Full view: warmup, meta-bandit, OOD detectors, ADWIN state.
curl -s -H "Authorization: Bearer $SYNTRA_ADMIN_KEY" \
  "http://localhost:8787/tenants/quick/jobs/scale/capsules/autoscaler/memory" | jq .
```

After ~30 feedback rounds, `warmup.state` flips to `"active"` and the
meta-bandit starts running all seven candidate algorithms in parallel,
converging on whichever performs best on your traffic.

## 7. Open the dashboard

In a browser:

```
http://localhost:8080
```

You will see:

- Lifecycle (Warmup → Active → Frozen)
- Strategy weights live
- Decision log
- Audit trail
- Per-context memory
- Meta-bandit candidate trials & cumulative reward

This is everything the API exposes, rendered. For integration code you
go straight to the API. For operators, the dashboard is the fast path.

## What just happened

You ran an adaptive choice through one round of the full Syntra
contract:

| Step | Endpoint | What moved |
|------|----------|------------|
| Install | `POST /…/install` | Graph binary + audit event |
| Learning | `PUT /…/learning` | Context spec & refusal config |
| Decide | `POST /…/decide` | Decision recorded; bandit sampled |
| Feedback | `POST /…/feedback` | Weights updated; meta-bandit credited |

Multiply that by thousands of real requests and the bandit converges on
the option that wins the reward you defined. That is the whole product.

## Where next

- Pick a [domain pack](examples/index.md) closest to your problem and
  copy the capsule shape.
- Read the [capsule concept](concepts/capsule.md) and
  [meta-bandit concept](concepts/meta-bandit.md) to understand what is
  happening under `/decide`.
- Look at the [API reference](reference/api.md) for the full surface
  including `/contexts`, `/audits`, `/evolution`, and the policy and
  reward-spec endpoints.
- Read the [operations stub](reference/operations.md) before running
  Syntra under load.
