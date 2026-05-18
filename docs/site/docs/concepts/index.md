# Concepts

Six concept pages, each focused on a single piece of how Syntra works.
Read them in order on a first pass, dip into them as reference after.

- [**Capsule**](capsule.md) — the unit of installation: a Lycan program,
  a sidecar of state, an HTTP path, and one audit log.
- [**Kernel**](kernel.md) — the 26 sandboxed building blocks a capsule
  can call. Stats, forecast, autoscale-recommend, HTTP, SQL, file I/O.
- [**Strategy node / choice node**](strategy-node.md) — the bandit-driven
  decision point inside a graph. One node per `/decide`, K options, one
  picked per request.
- [**Meta-bandit**](meta-bandit.md) — seven candidate algorithms run in
  parallel; the meta-layer converges on whichever performs best on your
  capsule's actual traffic.
- [**Drift detection**](drift.md) — ADWIN at two scopes. Capsule-level
  detector re-warms the whole learner; per-context detector resets just
  the affected bucket.
- [**Refusal**](refusal.md) — opt-in confidence-based abstention. When
  the input is OOD or the prediction interval is too wide, `/decide`
  returns `{"refused": true, ...}` so your service falls back.

If you want the underlying theory — what a contextual bandit is, why
delayed feedback matters, when this framing fits and when it does not
— the [contextual bandits in honest terms](https://github.com/ashhart/Syntra/blob/main/docs/concepts.md)
doc in the repository is the long-form treatment.
