# Cookbook

Task-shaped recipes for things operators and integrators actually do
once Syntra is running. Each recipe is self-contained,
copy-pasteable, and resolves in under five minutes.

## Available recipes

- [**Wiring up delayed feedback**](cookbook/wiring-delayed-feedback.md)
  — the `decisionId` persistence pattern for outcomes that resolve
  hours or days after the decision.

## Planned recipes (not yet written)

!!! note "Stubs below"

    The recipes listed below are planned but unwritten. The closest
    existing material is in the [domain packs](../examples/index.md)
    — each one is a worked capsule end-to-end and probably already
    demonstrates what your recipe was going to do.

### Authoring & install

- [ ] **Author a single-decision capsule from YAML** — minimum
      viable `capsule.yaml`, `syntra author` invocation, install.
- [ ] **Author a multi-decision capsule** — two strategy nodes,
      different reward signals, `decisionIndex` on feedback.
- [ ] **Add a feature-context spec** — convert a discrete-context
      capsule to features without losing learned state.
- [ ] **Add per-option action features** — opt into shared-state
      LinUCB.
- [ ] **Add a hierarchical option tree** — convert a flat 20-option
      capsule into a 5×4 hierarchy.
- [ ] **Custom reward spec with normalizers** — quality / latency /
      cost components, range normalization.

### Integration

- [ ] **Shadow-mode integration without behavior change** — call
      `/decide`, log the choice, still apply your existing policy.
- [ ] **Cut over from shadow mode to authoritative** — gate by
      capsule lifecycle state.
- [x] [**Wire feedback from a delayed pipeline**](cookbook/wiring-delayed-feedback.md)
      — pattern for Kafka / SQS / database-polling reward arrival.
- [ ] **Pin a decision to a customer** — passing through
      `contextKey` for per-customer isolation without exploding the
      tenant tree.
- [ ] **Handle refusal in three failure modes** — unreachable /
      refused / malformed; one fallback path.

### Operations

- [ ] **Migrate a capsule across tenants** — backup / restore the
      learned state.
- [ ] **Force a re-warmup** — when you know the regime has shifted
      faster than ADWIN will catch.
- [ ] **Freeze a capsule for review** — what `safety.freezeLearning`
      changes and what it doesn't.
- [ ] **Read meta-bandit health** — what to look for in `/memory`
      output to know whether the meta-layer is healthy.
- [ ] **Backup / restore via JSON bundles** — current pre-1E pattern
      via store volume copy.
- [ ] **Run behind a TLS proxy** — minimal nginx or Caddy config.

### Debugging

- [ ] **"Weights aren't moving"** — checklist: lifecycle state,
      feedback arrival rate, reward shape, drift events.
- [x] [**"All decisions are being refused"**](operations/debugging-refusals.md)
      — calibrator readiness, `oodThreshold` tuning, `coverage`
      interaction. (Cross-linked from the operations runbook.)
- [ ] **"LinUCB is being picked but the meta-bandit isn't converging"**
      — feature scaling, dead features, OOD on the feature side.
- [ ] **"`/decide` is slow"** — typical causes: large `memory.json`,
      sandboxed HTTP timeout, expensive `runtime.publish` chains.

## Contributing a recipe

Until the cookbook is filled in, the fastest path to a recipe is to
add it directly to the relevant example pack or concept page. When
the cookbook is populated, recipes should follow this shape:

- **Title** — task verb.
- **One-paragraph "why"** — the problem this recipe solves.
- **Steps** — numbered, with the actual commands.
- **Verification** — how to confirm it worked.
- **Where this fits** — links to the relevant concept and example
  pages.
