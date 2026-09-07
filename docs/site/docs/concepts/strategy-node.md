# Strategy node / choice node

A **strategy node** (sometimes called a **choice node** or `AdaptiveChoice`
in the runtime) is the bandit-driven decision point inside a capsule's
graph. It is the place where the program stops computing features and
starts asking "given everything I have derived so far, which of these K
labelled options should I return?"

Every `/decide` call hits at least one strategy node. The node samples
one option, the capsule's program builds the response, the decision is
written to `decision.jsonl`, and the response is returned to the
caller. When `/feedback` later arrives, the bandit credits the option
the node picked and updates its weights.

## What a strategy node looks like in YAML

In the simplest authoring path (YAML → `syntra author` → `.lyc`), the
strategy node is implicit: you declare the option list and a reward
shape, the compiler emits a graph with one strategy node fed by
`runtime.input`.

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

This compiles to a graph with one `AdaptiveChoice` node holding three
options. The bandit's weight vector lives in the capsule's `memory.json`,
indexed by `nodeId` and `contextKey` (or the feature vector, in the
feature-context flavor).

For an operational-intelligence capsule that computes its own features,
the strategy node lives at the end of the `.lycs` source, after the
`stats.*` / `series.*` / `ops.*` calls have shaped the inputs.

## Multi-decision capsules

A capsule can declare multiple strategy nodes via the `decisions[]`
list. On `/decide`, the runtime walks each node independently:

```yaml
decisions:
  - name: retry_policy
    options: [none, single, triple, exponential_fast, exponential_slow]
  - name: timeout_ms
    options: [100, 250, 500, 1000, 2500]
```

Each node runs its own meta-bandit and embeds its chosen
`candidateId` in the decision log. `/feedback` accepts a
`decisionIndex` so you can credit the right node when the outcome is
specific to one of them.

## The three adaptive flavors

The kernel side of the program — what features the capsule computes —
is orthogonal to the **bandit side** of the strategy node, which has
its own structural choice: three flavors of adaptive layer, all
reachable through the same `/decide` / `/feedback` API. The runtime
auto-detects which flavor a capsule uses from its installed sidecars
and `learning.json`.

### 1. Meta-bandit over per-option LinUCB (default)

Seven candidate algorithms run in parallel — Thompson, UCB1,
EpsilonGreedy, Weighted, Greedy, LinUCB, LinTS — and a rate-adaptive
[meta-bandit](meta-bandit.md) converges on whichever performs best on
this capsule's traffic. Every capsule that doesn't explicitly opt into
another flavor gets this. Best fit when the N options are independent
(no shared semantic structure) and you don't have prior knowledge
about which algorithm should win.

### 2. Shared-state LinUCB

For capsules whose options carry **semantic similarity** — LLM models
with similar token costs and capabilities, server flavours with
graduated CPU/RAM, retail variants of the same SKU. Enable by setting
`sharedState.enabled = true` in `learning.json` and supplying
`optionFeatures: { name -> [f64; d] }`. The runtime then maintains a
single θ over `[x_context, x_option]` rather than one θ per option.

The payoff: new options added later inherit a non-zero prior from
their action-feature vector alone. No separate cold-start per option.
See the [shared-state action embeddings demo](../examples/shared-state-action-embeddings.md).

### 3. Hierarchical bandits

For capsules whose action space **factors into a tree** — region ×
server-type, segment × creative, country × language. Enable by
declaring `hierarchical_options:` in `capsule.yaml`. The runtime walks
the tree at decide time using one meta-bandit per `HierState` (root +
per-branch), picks one option per level, and resolves to a leaf action.
`/feedback` propagates the observed reward to every level along the
recorded path.

Useful when the action space factors naturally — e.g., 5 regions × 4
server types is 20 leaves, but only 5 region-level decisions matter
most of the time. See the [hierarchical region routing demo](../examples/hierarchical-region-routing.md).

## What the node returns

The `/decide` response carries one block per strategy node in
`decisions[]`. For a single-node capsule:

```json
{
  "decisions": [
    {
      "node_id": 70,
      "chosen_option": 1,
      "confidence": 0.8345,
      "objective": "general",
      "weights": [0.1557, 0.8345, 0.0098],
      "activations": 42,
      "candidateId": "LinUcb"
    }
  ]
}
```

The fields integration libraries care about:

- `chosen_option` — zero-based index into the option list.
- `weights` — current strategy weights over the options. During Warmup
  these are uniform.
- `candidateId` — which of the seven meta-bandit candidates served this
  decision. Appears once the capsule is Active.
- `confidence` — the weight of the chosen option. Useful for adaptive
  throttling on top of refusal.

For hierarchical capsules the block carries `path: [int]`,
`leafName: string`, and `perLevelCandidateIds: [string]` instead of
`chosen_option`.

## What a strategy node is not

- **Not a classifier.** It does not output a label conditioned on a
  feature vector by learning a global mapping. It samples from a
  per-context bandit policy.
- **Not deterministic.** The default selection mode is greedy with
  `min_exploration` floor; every algorithm in the meta-bandit's
  candidate set has its own exploration step. Two `/decide` calls with
  identical inputs can return different options.
- **Not the only decision point.** A capsule can have multiple strategy
  nodes in series or in parallel. Each runs its own bandit.
- **Not free of cold-start.** A fresh option needs samples before its
  weight diverges from the uniform prior. The meta-bandit's
  EpsilonGreedy / Greedy candidates carry the early exploration; LinUCB
  / LinTS take longer to differentiate but reward features that matter.

## Where to go next

- [Meta-bandit](meta-bandit.md) — what runs *inside* the strategy node
  in Active state.
- [Drift detection](drift.md) — what re-warms a strategy node when its
  reward distribution shifts.
- [Refusal](refusal.md) — when a strategy node returns no decision at
  all.
- [Capsule](capsule.md) — how strategy nodes compose with kernels into
  a single installable thing.
