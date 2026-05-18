# Syntra in 30 minutes

This is an end-to-end walkthrough for someone who has Syntra running and
wants to drive a real capsule from authoring through learning to integration.
It uses `curl` for the platform-side operations and ends with the Python
integration pack to show what production adoption looks like.

By the end of this tutorial you will have authored a capsule from YAML,
installed it, configured feature-context learning, sent 100 decide/feedback
cycles, inspected what was learned, optionally enabled refusal, and pointed
the Python integration library at your capsule. For the API reference see
[`api.md`](api.md); for deployment shapes see [`deployment.md`](deployment.md);
for operational concerns once you ship see [`operating.md`](operating.md).

## Before you start

You need a running Syntra. The fastest path is the demo image — see the
[top-level README](../README.md) for the build-and-run command. The rest of
this tutorial assumes the appliance is reachable at `http://localhost:8787`
and the admin key is in your environment:

```bash
export SYNTRA_URL=http://localhost:8787
export LYCAN_ADMIN_KEY=<your admin key>
```

You also need the `syntra` CLI on your `PATH`. If you ran the demo image,
shell into it (`docker exec -it <container> bash`) or build the CLI locally
with `cargo build --release --bin syntra`.

A health check:

```bash
curl -s $SYNTRA_URL/health
# {"ok":true,"service":"Syntra"}
```

## Step 1: author a capsule

Write the capsule's options and reward shape in YAML. Save this as
`router.yaml`:

```yaml
name: llm-router
options:
  - cheap_fast
  - balanced
  - expensive_accurate
reward:
  type: continuous
  range: [-1.0, 1.0]
```

Three options, one reward channel between -1 and 1. Compile it:

```bash
syntra author router.yaml --out-dir ./router-capsule/
```

This emits `./router-capsule/program.lyc` (the graph binary) and a small set
of sidecar JSON files (`reward_spec.json` if you declared components,
`learning.json` template, and so on).

Optionally, smoke-test the capsule's logic locally before deploying:

```bash
syntra simulate router.yaml --rounds 5000 --true-arm-rewards "0.2,0.5,0.7" --seed 7
```

This drives a synthetic 5000-round trial with the named true rewards and
prints the convergence trace. Use it to sanity-check that the bandit
recovers the right arm before you involve real traffic.

## Step 2: install the capsule

POST the compiled `.lyc` binary to the install endpoint. The capsule path
follows the `tenants/{tenant}/jobs/{job}/capsules/{capsule}` shape:

```bash
curl -X POST $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/install \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @./router-capsule/program.lyc
```

Expected output:

```json
{"ok":true,"tenant":"myteam","job":"routing","capsule":"router","hash":"a1b2c3d4..."}
```

The `hash` is the SHA-256 of the uploaded bytes and is recorded in the
capsule's `audit.jsonl`. Keep it; you'll use it later to verify which graph
version learned which weights.

## Step 3: configure feature-context learning

By default a new capsule uses discrete context (an opaque `contextKey`
string). To enable LinUCB and rich feature-based learning, declare a feature
schema in `learning.json`. PUT it to the learning endpoint:

```bash
curl -X PUT $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/learning \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "contextSpec": {
      "type": "features",
      "features": [
        {"name": "prompt_tokens",   "type": {"kind": "continuous", "range": [0, 8000]}},
        {"name": "user_tier",       "type": {"kind": "categorical", "values": ["free", "pro", "enterprise"]}},
        {"name": "hour_of_day",     "type": {"kind": "cyclic", "period": 24.0}}
      ]
    }
  }'
```

Expected output (the server canonicalizes the body and echoes the merged
config back):

```json
{"ok":true,"config":{"algorithm":"simpleWeighted","contextSpec":{"type":"features","features":[...]},...}}
```

You can `GET /learning` at any time to inspect the current canonical config.
The feature kinds are `continuous` (with optional `range` for normalization),
`categorical` (one-hot encoded, the first level becomes the reference
category), and `cyclic` (encoded as sin/cos over the declared `period` —
ideal for time-of-day and day-of-year).

## Step 4: drive decide/feedback cycles

Now feed traffic. A single decide returns an option index and a decisionId:

```bash
curl -X POST $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/decide \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"features": {"prompt_tokens": 1500, "user_tier": "pro", "hour_of_day": 14.0}}'
```

Expected output (Warmup state for the first ~30 calls):

```json
{
  "ok": true,
  "decisionId": "dec_e1f2a3b4c5d60718",
  "warmup": {"state": "warmup", "...": "..."},
  "decisions": [{"node_id": 1, "chosen_option": 2, "weights": [0.33,0.33,0.34], "...": "..."}],
  "refused": false,
  "confidence": {"oodScore": 0.0, "intervalWidth": null, "refused": false, "...": "..."}
}
```

Send feedback with the decisionId and a reward in [-1, 1]:

```bash
curl -X POST $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/feedback \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"decisionId": "dec_e1f2a3b4c5d60718", "reward": 0.7}'
```

Expected output:

```json
{"ok":true,"nodeId":1,"option":2,"reward":0.7,"before":[0.33,0.33,0.33],"after":[0.31,0.31,0.38]}
```

Drive 100 cycles in a loop. The script below picks features randomly,
synthesizes a reward that rewards `expensive_accurate` for high token counts
and `cheap_fast` for low token counts, and sends both halves of every cycle:

```bash
for i in $(seq 1 100); do
  tokens=$(( RANDOM % 8000 ))
  tier=$(shuf -n1 -e free pro enterprise)
  hour=$(( RANDOM % 24 ))

  # decide
  resp=$(curl -s -X POST $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/decide \
    -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
    -d "{\"features\":{\"prompt_tokens\":$tokens,\"user_tier\":\"$tier\",\"hour_of_day\":$hour}}")
  did=$(echo "$resp" | python3 -c 'import sys,json; print(json.load(sys.stdin)["decisionId"])')
  opt=$(echo "$resp" | python3 -c 'import sys,json; print(json.load(sys.stdin)["decisions"][0]["chosen_option"])')

  # synthesize ground-truth reward
  if   [ $tokens -gt 4000 ] && [ $opt -eq 2 ]; then reward=0.8
  elif [ $tokens -lt 1000 ] && [ $opt -eq 0 ]; then reward=0.7
  elif [ $opt -eq 1 ]; then reward=0.5
  else reward=-0.2
  fi

  # feedback
  curl -s -X POST $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/feedback \
    -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
    -d "{\"decisionId\":\"$did\",\"reward\":$reward}" > /dev/null
done
```

This takes well under a minute. After it finishes, the capsule has crossed
the 30-feedback Warmup threshold and is in Active state.

## Step 5: inspect what was learned

The cheap inspection path is `/report`:

```bash
curl -s $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/report \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY"
```

Expected output (abridged):

```json
{
  "tenant": "myteam", "job": "routing", "capsule": "router",
  "hash": "a1b2c3d4...",
  "strategies": [{
    "node_id": 1, "activations": 100, "n_options": 3,
    "options": [
      {"option":0,"tries":34,"correct":21,"avg_ms":0.0,"weight":0.42},
      {"option":1,"tries":33,"correct":18,"avg_ms":0.0,"weight":0.33},
      {"option":2,"tries":33,"correct":19,"avg_ms":0.0,"weight":0.25}
    ]
  }]
}
```

The richer inspection path is `/memory`:

```bash
curl -s $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/memory \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" | python3 -m json.tool | less
```

Look for `strategies[nodeId].metaBandit.candidates[]`. Each entry is one of
the six candidate algorithms with its trials, rolling reward, and selection
state. With a feature-context capsule, LinUcb is in the list; after 100
rounds you usually see one or two candidates pulling ahead in rolling reward.

`/contexts` shows which buckets actually exist:

```bash
curl -s $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/contexts \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY"
```

For a feature-context capsule, the `contextKey` field is a synthetic
encoding of the feature vector — useful for sanity-checking that buckets
exist where you expect, less useful for direct interpretation than the
LinUCB theta vector inside `/memory`.

## Step 6 (optional): enable refusal

Refusal is a Phase E safety feature that lets `/decide` opt out when it
isn't confident, so your service can fall back. Enable it by setting the
`refusal` block in `learning.json`:

```bash
curl -X PUT $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/learning \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "refusal": {
      "enabled": true,
      "coverage": 0.95,
      "maxIntervalWidth": 0.3,
      "oodThreshold": 0.8
    }
  }'
```

Now send a decide with obviously out-of-distribution features (token count
far outside the trained range):

```bash
curl -s -X POST $SYNTRA_URL/tenants/myteam/jobs/routing/capsules/router/decide \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -d '{"features": {"prompt_tokens": 50000, "user_tier": "enterprise", "hour_of_day": 3.0}}'
```

Expected output:

```json
{
  "ok": true,
  "decisionId": "dec_...",
  "decisions": [],
  "refused": true,
  "oodScore": 0.94,
  "confidence": {
    "oodScore": 0.94,
    "intervalWidth": null,
    "coverage": 0.95,
    "refused": true,
    "refusalReason": "ood"
  }
}
```

`decisions` is empty, `refused` is `true`, and `refusalReason` is one of
`"ood"`, `"interval_too_wide"`, or `"insufficient_calibration_data"`. Your
service is expected to fall back to its default behaviour when it sees
`refused: true`. The integration library shown in Step 7 does exactly this.

Disable refusal again by PUTting `"refusal": {"enabled": false}` if you
don't want it on for the next step.

## Step 7: adopt it from a real service

The canonical Python integration is
[`../examples/retry-tuning/`](../examples/retry-tuning/) — a drop-in
`requests`-wrapping client that picks an HTTP retry policy per request via
Syntra. It is the reference shape for any Syntra adoption: thin wrapper
around your existing call path, decide on the way in, feedback on the way
out, hard fallback to a static policy whenever Syntra is unreachable,
refuses, or returns malformed data.

Install and use it against your capsule:

```bash
cd Syntra/examples/retry-tuning
pip install -e .

export SYNTRA_ADMIN_KEY=$LYCAN_ADMIN_KEY
python3 -c '
import os
from syntra_retry import RetryClient

client = RetryClient(
    syntra_url=os.environ["SYNTRA_URL"],
    capsule_path="/tenants/myteam/jobs/routing/capsules/router",
    admin_key=os.environ["SYNTRA_ADMIN_KEY"],
)
response = client.request("GET", "https://httpbin.org/status/200")
print(response.status_code)
'
```

Read [`../examples/retry-tuning/README.md`](../examples/retry-tuning/) for
the integration's full feature set, customization points, and the seven
unit tests that exercise the fail-safe paths. Other adoption examples are
landing under `Syntra/examples/` (`fraud-tuning/`, `queue-selection/`,
`syntra-go/`, `syntra-node/` — see those directories if present in your
checkout).

## Where to go next

The pattern is now in your hands. Real adoption is a matter of identifying
the discrete decision points in your service, declaring a feature schema
that captures the context that ought to drive the choice, and standing up
a thin client that talks `/decide` and `/feedback`. The hard part is almost
always the reward function — what does "this was a good choice" mean in
your domain, measured how, on what delay. Get the reward right and Syntra
will figure out the rest.

When you ship: read [`operating.md`](operating.md) before turning the
adaptive layer authoritative. Until then, shadow mode is your friend.
