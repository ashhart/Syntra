# Migrating from Vowpal Wabbit's contextual-bandit core to Syntra

This guide is for teams already using Vowpal Wabbit (`--cb_explore`,
`--cb_adf`) as their contextual-bandit backbone and evaluating whether Syntra
is worth a switch. The honest framing: VW is faster for raw bandits in
streaming mode; Syntra wins on operational shape. Both are real, and this
guide tries to be precise about which axis matters for your use case.

## Background reading

- [Bandit primer](../concepts.md) — contextual bandits, delayed feedback,
  exploration/exploitation tradeoffs, reward function design.
- [30-minute walkthrough](../tutorial.md) — Syntra end-to-end: author, install,
  configure, drive, inspect.
- [API reference](../api.md) — every HTTP endpoint with request and response
  shapes.
- [Canonical Python integration](../../examples/retry-tuning/) — the reference
  integration pattern and fail-safe fallback shape.
- [Offline evaluation](../../examples/offline-eval/) — if that directory is
  present in your checkout, it contains IPS/DR estimators you can use to
  compare your existing VW policy against a Syntra policy on historical logs
  before going live.

---

## Comparison table

| Dimension | Vowpal Wabbit | Syntra |
|---|---|---|
| Deployment shape | Single binary, in-process or pipe | HTTP appliance (Docker container), network call per decision |
| State storage | In-memory; snapshot via `--save_resume` or `--readable_model` | Persistent JSON sidecar files; `memory.json` survives container restarts |
| Multi-tenancy | Manual (separate processes or weight files) | Native: `tenant / job / capsule` hierarchy |
| Algorithm selection | Caller picks (`--cb_explore`, `--cb_adf`, `--epsilon`) | Meta-bandit runs six candidates in parallel, converges automatically |
| Drift detection | Not built-in; you wire ADWIN separately | Two-layer ADWIN built in: capsule-level and per-context |
| Refusal semantics | No native concept | Optional: `/decide` returns `refused: true` when OOD or interval too wide |
| Latency | Sub-millisecond in-process | One HTTP round-trip; ~1-5ms on localhost; ~10-30ms over LAN |
| Feature format | VW text format with namespaces and interactions | JSON feature vector; continuous, categorical, cyclic kinds declared in schema |
| Online streaming | Native; designed for it | Native; `/decide` and `/feedback` are stateless HTTP |
| Interaction features | `f1*f2` product features via namespace spec | Not natively supported (known gap; see below) |
| Audit trail | `--audit` flag output; ephemeral unless you capture it | Append-only `decision.jsonl`, `feedback.jsonl`, `audit.jsonl` on disk |
| Dashboard | None built-in | Browser UI at `/admin` with strategy weights, context viewer, audit log |

This is a genuine tradeoff table, not a marketing table. VW's raw throughput
is higher in every measurable way. The question is whether throughput is the
constraint.

---

## What VW does better

**Raw throughput and streaming throughput.** VW's in-process model prediction
runs in microseconds. If you are handling hundreds of thousands of decisions
per second and latency is your hard constraint, an HTTP hop to Syntra —
even on localhost — adds meaningful overhead. VW is the right answer for that
shape of problem.

**Online streaming with namespace features.** VW's text-format input supports
namespaces, quadratic and cubic feature interactions, and a broad suite of
reduction algorithms. If you are already exploiting `--nn` or
`--lrq` or complex feature engineering in VW's format, that feature richness
does not exist in Syntra today.

**Direct access to the weight vector.** VW exposes the learned weight vector
directly, which makes it easy to inspect or export the model for offline
analysis. Syntra exposes learned weights through `/memory` and `/report`, but
the format is specific to Syntra's internal structure and not compatible with
other tooling.

**Algorithm depth.** VW supports a wider menu of reduction algorithms than
Syntra's six meta-bandit candidates. If you have a specific algorithmic reason
to use `--squarecb`, `--cover`, or one of VW's neural-network reductions,
Syntra doesn't have those.

---

## What Syntra does better

**State survives restarts without ceremony.** VW requires explicit
`--save_resume` configuration and you have to manage the checkpoint lifecycle
yourself. Syntra's `memory.json` is written atomically on every feedback round
and pre-mutation snapshots are kept under `snapshots/`. A container restart
does not lose the learned state. For teams running in Kubernetes or other
environments where containers are ephemeral, this difference is significant.

**Multi-tenant isolation without process management.** Running separate VW
bandit instances for multiple tenants or jobs means managing separate processes,
separate weight files, separate training pipelines, and some kind of routing
layer in front. Syntra's `tenant / job / capsule` hierarchy provides native
isolation: separate memory, separate logs, separate policy configuration, per
path. Adding a new tenant is a POST request.

**Drift detection is on by default.** Syntra runs capsule-level and per-context
ADWIN detectors without any configuration. When reward distribution shifts
globally, the capsule re-warms and re-characterizes the problem. When a single
context bucket drifts while others are stable, only that bucket resets. With
VW, drift detection is your responsibility to wire outside the bandit.

**Refusal semantics.** When the bandit is uncertain — out-of-distribution input,
prediction interval too wide — Syntra can return `{"refused": true}` instead of
a low-confidence option. Your service code handles the refused response by
falling back to a safe default. This is a safety gate that VW does not have; in
VW, low-confidence decisions look the same as high-confidence ones unless you
implement your own confidence estimation layer.

**Auto algorithm selection.** You don't have to pick `--cb` versus `--cb_adf`
versus `--epsilon`. Syntra's meta-bandit runs Thompson sampling, UCB1,
EpsilonGreedy, Weighted, Greedy, and (for feature-context capsules) LinUCB in
parallel and converges on whichever performs best on your traffic. For teams
that don't want to tune algorithm hyperparameters, this is the right default.

**Operational surface.** The `/admin` dashboard, the structured JSON logs, the
`/audits` endpoint that records every install, policy change, drift event, and
refusal — these exist so an on-call engineer can diagnose a weight anomaly
without reading source code. VW's operational interface is what you build
yourself.

---

## Feature compatibility

Syntra's `contextSpec.features` corresponds roughly to VW's `--cb_features` or
namespace feature vectors. The mapping is approximate:

| VW concept | Syntra equivalent |
|---|---|
| Continuous scalar feature | `{"kind": "continuous", "range": [min, max]}` |
| Categorical/discrete feature | `{"kind": "categorical", "values": ["a","b","c"]}` — one-hot encoded |
| Cyclic/periodic feature (e.g., hour) | `{"kind": "cyclic", "period": 24.0}` — sin/cos encoded |
| Namespace (feature group) | No direct equivalent; flatten to named features |
| Interaction feature `f1*f2` | Not supported (see known gap below) |
| Missing/unknown feature | Syntra's LinUCB defends against NaN/Inf but cannot rescue a feature that doesn't predict |

A Syntra feature-context capsule enabling LinUCB is the closest operational
equivalent to VW's `--cb_adf` with a feature namespace. The learning config
declaring those features would look like this for a typical retry-tuning case:

```bash
curl -X PUT $SYNTRA_URL/tenants/myteam/jobs/retry/capsules/router/learning \
  -H "Authorization: Bearer $LYCAN_ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "contextSpec": {
      "type": "features",
      "features": [
        {"name": "recent_failure_rate", "type": {"kind": "continuous", "range": [0.0, 1.0]}},
        {"name": "p99_latency_ms",      "type": {"kind": "continuous", "range": [0.0, 5000.0]}},
        {"name": "hour",                "type": {"kind": "cyclic", "period": 24.0}}
      ]
    }
  }'
```

If your VW model is trained purely on continuous features without interactions,
the port is straightforward. If it relies on quadratic or cubic interactions,
you will lose that expressivity in Syntra today.

---

## Migration path

### Step 1: port the capsule definition

Your VW model encodes its options implicitly in the action set passed to
`--cb_adf`. In Syntra, options are declared explicitly in a capsule YAML. For
each action in your VW model, add a string option:

```yaml
name: my-bandit
options:
  - option_a
  - option_b
  - option_c
reward:
  type: continuous
  range: [-1.0, 1.0]
```

Compile it:

```bash
syntra author my-bandit.yaml --out-dir ./my-capsule/
```

Then install and attach the feature schema as shown above. If your VW model
uses a binary reward (0/1), declare `range: [0.0, 1.0]` instead. If it uses
a signed reward, use `[-1.0, 1.0]`.

### Step 2: validate convergence equivalence with simulate

Before replaying real traffic, use `syntra simulate` to verify that Syntra
converges to the same arm your VW model converges to on synthetic data with
known ground truth:

```bash
syntra simulate my-bandit.yaml \
  --rounds 5000 \
  --true-arm-rewards "0.2,0.5,0.7" \
  --seed 7
```

The `--compare-vw` flag (if available in your build; check `syntra simulate
--help`) allows passing a reference VW model file and printing convergence
divergence across rounds. The goal is not identical decisions — the algorithms
differ — but similar arm preferences after warmup on a stationary problem.

### Step 3: deploy Syntra in shadow mode alongside VW

Run Syntra as a sidecar. Keep VW authoritative. Send the same decision context
to both; use VW's decision in production; post `/feedback` to Syntra with the
outcome from VW's decision. This is shadow mode for the bandit: Syntra learns
from the propensity-weighted outcome of VW's decisions without influencing
live behaviour.

The offline-eval tooling in
[`../../examples/offline-eval/`](../../examples/offline-eval/) (see that
directory if present) can take the shadow-mode decision log and produce IPS
and Doubly Robust estimates comparing VW's live policy against the Syntra
policy that emerged in shadow — accounting for the propensity mismatch between
what VW chose and what Syntra would have chosen. This gives you an apples-to-
apples reward estimate before you flip traffic.

### Step 4: shift traffic and deprecate VW

Once shadow-mode comparison shows parity or improvement, shift a fraction of
traffic to Syntra (by setting `shadow=False` in the integration client for
that fraction) and compare live performance. When you are satisfied, remove
the VW integration.

---

## Known gap: interaction features

VW's quadratic and cubic interaction features — declared via `--quadratic`,
`--cubic`, or the namespace interaction syntax — are not natively supported in
Syntra's current feature schema. `contextSpec.features` accepts individual
named features; if you have `f1*f2` interactions that are important to your
model's accuracy, you have two options:

1. Precompute the interaction as an explicit feature in your application before
   sending it to `/decide`. This is mechanical but it works.
2. Stay on VW for the cases where interactions are the key predictive signal
   and use Syntra for cases where your feature set is interaction-free.

This is a genuine limitation, not a configuration gap. If interaction features
are central to your use case, Syntra is not ready for you today.

---

## Honest summary

If you are running VW for raw throughput or for the breadth of its algorithmic
menu, the switch to Syntra will cost you something real. If you are running VW
and also maintaining your own drift detection, your own state persistence
layer, your own multi-tenant routing, and your own confidence-estimation gate —
and those systems are consuming more engineering time than the bandit itself —
then Syntra's operational shape is the thing worth evaluating.

The migration path does not require a big-bang cutover. Shadow mode exists
precisely so you can run the comparison before committing.

For endpoint details, see [api.md](../api.md). For what shipped in each
platform phase and the known debt list, see [CHANGELOG.md](../../CHANGELOG.md).
