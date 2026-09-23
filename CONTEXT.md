# Architecture notes

Syntra is a self-hosted decision service. An application sends context to
`/decide`, a compiled Lycan capsule picks one of a fixed set of actions, and
the application later reports an outcome to `/feedback`. Syntra keeps the
learned state and the decision, feedback and audit logs in a filesystem
store.

## Request path

```text
application
  -> HTTP API (src/server/routes.rs, auth in src/server/auth.rs)
  -> tenant / job / capsule lookup (src/store.rs)
  -> policy load, fail closed to deny-all (src/context.rs)
  -> compiled Lycan graph execution (src/graph_executor/)
  -> decision log + audit log
  -> later: /feedback joins the decision and updates learned state
```

- `src/server/decide.rs` loads the capsule, encodes context, selects an
  option and appends the decision log.
- `src/server/feedback.rs` resolves a `decisionId` from the decision log and
  applies the reward.
- `src/capabilities/` holds the native kernels and the sandbox
  (`sandbox.rs`): file access is rooted in the capsule's `data/` directory,
  outbound HTTP needs an allow-listed host, https, and a public address.
- `src/store.rs` owns the on-disk layout described in the README.

## Language core

Lycan source (`.lycs`) compiles to a graph binary (`.lyc`) that the
verifier checks before execution. The normative spec is `docs/lycan/spec/`,
and `tests/conformance_vectors.rs` checks the binary formats byte for byte.

## Known limitations

These are open in the current release and tracked for the v2 decision core:

- Decision logs do not record the probability of the chosen action, so
  logged traffic cannot support unbiased off-policy evaluation.
- `syntra replay` compares a candidate with the baseline using per-action
  rewards supplied in the log. Real traffic only has the reward of the
  action taken, so the promotion gate is only meaningful on simulated logs.
- Several learners coexist on the server path (graph weights, per-context
  buckets, a candidate portfolio under a meta-bandit, LinUCB state keyed by
  a hash of the rounded feature vector), and the configured algorithm can
  be overridden. Feature-context capsules do not generalize across unseen
  feature vectors.
- The store is single-process. `/feedback` scans the decision log to find a
  decision, so its cost grows with log size.

See [SECURITY.md](SECURITY.md) for the security model and
[ROADMAP.md](ROADMAP.md) for planned work.
