# Migration guides

If you have an existing adaptive layer or set of static rules, Syntra
is most useful as a *direct replacement* — same hot path, same
fallback, same callers. These three guides cover the most common
starting points.

## Available migration guides

- [**From static rules**](migration/from-static-rules.md) — full
  before/after walkthrough with capsule YAML, integration code,
  and operational notes on what the migration changes.

## Other migration paths (not yet written)

!!! note "Stubs below"

    The migration paths below have a one-paragraph sketch each.
    Detailed step-by-step guides — including the "before" snippets,
    the conversion checklist, and the rollback path — will land in
    a later release.

## From static rules (short overview)

You have hand-tuned `if/elif/else` logic mapping request features to
one of N labelled policies, and you want to let a bandit learn the
mapping from outcomes instead.

The shape of the migration: keep the N labels, keep the policy
implementations, replace the `if/elif/else` with a `/decide` call,
record an outcome via `/feedback` once the result resolves. Shadow
mode (call `/decide`, log the answer, keep doing what you were doing)
lets you verify the bandit is suggesting something sensible before
letting it influence live behaviour.

The minimum viable migration is roughly fifty lines of code: a
`syntra_*` client wrapping the existing call site, a one-time
`setup_capsule.py` style install of the capsule with the N options,
and an outcome reporter on whatever pipeline currently knows whether
the decision worked. Worked examples for retry, fraud thresholds,
queue selection, and LLM routing are the four
[integration packs](../examples/index.md#integration-packs).

**To be expanded:**

- [ ] Before/after diff for a representative `if/elif/else`.
- [ ] How to pick the option labels (granularity, naming, how to
      add later without resetting learned state).
- [ ] Shadow-mode evaluation pattern with offline regret estimate.
- [ ] Cutover checklist.
- [ ] Rollback to the static rules without losing the learned
      state (so you can retry the cutover later).

## From Vowpal Wabbit

You have a Vowpal Wabbit contextual-bandit deployment — `--cb_explore`,
`--cb_adf`, IPS / DR off-policy evaluation already wired up — and
you are evaluating Syntra as a replacement.

The shape of the migration: Syntra ships fewer knobs and more
opinions. The meta-bandit picks the candidate algorithm for you
(VW's `--cb_type` becomes a per-capsule runtime choice). Reward is
posted by `decisionId` after the fact instead of in the same call
that picked the arm. The persistent store is local-filesystem
JSON / JSONL instead of a VW model file.

What you keep: the feature engineering work. Your VW
namespaced-features can map to Syntra's `contextSpec: features`. The
IPS / DR machinery that VW exposes for off-policy evaluation has a
direct analog in
[`examples/offline-eval/`](https://github.com/ashhart/Syntra/tree/main/examples/offline-eval).

What you give up: the action-dependent features (`--cb_adf`) flavor
maps to Syntra's [shared-state action embeddings](../examples/shared-state-action-embeddings.md)
in spirit but not in tooling — VW's online learning over an
arbitrary number of dynamically-presented actions is not what
Syntra is shaped for. If you need that exact flavor, Syntra is the
wrong tool.

**To be expanded:**

- [ ] Feature namespace → `contextSpec.features` translation table.
- [ ] VW `--cb_type ips` / `mtr` / `dm` / `dr` mapping to Syntra's
      meta-bandit candidate set.
- [ ] Off-policy eval workflow comparison (VW `--audit`,
      `--predict` vs Syntra's `decision.jsonl` + `examples/offline-eval/`).
- [ ] Where VW will keep being the better tool (extremely high
      cardinality, action-dependent features, online updating with
      no `/feedback` round-trip).

## From a custom bandit implementation

You have a Thompson sampling or UCB1 implementation living in your
service code, posted feedback rolling into it directly, and you
want to extract it into a proper appliance.

The shape of the migration: your custom bandit becomes one capsule.
The state your code holds in memory becomes `memory.json`. The
log line you write on every decision becomes `decision.jsonl`. The
reward update step becomes the `/feedback` round-trip. The
algorithm you implemented becomes one entry in Syntra's meta-bandit
candidate set — and the meta-bandit may converge on a different one
than you implemented, which is the point.

The risk in this migration is mostly cultural rather than
technical: a custom bandit feels closer to "your code" than an
appliance does. The benefit of giving it up is that the audit
trail, refusal, drift detection, multi-decision capsules,
hierarchical and shared-state flavors, and the operator console all
come along for free.

**To be expanded:**

- [ ] State extraction guide — taking your in-memory weights and
      bootstrapping a Syntra capsule from them without losing
      learned mass.
- [ ] Reward function audit — using `examples/offline-eval/` to
      sanity-check that your existing reward is monotonic in what
      you actually care about.
- [ ] Algorithm parity check — confirming the meta-bandit
      reproduces your custom bandit's behaviour on replayed
      traffic.
- [ ] Decision-log cutover — running both side-by-side during the
      migration window, then retiring the custom path.
