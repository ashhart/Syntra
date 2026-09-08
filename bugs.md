# Syntra Bug Review — 2026-09-07

Findings from a systematic review of the decision/learning/executor paths.
Each entry: symptom, evidence (repro command + observed output), root cause,
and fix. All bugs below were reproduced before fixing (red), then verified
green after the fix.

## BUG-1 — Invalid feedback advances the warmup lifecycle

**Severity:** High (state corruption from invalid input)

**Symptom:** `POST /feedback` with a `decisionId` that does not exist returns
404, but still counts toward the capsule's warmup sample target. 30 bogus
feedbacks flip a capsule from `warmup` to `active` and pick a
characterization/algorithm from garbage rewards.

**Repro (red):**
```bash
# install examples/demo_llm_model_router.lyc, then:
for i in $(seq 1 30); do
  curl -X POST .../feedback -d "{\"decisionId\":\"dec_bogus_$i\",\"reward\":1.0}"
done
# every response: 404
curl .../report   # warmup: {"state":"active","characterization":"Binary { positive_rate: 1.0 }"}
```

**Root cause:** `src/server/feedback.rs` — `do_feedback` (flat path) and
`do_feedback_hierarchical` call `warmup_state.record_feedback(reward)` and
`save_warmup_state_in_job` **before** the `decisionId` lookup that returns
404. Same ordering issue in the hierarchical path.

**Fix:** Move the warmup record/save to after the decision lookup and option
validation succeed (i.e., only record feedback that will actually be applied).
The batch endpoint inherits the fix because it calls `do_feedback` per event.

## BUG-2 — Verifier accepts malformed strategy graphs; executor panics

**Severity:** High (remote DoS via crafted capsule; fail-closed violation)

**Symptom:** A `.lyc` whose `Strategy` node has `weights.len() > operands.len()`
passes `verifier::verify` and then panics the executor with
`index out of bounds: the len is 2 but the index is 2` at
`src/graph_executor/exec.rs:349` (`results.remove(best_idx)` in the
`SameOutput` branch). In the server, `/install` accepts arbitrary bytes and
`/decide` runs the verifier then the executor with no `catch_unwind` — a
crafted capsule kills a worker thread; 8 requests exhaust all workers.

**Repro (red):** `tests/repro_sameoutput.rs` (throwaway at time of finding):
- `verifier result: Ok("OK")` — verifier accepts 3 weights / 2 operands
- executor panics: `index out of bounds: the len is 2 but the index is 2`

**Root cause:** `src/verifier.rs` never validates that a
`Strategy`/`AdaptiveChoice` node's `weights.len()` matches its operand count
(`operands.len()` for `None`/`SameOutput`/`Validated`, `operands.len()+1` for
`WithinTolerance` which stores epsilon in the last slot). The executor's
`SameOutput` branch then indexes `results` (sized by `n_options`) with an
index derived from the full `weights` vector.

**Fix:**
1. `verifier.rs`: reject `Strategy`/`AdaptiveChoice` nodes where
   `weights.len() < operands.len()`, or where `WithinTolerance` and
   `weights.len() != operands.len() + 1`.
2. `graph_executor/exec.rs`: clamp `best_idx` to `results.len() - 1` before
   `results.remove` (defense in depth; never panic on malformed input).
3. Regression test: the crafted graph must be rejected by the verifier and,
   if it ever reaches the executor, must not panic.

## BUG-3 — `find_decision_in_job` resolves decision IDs by substring

**Severity:** Medium (wrong-decision credit)

**Symptom:** `store.find_decision_in_job` scans the decision log with
`line.contains(decision_id)`. A feedback for `dec_abc` can match a different
decision whose serialized JSON contains `dec_abc` as a substring (e.g.
`dec_abcdef12345678`), crediting the reward to the wrong decision. The scan is
newest-first, so the most recent unrelated decision wins.

**Root cause:** `src/store.rs` — `find_decision_in_job` (and the legacy
`find_decision`) use substring matching instead of parsing the event and
comparing the `id` field exactly.

**Fix:** Parse each line as JSON and compare `event["id"] == decision_id`
exactly (fall back to substring only for pre-v2 log lines that lack an `id`).

## BUG-4 — Read-scoped token can mutate learned policy via `?learn=true`

**Severity:** Medium (privilege escalation on a read-only credential)

**Symptom:** `Scope::Read` authorizes `CapsuleDecide`. `POST /decide?learn=true`
persists the graph (weight updates) and memory sidecar. A read-only token can
therefore permanently alter a capsule's learned policy — contradicting the
scope's name and the `read_token_can_read_and_decide_only_its_capsule` test's
intent (read + decide without mutation).

**Root cause:** `src/server/routes.rs` — the decide route derives
`learn` purely from the URL (`url.contains("learn=true")`) and never checks
the granted scope.

**Fix:** When the granted scope is `Scope::Read`, force `learn=false`
(Admin/TenantAdmin keep the URL flag). Read tokens can still use the capsule
and append decision logs; they cannot mutate policy.

## BUG-5 — Flat feedback weight update tracks success flux, not mean reward

**Severity:** High (learner converges to the wrong arm under stochastic rewards)

**Symptom:** In the adaptive clinical-trial demo (2 contexts × 3 treatments,
Bernoulli outcomes via `decisionId` feedback), the runtime poured patients
into the inferior arm: in `mild`, where the true best is A (.65 vs B .45),
allocations landed at [21, 96, 15] for A/B/C, and the trial's observed
responses fell *below* a fixed 1:1:1 control. A deterministic-reward probe
learns perfectly (`mild → [0.967, 0.016, 0.016]`) — the defect only
appears when outcomes are stochastic.

**Root cause:** the four flat external-feedback update sites —
`src/server/feedback.rs` (graph node weights), `src/learning/feedback.rs`
(context bucket weights **and** `OptionState::Weighted`), and
`src/bin/lycan.rs` (CLI feedback) — used `w[chosen] += lr * reward`, with
zero movement on failure (reward 0 ⇒ no-op). Normalized weights then
track cumulative *success flux* (rate × allocation), not mean reward: an
arm that briefly leads gets pulled more, books more absolute successes,
and self-confirms — rich-get-richer with equilibrium `w ∝ 1/p`. The
hierarchical learner (`src/hierarchical_state.rs:214`) already used the
correct mean-seeking rule, and the code's own comment calls the weight "a
current estimate" — the intended semantics was mean; the implementation
was flux.

**Fix:** mean-seeking rule at all four sites:
`delta = clamp(lr * (reward − w[chosen]), ±maxWeightDeltaPerFeedback)`,
complementary updates unchanged. Weights converge to mean reward; a
reward of 0 now lowers the chosen arm instead of being a no-op. Verified:
Bernoulli probe converges per context (`mild [.574, .253, .174]`,
`severe` B leads at .603); the seeded trial beats the fixed control by
+22–45% observed responses across seeds 7/11/42. Integration tests
`test_feedback_*` were re-pinned from old-rule arithmetic (`0.55`/`0.45`/
`0.65` string matches) to direction assertions; `docs/lycan/learning.md`
documents the rule.

## Verified non-issues (checked, no action)

- `backup.rs` restore: rejects absolute paths, `..`, and empty paths before
  staging — no traversal.
- `auth_tokens.rs`: tokens are SHA-256 hashed at rest; scoped `allows()` is
  fail-closed.
- `rate_limit.rs`: per-principal token buckets, correct refill math; map grows
  unboundedly but is bounded by admin-issued tokens.
- `conformal.rs` / `change_detection.rs` / `ood.rs` / `learning/stats.rs`:
  math checked against tests; no defects found.
- `meta_bandit.rs`: exploration decay, tie-break, forgetting all correct.

---

## Fix status — all five fixed and verified
| Bug | Fix | Regression test | Verified |
|-----|-----|-----------------|----------|
| BUG-1 | `src/server/feedback.rs`: warmup record/save moved after decision lookup + option validation (flat and hierarchical paths) | `tests/bugfix_regressions.rs::bogus_feedback_does_not_advance_warmup` | Original repro now shows `warmup` (0/30) after 30 bogus 404s |
| BUG-2 | `src/verifier.rs`: reject Strategy/AdaptiveChoice with `weights.len() != operands.len()` (`+1` for WithinTolerance); `src/graph_executor/exec.rs`: clamp `best_idx` to `results.len()-1`, clamp weight loop to `n_options` | `tests/verifier_strategy_weights.rs` (4 tests) | Verifier rejects malformed graph; executor no longer panics |
| BUG-3 | `src/store.rs::find_decision_in_job`: exact JSON `id` match (substring fallback only for unparseable legacy lines) | `tests/bugfix_regressions.rs::find_decision_matches_exact_id_not_substring` | Prefix-collision case resolves to the exact-id decision |
| BUG-4 | `src/server/routes.rs`: Read-scoped tokens force `learn=false` on both decide routes | `tests/bugfix_regressions.rs::read_token_cannot_mutate_policy_via_learn` | Read token + `?learn=true` returns `learned:false`; admin keeps `true` |
| BUG-5 | Mean-seeking weight update at all four flat feedback sites (`server/feedback.rs`, `learning/feedback.rs` ×2, `bin/lycan.rs`), matching `hierarchical_state.rs` | Bernoulli convergence probe + seeded trial headline in `scripts/demo.sh` (asserted by `tests/demo_smoke.rs`) | Per-context winners correct; trial beats fixed control +22–45% |

Full suite after fixes: 530 tests, 0 failed.
