# Lycan Learning Semantics

Status: Draft v0.1 — describes implementation as of 2026-09-08

Normative description of the implemented learning stack: capsule warmup lifecycle, reward-shape
characterization, executor-internal weight updates, the server `/feedback` pipeline, selection
algorithms, per-node meta-bandits, hierarchical credit propagation, OOD scoring and refusal,
`memory.json` persistence, and the shared RNG.

The key words MUST, MUST NOT, SHOULD, MAY are used as in RFC 2119.

Citation conventions:

* Every constant and formula below carries a `path:line` citation into the working tree at
  2026-09-08. Facts so cited are `[verified]`.
* `[GAP]` marks a divergence between intent and implementation (unenforced, unreachable, or
  fail-open behavior). `[CURRENT-BEHAVIOR]` marks an implementation choice a future version may
  change. `[UNVERIFIED]` marks a claim carried from the 2026-09-08 fact-report assembly pass that
  could not be reproduced in code (see Appendix A; several such claims were proven false and are
  listed there explicitly rather than normatively).

## 1. Architecture overview

Learning state lives in three cooperating layers over the compiled `NeuralGraph`:

1. **Executor-internal layer** (§4): the `Strategy`, `AdaptiveChoice`, `Branch`, and `Feedback`
   opcodes mutate graph weights in-run with fixed constants (`lr 0.08` contracts / `lr 0.05`
   Feedback, clamp `[0.01, 0.99]`, renormalize) and journal every mutation (`src/graph.rs:95-116`).
   The `Adapt` opcode rebinds function bodies and performs no weight math [verified
   `src/graph_executor/exec.rs:806-818`].
2. **Server-driven sidecar layer** (§5–§9): per-capsule `memory.json` + `warmup.json` maintained by
   `/decide` and `/feedback`. A capsule-level warmup lifecycle (30-sample warmup, reward-shape
   characterization → `PickedAlgorithm`, capsule ADWIN δ=0.0005 regime-change revert) gates
   selection mode; per-node meta-bandits (5- or 7-candidate portfolio, sqrt-decaying exploration,
   floor 0.05, geometric forgetting 0.999) choose a *candidate algorithm* per decision; per
   `(node, context, candidate)` buckets and option-state posteriors (Beta-Bernoulli / UCB /
   LinUCB / LinTS) receive mean-seeking weight deltas.
3. **Hierarchical layer** (§8): capsules with a `hierarchical_spec.json` bypass the flat graph
   entirely; per-level bandit buckets in `hierarchical_state.json` receive leaf rewards with Full or
   geometrically discounted credit.

All sidecars persist via atomic write (tmp file + `write_all` + `fsync` + `rename`) [verified
`src/store.rs:735-742`]. Note: `write_atomic` uses `path.with_extension("tmp")`, so the temp name is
`memory.tmp`, not `memory.json.tmp` [verified `src/store.rs:737`]. `[CURRENT-BEHAVIOR]`

Randomness flows through one process-global SplitMix64 (§10). Determinism is only claimed for
sequential single-threaded request streams.

## 2. Warmup lifecycle

### 2.1 States

`CapsuleLifecycle` [verified `src/warmup.rs:6-10`]:

| State | Payload | Meaning |
|---|---|---|
| `Warmup` | `samples_collected`, `target` | Baseline collection; no learned selection |
| `Active` | `algorithm: PickedAlgorithm`, `characterization: RewardShape` | Learned selection + change detection |
| `Frozen` | `algorithm`, `reason` | Feedback ignored for lifecycle purposes |

`record_feedback(reward)` returns a `FeedbackOutcome` (`Collecting`, `WarmupComplete`,
`ActiveStable`, `ChangeDetected`, `FrozenIgnored`) [verified `src/warmup.rs:13-27`].

### 2.2 Transitions (exact)

`WarmupState::record_feedback` [verified `src/warmup.rs:79-126`]:

| # | From → To | Guard | Side effects |
|---|---|---|---|
| T1 | Warmup → Warmup | `collected < target` | push reward, `samples_collected += 1` |
| T2 | Warmup → Active | `collected >= target` (`:90`, `>=`) | `characterize(all collected rewards)` → `pick_algorithm`; detector seeded with ALL warmup rewards before going Active (`:92-95`) |
| T3 | Active → Active | `detector.add(r)` returns `None` | none |
| T4 | Active → Warmup | `detector.add(r)` returns `Some(change)` | `detector.reset()`; `collected_rewards.clear()`; state = `Warmup { samples_collected: 0, target: self.target_samples }` (`:106-115`). The triggering reward is NOT re-accumulated into the new warmup window; it is routed by the server into the legacy bucket (§5.4) |
| T5 | Frozen → Frozen | always | `FeedbackOutcome::FrozenIgnored` |

* Default `target` is **30** at every construction site: `WarmupState::new(30)` [verified
  `src/server/decide.rs:94-95, :276-277`; `src/server/inspect.rs:395-396, :481-482`] and
  `WarmupState::with_capsule_delta(30, learning_cfg.safety.capsule_adwin_delta)` [verified
  `src/server/feedback.rs:110-113, :307-310`].
* `target_samples` is NOT configurable through `learning.json` [GAP — field exists on the struct but
  no construction site reads config].
* `Frozen` is reachable only via `freeze()` [verified `src/warmup.rs:128-132`], which has **no
  non-test caller** [verified sole call site `src/warmup.rs:215` inside `#[cfg(test)]`]. `[GAP]`
  Frozen is dead state in production. (The unrelated config flag `safety.freezeLearning`
  [verified `src/learning/config.rs:111`] hard-errors `apply_feedback` with "learning is frozen"
  [verified `src/learning/feedback.rs:148-150`] — it does not set `CapsuleLifecycle::Frozen`.)
* Boundary case: the 29th feedback returns `Collecting { collected: 29, target: 30 }`; the 30th
  transitions (§11 C-L2).

### 2.3 Gating and persistence

* Warmup state advances **only after the feedback request is fully validated** (decisionId
  resolved, node/option in range). An unknown `decisionId` → HTTP 404 with no lifecycle effect
  [verified `src/server/feedback.rs:246-250, :302-314`].
* Feedback against a *refused* decision is acknowledged 200 with `"noted": "feedback recorded
  against refused decision; bandit state unchanged"` and returns before any learning or warmup
  advance [verified `src/server/feedback.rs:216-231`].
* `warmup.json` is loaded/saved through `write_atomic` on every accepted `/feedback`, pretty-printed
  [verified `src/store.rs:402-412`; callers `src/server/feedback.rs:115-118, :312-314`]. A
  malformed `warmup.json` deserializes to `None` → a fresh default `WarmupState` [verified
  `src/store.rs` load via `from_str(...).ok()`]. The struct has no serde renames/aliases, so legacy
  field names (`samples`/`targetSamples`/`status`) are NOT accepted [verified
  `src/warmup.rs:29-35`; fact-report claim of legacy-key parsing REFUTED, Appendix A-1].
* During `Warmup`, `/decide` executes with strategy weights flattened to `1/n_options` in memory
  and `SelectionMode::Weighted` [verified `src/server/decide.rs:281-282, :323-325`;
  `flatten_strategy_weights` `src/server/helpers.rs:65-79` — the WithinTolerance epsilon weight slot
  is excluded from flattening]. The flattening is persisted to `program.lyc` only when
  `learn=true` [verified `src/server/decide.rs:751-757`].
* `/decide` and `/feedback` responses carry the lifecycle block
  `{"state":"warmup","collected":c,"target":t}` / `{"state":"active","algorithm":...}` /
  `{"state":"frozen",...}` [verified `src/server/decide.rs:772-786`, `src/server/feedback.rs:601-615`].

### 2.4 Downstream effects of lifecycle transitions

* On `WarmupComplete`: audit event `warmup_complete` with `{algorithm, characterization}` debug
  renderings [verified `src/server/feedback.rs:320-330`]. `PickedAlgorithm` maps to the executor
  selection mode: `Thompson`/`Weighted` → `Weighted`, `UCB` → `Greedy`, `EpsilonGreedy` →
  `EpsilonGreedy`; absent (pre-Active) → `learning.json safety.selectionMode` [verified
  `src/server/decide.rs:281-303`]. `is_binary_reward` is defined as *the picked algorithm being
  `Thompson`* [verified `src/server/decide.rs:319-321`].
* On capsule-level `ChangeDetected`: `memory.reset_meta_bandit(node)` +
  `memory.reset_candidate_contexts(node, contextKey)` for every Strategy/AdaptiveChoice node of the
  triggering decision's context, and the triggering observation bypasses candidate routing
  (`chosen_candidate = None`) so the fresh meta-bandit records the credit against the legacy bucket
  only [verified `src/server/feedback.rs:423-431`]. Audit `change_detected` includes
  `dropped`, `oldMean`, `newMean` (`:331` region). `[CURRENT-BEHAVIOR]` warmup history is cleared
  (fact-report claim "history retained" REFUTED, Appendix A-1).

## 3. Change detection (ADWIN and option-level detectors)

### 3.1 ADWIN

`AdwinDetector` [verified `src/change_detection.rs:29-48, 90-129`]:

| Parameter | Capsule-level | Per-(node, context) |
|---|---|---|
| δ (delta) | **0.0005** [`change_detection.rs:46-48`; default fn `learning/config.rs:242`] | **0.002** [`change_detection.rs:40-42`; default fn `learning/config.rs:247`] |
| max window size | 1000 [`warmup.rs:53`; `learning/capsule.rs:261-278`] | 1000 |
| min subwindow | 5 (change test requires `n >= 2·5 = 10`) [`change_detection.rs:29-36, :90`] | 5 |

Cut rule [verified `src/change_detection.rs:110-115`]: over every split point, with
`m = 1 / (1/n_old + 1/n_new)`,

$$\varepsilon = \sqrt{\tfrac{1}{2m}\ln\tfrac{4n}{\delta}}$$

a change fires when `|mean_old − mean_new| > ε`; the `split` oldest samples are dropped and
`ChangeDetected { dropped: split, old_mean, new_mean }` is returned. **`dropped` is purely
informational** — no code anywhere thresholds on it (fact-report "change at dropped ≥ 12" and
"dropped ≥ 12 → reset_weights_to_uniform" REFUTED; no `reset_weights_to_uniform` exists) [verified
grep of `src/` + Appendix A]. Config overrides `safety.capsuleAdwinDelta` /
`safety.contextAdwinDelta` are clamped to `[1e-9, 0.5]`, with legacy key `safety.adwinDelta`
supplying both layers [verified `src/learning/config.rs:369-379`].

Both ADWIN layers are live: the capsule detector drives lifecycle T4 (§2.2); the per-context
detector runs independently on every flat `/feedback` — on change it resets ONLY that
(node, contextKey)'s candidate contexts and its own window; the capsule stays in its current
lifecycle [verified `src/server/feedback.rs:573-596`, audit `context_change_detected`].

### 3.2 Option-level change points (learning.json `changeDetection`, default OFF)

`apply_feedback` runs per-option detectors on the chosen option's stats [verified
`src/learning/feedback.rs:44-89, :186`]:

* **PageHinkley** (default method): requires `tries ≥ 5`; drift-accumulated `ph_cumsum` /
  `ph_min` (increments `r − mean − minDrift` and `mean − minDrift − ` floored at 0); fires when
  either exceeds `threshold` (5.0), then resets both.
* **ModelSurprise**: requires `tries ≥ 5` and window ≥ 4; z-score
  `|r − windowed_mean| / sqrt(var_recent / window_len)`; fires when the fraction of surprising
  points (`z > surprise_k_sigma` = 2.5) within the window reaches `surprise_fraction_threshold`
  = 0.30.
* On fire: `change_points += 1`, `change_boost_remaining = boost_duration` (default 50). The boost
  raises effective exploration ε (§6.2). Defaults: `threshold 5.0, minDrift 0.05,
  explorationBoost 0.25, boostDuration 50, surpriseKSigma 2.5, surpriseFractionThreshold 0.30`
  [verified `src/learning/config.rs:226-236` (defaults) and `:405-420` (parse)].

## 4. Executor-internal weight updates

Initial weights at compile time: `AdaptiveChoice` = `1/n` equal [verified
`src/graph_compiler.rs:285-287`]; `Strategy` = `1/n` plus a **trailing epsilon slot** `1e-6` for
WithinTolerance tolerance [verified `src/graph_compiler.rs:309-312`].

All weight mutations converge on: per-weight `clamp(0.01, 0.99)` then, when
`sum > 0.0`, renormalize (`w_i /= sum`) [verified `src/graph_executor/exec.rs:355-359, 461-465,
558-560, 880-884`; `src/graph_executor/state.rs:13, 17-27`]. Every mutation journals a
`JournalEntry { run_number, node_id, mutation: MutationKind::WeightUpdate, reason: u32::MAX }`
[verified `src/graph.rs:95-116`; sites `exec.rs:362-367, 467-472, 563-568, 888-893`]. There is no
`WeightUpdate` struct [verified grep; Appendix A].

### 4.1 `Strategy` opcode (contracts) [verified exec.rs:262-573]

Common constants: learning rate **0.08** [verified `exec.rs:334, 441, 547`]; majority/consensus
guard `count > n_options / 2` (integer division) [verified `exec.rs:319, 426`]; speed score

$$\text{score}_i = 1 - \frac{2\,(t_i - t_{\min}}{\text{range}} \qquad (\text{NEVER clamped to } [0,1]; \text{ only } w_i + 0.08\cdot\text{score}_i \text{ is clamped})$$

[verified `exec.rs:349, 455, 551`; fact-report claim that the score itself is clamped to [0,1]
REFUTED].

| Contract | Reward derivation | Update |
|---|---|---|
| **SameOutput** | Run ALL options, time each (`as_nanos`); plurality vote over stringified outputs; `correct_i ⇔ result_i == majority` | if `has_majority`: wrong options `w_i = (w_i − 0.2).clamp(0.01, 0.99)` [exec.rs:344-347]; correct options `w_i += 0.08 · score_i` with `t_min` = min time among CORRECT options, `max` = max time over ALL options, `range = max − t_min`; **no reward when `range ≤ 0`** [exec.rs:336-352]; renormalize [exec.rs:355-359]. **No majority → NO weight update** (stats still recorded) [exec.rs:319-330] |
| **WithinTolerance** | Run ALL options; numericize (Float/Int direct; `Str` parsed as comma list and summed; else 0.0); reference = median; `tol` = trailing epsilon weight (default 1e-6) [exec.rs:394]; `correct_i ⇔ |v_i − median| ≤ tol` | identical shape (punish −0.2 / reward `0.08·score` / renormalize), but normalization covers only `weights[..n]` with `n = len−1` — the epsilon slot is excluded from both learning and renorm [exec.rs:440-472]. No consensus → no update |
| **Fallback** (no contract) | ONE option per activation; selection = ε-exploration over least-tried, else argmax weight | see §4.2 |

Fallback exploration [verified `exec.rs:490-515`]:

$$\varepsilon = \max\!\left(\frac{0.3}{1 + \text{total\_tries}/5},\; 0.02\right)$$

the explore gate is **deterministic pseudo-random**, `explore ⇔ (activation_count·7 + 13) mod 100
< ε·100` [verified `exec.rs:495`] (it consumes no RNG); exploring picks the least-tried option set,
ties broken by `activation_count % |candidates|` [verified `exec.rs:499-508`]. Once **every** option
has `tries > 0` and the average-time range > 0, ALL options are updated every activation by
`w_i += 0.08 · score(avg_time_i)` (min/max over average times), clamped and renormalized [verified
`exec.rs:526-560`]. There is no "+0.1 winner / ×0.9 others" rule [verified grep of `exec.rs`;
Appendix A]. Chosen options always get `correct += 1` in stats (no verdict exists) [verified
`exec.rs:528-533`].

### 4.2 `AdaptiveChoice` + `Feedback` opcodes

`AdaptiveChoice` performs selection only [verified `exec.rs:172-241`]:

* WithinTolerance reserves the trailing weight as epsilon slot (`n_options = len − 1`)
  [verified `exec.rs:178-184`].
* Mode from `ExecutionContext`: `Greedy` (argmax), `Weighted` (roulette, 1 RNG draw
  `exec.rs:210`), `EpsilonGreedy` (2 draws `exec.rs:221-222`). Default ctx mode is `Greedy`,
  ε `0.10` [verified `src/context.rs:74-108`]; the server passes
  `learning.json safety.selectionEpsilon` (default 0.10, clamped `[0, 0.5]`) [verified
  `src/server/decide.rs:532-533`; `src/learning/config.rs:352-354`].
* Chosen index is stashed as `node.bias` for the `Feedback` opcode [verified `exec.rs:233-236`].

`Feedback` opcode is the ONLY weight-update path for AdaptiveChoice/Strategy targets from user
programs [verified `exec.rs:842-898`]:

* Reward coercion: Float/Int passthrough; `Bool(true) → +1.0`, `Bool(false) → −1.0`, other →
  `0.0` [verified `exec.rs:851-857`].
* `lr = 0.05` (fixed); chosen `w += r·lr`; each other option `w −= r·lr/(n−1)` (**additive** share,
  not multiplicative decay); clamp `[0.01, 0.99]`; renormalize [verified `exec.rs:868-884`].

There is no decaying learning rate in the executor; the only decaying quantity `0.3/(1+tries/5)`
capped at floor `0.02` is the Strategy **exploration ε** of §4.1, not an lr [verified
`exec.rs:492`; Appendix A].

### 4.3 `Branch` and `Adapt`

* `Branch` [verified `exec.rs:151-170`]: taken slot `+0.01`, untaken slot `−0.01`, **queued** into
  `weight_deltas` and flushed post-run: `(w + δ).clamp(0.01, 0.99)` then per-touched-node
  renormalization [verified `src/graph_executor/mod.rs:19, :85`; `state.rs:9-29`]. No reward
  term and no EMA.
* `Adapt` [verified `exec.rs:806-818`]: rebinds a `GraphFn` variable's body; performs **zero**
  weight arithmetic. (Fact-report "Adapt: w += 0.05·(r−w)" REFUTED — the 0.05 lr lives in
  `Feedback`, §4.2; Appendix A.)

## 5. Server `/feedback` pipeline (flat capsules)

Order of operations for `do_feedback` [verified `src/server/feedback.rs:151-639`]:

1. **Hierarchical dispatch** — capsules with a `hierarchical_spec.json` divert to §8
   [verified `:152-155`].
2. **Reward resolution** (`f64`), first match wins [verified `:170-203`]:
   * `reward` (explicit scalar); else
   * `components` + reward spec (inline `rewardSpec` or installed `reward_spec.json`): combined =
     Σ `weight · norm(raw)`, where `norm` is `minmax` over `[lo, hi]`, `budget` = `raw/budget`, or
     identity, each clamped `[0, 1]` [verified `src/learning/feedback.rs:363-397`]; per-component
     map retained for objective mirroring (step 11); else
   * `outcome` + `learning.json rewardPolicy` weights: Σ `value·weight` (bools as 1/0) [verified
     `src/learning/feedback.rs:351-361`]; fallback without policy:
     `outcome.success → +1.0 / −1.0 / else 0.0`; else 400.
3. **Target resolution**: `decisionId` → decision-log lookup (404 on unknown); `contextKey`
   inherited from the decision unless explicitly supplied; **refused decision → 200 no-op**
   (§2.3); optional `decisionIndex` (default 0) selects which AdaptiveChoice entry;
   `candidateId` and `featureVector` are read from the decision entry. Explicit mode:
   `strategyId|nodeId` + `option` [verified `:206-268`].
4. Node must exist and be `Strategy | AdaptiveChoice`; `n_options` excludes the WithinTolerance
   epsilon slot; `option` in range [verified `:280-299`].
5. **Warmup advance** after validation (§2.3), `warmup.json` saved [verified `:302-341`].
6. **Graph mirror** (flat `program.lyc` weights), skipped entirely when the decision carries a
   `candidateId` (`skip_weight_mutation = chosen_candidate.is_some()`) [verified `:276, :360`].
   `[CURRENT-BEHAVIOR]` The fact-report's skip predicate (`learned && memory_has_strategies &&
   reward ≠ 0`) does not exist (Appendix A). When active:
   * `r' = reward.clamp(−rewardClip, +rewardClip)`, default clip **2.0** (disabled when 0)
     [verified `:344-348`; `config.rs:262`].
   * `lr = learningRate.clamp(0.0001, 0.5)` (default 0.05) [verified `:349`; `config.rs:284, :349`].
   * $$\Delta = \operatorname{clamp}_{\pm 0.15}\big((r' - w)\cdot lr\big)$$ — the `maxWeightDeltaPerFeedback`
     clamp is applied **AFTER** the lr multiplication, in both the mirror and the sidecar [verified
     `:358-359`; `learning/feedback.rs:202`; fact-report "clamp BEFORE lr" REFUTED, Appendix A].
   * chosen `w += Δ`; each other `w −= Δ/(n−1)`; each `clamp(0.01, 0.99)` [verified `:360-368`].
   * min-exploration floor: `w_j = max(w_j, minExploration/n)`, then renormalize by the sum
     [verified `:371-379`; default `minExploration = 0.02`, `config.rs:260`].
7. Node `state_slot` counters (if present): activation count `+1`, success count `+1` iff
   `reward > 0` [verified `:381-388`].
8. Journal `MutationKind::FeedbackReceived` into the graph when `journalOnFeedback` (default
   true) [verified `:390-397`]; snapshot when `snapshotOnFeedback` (default true) [verified
   `:400-402`]; **graph is saved unconditionally** after this point (a byte-identical rewrite when
   weights did not change) [verified `:398-403`].
9. Audit `feedback` + `feedback.jsonl` append [verified `:405-419`].
10. Capsule-change reset (§2.4) when lifecycle T4 fired [verified `:423-431`].
11. **Sidecar routing** [verified `:432-560`], where the candidate set is the 5-candidate discrete
    portfolio under `contextSpec: Discrete` and the 7-candidate full portfolio under `Features`
    [verified `:433-442`]:
    * Candidate `LinUcb`/`LinTs`, `sharedState.enabled` with a stored `featureVector`: single shared
      θ strategy (lazily built from `sharedState.optionFeatures`, option name = index into the
      BTreeMap key order [verified `:450-458`]); `apply_feedback(name, x, raw reward)`; meta-bandit
      records the candidate.
    * Candidate `LinUcb`/`LinTs` without shared state: per-candidate bucket,
      `ensure_linucb_states(bucket, d, λ=1.0)` [verified `:500-503`]; predicted = `x·θ` before
      update; `LinUcbState::update(x, raw reward)` (Sherman–Morrison, §7.3);
      `rebuild_due(1000) → rebuild_inverse()` [verified `:504-511`]; conformity calibrator records
      `(predicted, raw reward)`.
    * Any other candidate (or none): `apply_feedback` / `apply_feedback_signal` on the candidate
      bucket (or the legacy `(node, contextKey)` bucket when no candidate).
    * In every candidate branch: `meta_bandit.record(candidate, RAW reward)` — unclipped, possibly
      negative [verified `:527, :556`; and `get_or_init_*` call sites]. `[CURRENT-BEHAVIOR]`
      meta-bandit credit is NOT clipped while weight updates ARE.
    * Per-component values mirrored into `stats.objective_rewards/objective_counts` of the touched
      bucket [verified `:538-556`].
12. **Per-context ADWIN** (§3.1) fed with the raw reward; change → candidate-context reset
    [verified `:561-596`].
13. `memory.json` saved unconditionally (`save_memory_in_job`) [verified `:597`]; response echoes
    `before`/`after` weights, `warmupTransitioned`, `changeDetected`,
    `contextChangeDetected`, lifecycle (§2.3).

### 5.1 Sidecar `apply_feedback` (per-bucket core) [verified `src/learning/feedback.rs:142-231`]

1. `safety.freezeLearning` → `Err("learning is frozen")` (no state change) [verified `:148-150`].
2. Clip to `±rewardClip` [verified `:158-162`].
3. **Geometric forgetting** on the chosen bucket's `OptionStats`: `reward_sum`, `reward_sq_sum`,
   `effective_tries`, and rounded `tries/successes/failures` all `× optionStateForgetting`
   (default **0.999**, parsed clamp `[0,1]`) [verified `:13-31`; `config.rs:268, :355-357`].
   There is NO blended `effective_reward = r·f + clip·(1−f)` anywhere (Appendix A). Optional
   half-life decay (`decay.halfLifeFeedbacks`, default 200, OFF) multiplies the same accumulators by
   `0.5^{1/h}` [verified `:33-41`].
4. Chosen option stats: `tries += 1`; `successes += 1` if `r' > 0` else `failures += 1` if
   `r' < 0`; sums/`last_reward`/`last_updated`/`effective_tries` updated; sliding `window`
   (default size 100, OFF); `change_boost_remaining −= 1`; option-level change point (§3.2);
   posterior reset from window stats when delayed feedback is OFF [verified `:165-190`].
5. **Conformity calibrator** fed `(predicted = visible weight, observed = r')` for every option
   state except `LinUcb` (whose path records separately, step 11 above) — fed regardless of
   `conformal.enabled` [verified `:103-113, :192`].
6. Weight delta identical in shape to §5 step 6: `Δ = clamp_{±0.15}((r' − w)·lr)`; chosen
   `+= Δ`, others `−= Δ/(n−1)`, clamp `[0.01, 0.99]` [verified `:199-211`].
7. **Min-exploration-preserving renormalization** (proportional reweight, not a bare floor):
   `w_i ← minExploration/n + (1 − minExploration)·(w_i/Σw)`; uniform `1/n` when `Σw = 0`
   [verified `:214-225`].
8. `update_option_states` (§5.2) [verified `:227`].

`apply_feedback_signal` (delayed feedback, `signalKind` + `delayedFeedback.enabled`): Gaussian
fusion of the observation with the option posterior
`post_var = (1/prior_var + 1/noise_var)^{-1}`, `post_mean = post_var·(prior_mean/prior_var +
(z − bias)/noise_var)`; the rest of the pipeline then runs on `posterior_mean` [verified
`src/learning/feedback.rs:91-101, 323-348`].

### 5.2 Option-state posteriors [verified `src/learning/feedback.rs:233-321`]

State kind is forced to match `learning.json algorithm` (`ThompsonSampling → betaBernoulli`,
`Ucb1 → ucb`, else `weighted`) with full rebuild on mismatch [verified `:302-321`].

| State | Forgetting (per feedback, all options) | Update for chosen option |
|---|---|---|
| `Weighted{w}` | none (scalar already current) | `w += clamp_{±0.15}((r'−w)·lr)`, clamp `[0.01,0.99]`; others `−= Δ/(n−1)` [`:262-267, :288-298`] |
| `BetaBernoulli{α,β}` | `α ← 1 + (α−1)·f`, `β ← 1 + (β−1)·f` [`:244-249`] | `r' ≥ 1−1e-9 → α += 1`; `r' ≤ 1e-9 → β += 1`; otherwise **fractional** `s = clamp(r',0,1)`: `α += s`, `β += 1−s` [`:269-280`]. Init `(1,1)` — there is NO extra `+0.5` pseudocount anywhere (Appendix A) |
| `Ucb{tries, total}` | `tries *= f`, `total *= f` [`:250-253`] | `tries += 1`, `total += r'` [`:281-284`] |
| `LinUcb` | none here | updated only on the feature path (§5 step 11) |

Visible weight (inspection / conformal prediction): `β` posterior mean `α/(α+β)`; `Ucb` mean
`clamp(total/tries, 0, 1)` with `0.5` at zero tries; `LinUcb` = `‖θ‖₂` activity proxy [verified
`src/learning/memory.rs:45-59`].

## 6. Selection algorithms

### 6.1 Two cooperating selection mechanisms `[CURRENT-BEHAVIOR]`

(a) **Config-driven scoring** (`learning.json algorithm`, §6.2) produces an
`algorithm_choice` per Strategy/AdaptiveChoice from the *legacy* `(node, contextKey)` bucket during
`apply_context_memory_to_graph`; the bucket's weights are copied onto the graph node first [verified
`src/server/helpers.rs:81-143`]. When `algorithm ∈ {ThompsonSampling, Ucb1}` the choice is then
applied as: **binary rewards** (`is_binary_reward`, §2.4) — hard commit `w_chosen = 1 − ε·(n−1)`,
others `ε = minExploration/n` [verified `helpers.rs:120-127`]; **continuous rewards** — soft nudge
`w_choice = max_w + 1e-3` then renormalize [verified `helpers.rs:128-135`].

(b) **Meta-bandit-driven mode mapping** (§7): in `Active`, the selected candidate maps to the
executor's selection mode: `Thompson|Weighted → Weighted`, `Ucb|Greedy|LinUcb|LinTs → Greedy`,
`EpsilonGreedy → EpsilonGreedy`; `LinUcb`/`LinTs` instead score options directly (§7.3) and force
`Greedy` for the first node [verified `src/server/decide.rs:440-518`]. `[GAP]` UCB-style exploration
at decide time exists ONLY via the config-driven path (a) and the LinUCB family; a meta-bandit
`Ucb` candidate merely runs Greedy over bucket weights.

### 6.2 `select_option` formulas (learning/selection.rs) [verified `src/learning/selection.rs`]

Effective ε [verified `:15-29`]:

$$\varepsilon_{\text{eff}} = \min\Big(\max(\varepsilon_{\text{cfg}},\ \text{minExploration}) + \underbrace{\max_i \min\!\big(\tfrac{\text{boost}_i}{50}, 1\big)\cdot 0.25}_{\text{change boost (largest active)}},\ 0.5\Big)$$

| Algorithm (JSON key) | Formula (exact) | Anchor |
|---|---|---|
| `simpleWeighted` (default) | roulette over weights (`r = rand·Σw`, cumulative); index 0 on `Σw ≤ 0` | `:59-69` |
| `epsilonGreedy` (`epsilon`, default 0.1) | with prob `ε_eff`: uniform draw (2 RNG); else argmax of `stats_score` (untried → weight fallback) | `:91-114` |
| `ucb1` | untried option first; else `argmax_i stats\_score_i + \sqrt{2\ln N' / n_i}` with `N' = max(\ln \Sigma n, 1)`, `n_i` = `effective_tries` when decay enabled (else `tries`, floor 1); plus GKT corruption bonus `budget / n_i` when `corruptionRobust.enabled` | `:116-147` |
| `thompsonSampling` | if any Beta state: per-option Gaussian-approx Beta sampler `clamp(N(α/(α+β), αβ/((α+β)²(α+β+1))), 0, 1)`, argmax; else Gaussian TS: untried first, `argmax_i sample_i`, `sample = mean + z·\sqrt{var/n'}` (`var` = windowed or cumulative variance floor `1e-6`, posterior-std floor `1e-4`) | `:158-210` |
| `softmax` (`temperature`, default 1.0) | `T = max(temp, 0.01)`; scores = `stats_score/T` (untried → 0); max-subtracted `exp` softmax | `:212-232` |

Fact-report formulas `w + 0.5·√(ln N/n_i)`, `w + √(max(w(1−w),1e-6)/10)·z`, `Beta(a+1,b+1)`,
`exp(5w)/Σ` (T=0.2) are all REFUTED (Appendix A). RNG draws: `rand_f64` at `:62, :98-99,
:151-152 (Box–Muller), :225`.

`stats_score` [verified `:71-89`]: untried → `−∞`; mean source precedence
`trimmed(trimmedFraction) > windowed > decayed > cumulative`; risk-sensitive blend
`(1−b)·mean + b·CVaR_α(window)` when `riskSensitive.enabled` (defaults `α=0.10, b=0.30`), where
CVaR = mean of the worst `⌈α·n⌉` windowed rewards [verified `src/learning/stats.rs:121-130`].
The fact-report's CVaR rules (`w + 0.15·(worst−w)`, `cvar_aversion`, `stats_score > w + 0.01`
override) do not exist (Appendix A).

## 7. Meta-bandit (per-node candidate selection)

### 7.1 Portfolio [verified `src/meta_bandit.rs:8-72`]

| Candidate | JSON id (`CandidateId::as_str`) | Meaning at decide time (§6.1b) |
|---|---|---|
| Thompson | `Thompson` | Weighted mode over bucket weights |
| Ucb | `Ucb` | Greedy mode |
| Weighted | `Weighted` | Weighted mode |
| EpsilonGreedy | `EpsilonGreedy` | EpsilonGreedy mode (ε = `safety.selectionEpsilon`) |
| Greedy | `Greedy` | Greedy mode |
| LinUcb | `LinUcb` | Direct LinUCB scoring (§7.3) |
| LinTs | `LinTs` | Direct LinTS scoring (§7.3) |

Portfolio = 5 (discrete) or 7 (feature) chosen by `contextSpec`, NOT by a `sharedState.enabled`
flag [verified `src/server/feedback.rs:433-442`; `src/server/decide.rs:417-424`; Appendix A]. JSON
ids are **capitalized** exactly as in the table (fact-report lowercase keys REFUTED).

### 7.2 Selection and credit [verified `src/meta_bandit.rs:118-231`]

Exploration probability (rate-adaptive, Bibaut-Chambaz-van der Laan 2020 shape):

$$p_{\text{explore}} = \begin{cases} 1.0 & \text{total\_rounds} = 0\\ \operatorname{clamp}_{[0.05,\,1]}\!\sqrt{\dfrac{N_{\text{cand}} \cdot 5.0}{\text{total\_rounds}}} & \text{otherwise} \end{cases}$$

[verified `:140-148`; parameters `exploration_decay = 5.0`, `min_exploration = 0.05`,
`:131-133`]. The fact-report's inverted form `√(5·total/N)` is REFUTED (Appendix A).

* Two RNG draws are **injected** (`select(rng_value, rng_pick)`, never called internally):
  `rng_value < p` → uniform candidate index (`rng_pick`); else exploit = argmax mean reward,
  ties → **fewer trials** wins; degenerate default `Thompson` [verified `:151-173`].
* Credit order (`record`, called from `/feedback` with the RAW reward): geometric forgetting
  `trials ×= f`, `cumulative_reward ×= f` on **ALL** candidates **before** incorporating the new
  observation; then chosen `trials += 1`, `cumulative += r`; `total_rounds += 1`. Default
  `f = 0.999` (≈ 700-event half-life) [verified `:176-194`].
* `reset()` zeroes all records and `total_rounds` (capsule regime change, §2.4) [verified
  `:197-204`].
* Persistence: per-node inside `memory.json` (§9); the reward credited is the same-sign raw
  `/feedback` reward keyed to the candidate recorded in the decision log (`candidateId` field,
  attached per AdaptiveChoice node at `src/server/decide.rs:599-610`) [verified
  `src/server/feedback.rs:521-557`].

### 7.3 LinUCB / LinTS [verified `src/linucb.rs`]

Feature vector `x` = `contextSpec.encode()` with `encoded_dimension = Σ feature dims + 1` bias;
Categorical one-hot drops the reference (first) level; trailing `1.0` bias always appended
[verified `src/feature_schema.rs:308-322, 402-405`]. Per-option state:
`LinUcbState{ A, A⁻¹, b, d, λ, since_last_rebuild }`, init `A = λI`, `A⁻¹ = (1/λ)I`, `b = 0`
(λ = 1.0 at the `ensure_linucb_states(bucket, d, 1.0)` call sites).

* UCB score: `score = x·θ + min(α·√(xᵀA⁻¹x), 10α)`, `θ = A⁻¹b`; **bonus capped at 10α**
  [verified `linucb.rs:85-100`]. Call-site α: **1.0** for LinUcb [verified
  `src/server/decide.rs:458`].
* LinTS score: `θ̃ = θ + v·L·z` with `A⁻¹ = LLᵀ` Cholesky, z iid standard normal, score
  `x·θ̃`; **fallbacks**: non-PSD Cholesky → posterior mean `x·θ`; non-finite score → mean
  [verified `linucb.rs:50-79`]. Call-site `v = 0.1` [verified `src/server/decide.rs:473`].
* Update (Sherman–Morrison): `A ← A + xxᵀ`;
  `A⁻¹ ← A⁻¹ − (A⁻¹x)(A⁻¹x)ᵀ / max(1 + xᵀA⁻¹x, 1e-12)`; `b ← b + r·x`;
  `since_last_rebuild += 1` [verified `linucb.rs:113-147`].
* Rebuild cadence: count-based — `rebuild_due(threshold)` when `since_last_rebuild ≥ threshold`;
  server uses **threshold 1000**; rebuild = Gauss–Jordan inverse of `A` (keep stale `A⁻¹` on
  inversion failure) [verified `linucb.rs:149-162`; `src/server/feedback.rs:509-511`]. The
  fact-report's `trace > 5000·d` trigger does not exist (Appendix A).
* **Shared-state strategy** (`sharedState.enabled`): one θ over `x = [x_context ; x_option]`
  (`d_total = d_context + d_option`), same UCB/LinTS score family with `sharedState.alpha`
  (default 1.0, `scoreKind` ∈ `ucb | lin_ts`, default `ucb`), same Sherman–Morrison +
  count-based rebuild [verified `linucb.rs:166-326`; `src/learning/config.rs:227-239`; decide
  short-circuit `src/server/decide.rs:346-412`]. `[CURRENT-BEHAVIOR]` All AdaptiveChoice nodes
  short-circuit to the shared θ; the recorded candidate stays `LinUcb` so feedback routing is
  unchanged [verified `decide.rs:407-411`]. Option name ↔ index = BTreeMap key order of
  `sharedState.optionFeatures`, both at decide and feedback [verified `decide.rs:374-375`;
  `src/server/feedback.rs:450-458`].

## 8. Hierarchical capsules

Constraints: `MAX_DEPTH = 4`, `MAX_LEAVES = 256`, `MIN_OPTIONS_PER_LEVEL = 2` [verified
`src/hierarchical.rs:9-17`]. Propagation modes [verified `src/hierarchical.rs:90-99, 359-382`]:

$$\text{credit at level } idx \text{ of a length-}N \text{ path} = \begin{cases} reward & \text{Full (default)}\\ reward \cdot factor^{\,N-1-idx} & \text{Discounted}\{factor\} \end{cases}$$

Both modes credit **every level along the chosen path** (only the magnitude differs) [verified
`propagate_reward`]. The fact-report's "Discounted = chosen path only (vs Full = all levels)" is
misdescribed — REFUTED (Appendix A).

* Selection [verified `src/hierarchical_state.rs:102-146`]: per-level weighted-random arm draw over
  bucket weights (bucket key `d{depth}|{path}`); the per-level `MetaBandit` (5-candidate default
  portfolio) is consulted to *stamp* `perLevelCandidateIds` in the decision log, and does NOT
  choose the arm [verified `:124-131`]. `[GAP]` The per-level meta-bandit observes selections it
  does not control. The fact-report's "meta arm if present else weighted sample" and
  `per_context_stats` decay 0.02/obs do not exist (Appendix A).
* Feedback [verified `src/hierarchical_state.rs:159-252`]: per level along the path with
  level credit `r_ℓ`:
  1. `w_chosen += 0.1·(r_ℓ − w_chosen)` (lr **0.1**, mean-seeking) [`:212-214`];
  2. global floor `w_i ≥ 1e-6`, then renormalize [`:215-222`] — NOT the flat clamp
     `[0.01, 0.99]` (Appendix A);
  3. `OptionStats` accumulate `r_ℓ` [`:227-232`];
  4. per-level `meta_bandit.record(candidate, r_ℓ)` — candidate from
     `perLevelCandidateIds` when supplied and length-matched, else the current leader (default
     `Thompson`); a length mismatch falls back and does NOT credit supplied ids [`:240-249`].
* State persists to `hierarchical_state.json` via `write_atomic` [verified `src/store.rs:435-454`].

## 9. OOD scoring and refusal

Scoring happens at `/decide` on the **first** AdaptiveChoice node, BEFORE `record()` for the
current request, then the detector is updated [verified `src/server/decide.rs:244-267`].

### 9.1 Discrete contexts [verified `src/ood.rs:9-63`]

`DiscreteOodDetector` (`staleness_threshold = 1000`, `min_warmup_rounds = 50`):

$$\text{score}(k) = \begin{cases} 0 & \text{total\_rounds} < 50\\ 1 & k \notin \text{seen}\\ \min\!\big(1, \frac{\text{total} - \text{last\_seen}(k)}{1000}\big) & \text{otherwise} \end{cases}$$

The fact-report's `max(1, min(0.99, (n−10)/20 + |0.5−mean|·2))` / `min_trials 10` formulas do not
exist (Appendix A).

### 9.2 Feature contexts [verified `src/ood.rs:66-197`]

`FeatureOodDetector`: Welford online mean + M2; `Σ⁻¹` rebuilt from `M2/(n−1) + 1e-4·I` when
`rebuild_due(100)` at the decide call site [verified `src/server/decide.rs:265-267`];

$$\text{score}(x) = \begin{cases} 0 & n < 50\\ \min\!\big(10, \dfrac{(x-\mu)^\top \Sigma^{-1} (x-\mu)}{d + 3\sqrt{2d}}\big) & \text{otherwise} \end{cases}$$

The normalizer `d + 3√(2d)` (χ² 99%-rule shape) is CONFIRMED; there is no mean-shift term and no
`max(2d,20)` or `max(50,20d)` warmup/rebuild rule (Appendix A).

### 9.3 Refusal gate [verified `src/server/decide.rs:676-722`]

Refusal is evaluated only when `refusal.enabled` (default **false**) AND lifecycle is `Active`
(warmup must collect baseline data). One threshold on the §9.1/§9.2 score plus a conformal
interval-width gate:

1. `oodScore ≥ refusal.oodThreshold` (default **0.8**; parsed clamp `[0, 10]`) → reason `"ood"`
   [verified `:710-711`; `config.rs:70-79, :510`].
2. Else interval width `= 2·Q_{1−coverage}(residuals)` over the chosen bucket's conformity
   calibrator (coverage default 0.95 → α = 0.05; split-conformal index
   `⌈(n+1)·coverage⌉ − 1`; `None` while `n < 30` residuals) [verified `src/conformal.rs:22-24,
   48-67`; `src/server/decide.rs:677-706`]: `None` → `"insufficient_calibration_data"`; width `>`
   `maxIntervalWidth` (default 0.5) → `"interval_too_wide"`.
3. Refused responses return 200 with `"decisions": []`, `"refused": true`, `confidence` block, and
   an `audit.jsonl` `decision_refused` event; the decision log records `refused`/`refusalReason`/
   `oodScore`/`intervalWidth` [verified `:724-833`]. `[CURRENT-BEHAVIOR]` The graph was already
   executed to compute these scores — refusal suppresses exposure of the decision, it does not
   pre-gate execution. Feedback on refused decisions is a no-op (§2.3).

The fact-report's dual thresholds (`≥ 0.5` feature / `≥ 0.95` staleness) and its
"`decision_score ≥ 1−confidence` refusal currently DEAD" claim are both REFUTED: `decision_score`
appears nowhere in `src/`; the refusal path above is live and interval-keyed (Appendix A). The
conformity calibrator is fed on every non-LinUCB `/feedback` regardless of `conformal.enabled`
(§5.1 step 5); `conformal.enabled` gates only the *surfaced* `conformalBandRadius`/`predictionSet`
fields [verified `src/learning/feedback.rs:115-137`; `src/server/decide.rs:570-575`].

## 10. memory.json schema (version 7)

`CapsuleMemory::to_json` hard-codes `"version": 7` [verified `src/learning/capsule.rs:72`];
`from_json` records the incoming `version` (fallback 1) and NEVER branches on it — schema evolution
is per-field fallback only, no migration pass [verified `:79-81`]. `[GAP]` A v8 reader would still
emit v7. Known legacy read: buckets lacking `conformityCalibrator` fall back to the v5 raw
`conformityScores` array restored as `(maxSize 500, minSamples 30)` [verified `:509-514`].

Field map [verified `src/learning/capsule.rs:34-133, 390-526`; `src/learning/memory.rs`]:

```
memory.json
├─ version: 7                                  (always written)
├─ strategies: { "<nodeId>": {
│    nodeId, nOptions,
│    contexts: { "<contextKey>": <Bucket> },          // legacy flat bucket
│    candidateContexts: { "<Candidate>|<contextKey>": <Bucket> },
│    metaBandit: { candidates: [{id, trials, cumulativeReward}],
│                  totalRounds, explorationDecay, minExploration, forgettingFactor },
│    contextDetectors: { "<contextKey>": { window, delta, maxSize, minSubwindow } },
│    discreteOod: { seen: {k: (count,lastSeen)}, totalRounds, stalenessThreshold, minWarmupRounds } | null,
│    featureOod:  { mean, m2, d, n, covInv, sinceLastRebuild, regularization, minWarmupRounds } | null } }
├─ timeSeriesWindows: { "<featureName>": <TimeSeriesWindow.serialize()> }
└─ sharedState: <SharedStateOptionStrategy.to_json()> | null
```

`<Bucket>` = `{ weights, stats: [OptionStats], updatedAt, conformityCalibrator:
{ residuals, maxSize, minSamples }, optionStates: [OptionState] }`. The canonical persistence path
re-serializes each stat with UNROUNDED `rewardSum`, `rewardSqSum`, `effectiveTries`, `window`,
`phCumsum`, `phMin`, `changeBoostRemaining`, `changePoints` on top of the display JSON
(`tries, successes, failures, rewardMean, rewardMeanWindowed, rewardVariance, lastReward,
lastUpdated, effectiveTries, windowFill, changePoints, changeBoostActive, posteriorMean,
posteriorVar, signalCounts, surpriseRecent, objectiveRewards, objectiveCounts`) [verified
`src/learning/stats.rs:132-160`; `capsule.rs:457-483`]. `optionStates` of wrong length → rebuilt as
`weighted(weights)` [verified `:517-522`]. OptionState JSON forms: `{kind:"weighted",weight}` /
`{kind:"betaBernoulli",alpha,beta}` / `{kind:"ucb",tries,totalReward}` /
`{kind:"linucb",d,lambda,aInv,a,b,sinceLastRebuild}` [verified `src/learning/memory.rs:61-111`].

The fact-report's §5 field map (`default_weights`, `last_update`, `StrategyContextBucket{mean_reward,
total_reward}`, standalone `ContextBucket{reward_sum,trials,last_reward}`, `conformity`/`last_reset`
fields) describes no structure present in the code (Appendix A).

Related stores: `learning.json` (LearningConfig, `write_atomic`), `warmup.json` (§2.3),
`hierarchical_state.json` / `hierarchical_spec.json` (§8), `program.lyc` (graph bytes; learn-gated on
decide §2.3, unconditional on feedback §5.8), JSONL logs `decision.jsonl`/`audit.jsonl`/
`feedback.jsonl`/`evolution.jsonl` — appended under a global mutex with 64 MiB single-generation
rotation to `<name>.jsonl.1` and **NO fsync per append**; `find_decision_in_job` returns an honest
404 once an entry rotates out [verified `src/store.rs:22, 522-561, 573, 719-726`].
`save_memory_if_changed_in_job` byte-compares and skips write+fsync when identical (used by
`/decide`) while `/feedback` always writes [verified `src/store.rs:637-656`;
`src/server/decide.rs:749`; `src/server/feedback.rs:597`].

## 11. RNG and determinism

`src/learning/rng.rs` [verified `:4-41`]:

* One process-global `SEEDED_RNG: Mutex<Option<u64>>`; `None` ⇒ entropy fallback.
* SplitMix64 when seeded — constants CONFIRMED [verified `:22-28`]:

  ```
  state += 0x9e3779b97f4a7c15
  z = state
  z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9
  z = (z ^ (z >> 27)) * 0x94d049bb133111eb
  z = z ^ (z >> 31)
  rand_f64 = (z >> 11) / 2^53          // uniform [0,1)
  ```

* Unseeded fallback: `DefaultHasher(SystemTime nanos ⊕ thread id ⊕ atomic counter) % 1_000_000 /
  1e6` — a 6-decimal-quantized, non-uniform stream [verified `:31-41`]. `[GAP]` The fallback is
  neither uniform nor reproducible; it is a legacy path.
* Seeding: **server only.** `LYCAN_RNG_SEED` parsed as u64 at startup; invalid value → warn +
  entropy fallback [verified `src/server/mod.rs:73-83`]. Runtime `POST /admin/rng/seed`
  (scope `AdminGlobal`): numeric body sets, `{}`/`{"seed":null}` clears (back to entropy) [verified
  `src/server/admin.rs:6-29`; route `src/server/routes.rs:150-154`]. The `lycan` CLI never calls
  `seed_rng` and ignores the env var [verified grep of `src/bin/lycan.rs`: zero references].
* Consumers of `rand_f64`: executor Weighted/EpsilonGreedy [verified `exec.rs:210, 221-222`];
  selection algorithms [verified `selection.rs:62, 98-99, 151-152, 225`]; server decide (hierarchical
  `select_path` closure, meta-bandit `select(r1,r2)`, Thompson/LinTS Box–Muller draws, clamped
  `[1e-12, 1−1e-12]`) [verified `decide.rs:41, 365-370, 426-429, 459-473`]. `meta_bandit.rs` and
  `hierarchical_state.rs` take randomness **injected**, never call `rand` directly [verified
  `meta_bandit.rs:151`; `hierarchical_state.rs:102-105`].
* Determinism caveat: the seed is shared process-globally and advances per draw, so byte-reproducible
  decision streams require strictly sequential request processing. Interleaved requests from
  different capsules share the stream [verified `rng.rs:4`]. Simulated/test PRNGs (`simulate.rs`
  xorshift, `ood.rs`/`meta_bandit.rs` test LCGs) are separate streams and NOT part of this ABI
  [verified `simulate.rs:1189-1199` region; `ood.rs:251-253` region].

## 12. Conformance requirements

A conforming implementation MUST satisfy each item below; boundary values are exact.

**Lifecycle**

* C-L1 — With default config, the 29th accepted feedback returns `Collecting { collected: 29,
  target: 30 }` and leaves `state="warmup"`; the 30th returns `WarmupComplete` + audit
  `warmup_complete` and `state="active"` (§2.2).
* C-L2 — `characterize` sees exactly `collected_rewards` at transition; a 29-sample prefix MUST
  NOT be characterized (Unknown floor, §13).
* C-L3 — After a sustained regime flip, `ChangeDetected` MUST fire, state resets to
  `Warmup{0, 30}` with an empty detector and empty history, and `/feedback` MUST report
  `"changeDetected": true`, meta-bandit trials 0 for the node, and the triggering reward applied to
  the legacy bucket only (§2.4, T4).
* C-L4 — Feedback with unknown `decisionId` → 404 and zero lifecycle/memory/graph side effects;
  feedback on a refused decision → 200 `noted`, zero side effects (§2.3).
* C-L5 — A malformed `warmup.json` MUST yield a fresh `Warmup{0,30}` state with δ from
  `learning.json` (or default 0.0005) without erroring (§2.3).

**Characterization (§13 tables)**

* C-R1 — `characterize` of 29 samples → `Unknown` with reason `"need 30 samples, got 29"`.
* C-R2 — Rewards in {0, 1} within `|r| < 1e-9` or `|r−1| < 1e-9` → `Binary`; a reward differing by
  1e-8 from {0,1} → NOT Binary.
* C-R3 — zero-ratio exactly 0.70 (21/30 zeros) → `BoundedContinuous` (rule is strict `>`);
  22/30 → `Sparse`; all-zero → `Unknown("all zero")`.
* C-R4 — `BoundedContinuous` with `|mean| ≤ 1e-9` → `CV = ∞` → `UCB{c: 2.0}`; `std/|mean|` exactly
  0.50 → `Weighted{learning_rate: 0.1}` (rule is strict `>`); `Binary` → `Thompson{1,1}`;
  `Sparse` → `UCB{c: 3.0}`; `Unknown` → `Weighted{0.05}`.

**Executor weights (§4)**

* C-W1 — SameOutput, n=4: 2/4 agreement → NO weight change (majority needs `> n/2`, i.e. ≥3);
  3/4 → the 1 dissenter loses exactly 0.2 pre-clamp; correct options change only if `range > 0`.
* C-W2 — Speed score is unclamped: an option slower than `t_min + range/2` receives a negative
  delta; `w` result clamps to `[0.01, 0.99]`; post-update weights sum to 1 (WithinTolerance: sum of
  `weights[..n_options]` = 1 with the epsilon slot unchanged).
* C-W3 — Fallback: no weight update until every option has `tries > 0`; explore iff
  `(activation_count·7+13) % 100 < ⌊ε·100⌋`; ε floor 0.02 reached at `total_tries ≥ 70`
  (`0.3/(1+14) = 0.02`).
* C-W4 — Feedback opcode: `Feedback(true)` ≡ `r=1.0`, `Feedback(false)` ≡ `r=−1.0`; chosen
  `+0.05`, others `−0.05/(n−1)`, then renormalize; `Adapt` performs zero weight mutations.
* C-W5 — Branch deltas apply once, post-run, clamped, then per-node renormalized.

**Server pipeline (§5)**

* C-P1 — Delta ordering MUST be `(r' − w)·lr` THEN `clamp(±0.15)`. Test: `learningRate = 0.5`,
  `r' − w = 1.0` → `|Δ| = 0.15` (a before-lr clamp would give `0.5`). At defaults
  (`lr 0.05`, clip 2.0) `|Δ| ≤ 0.0995` — the 0.15 clamp MUST NOT bind.
* C-P2 — Weight bounds `[0.01, 0.99]`; every weight `≥ minExploration/n` after both mirror and
  sidecar updates; sidecar proportions satisfy `w_i = 0.02/n + 0.98·rel_i` exactly.
* C-P3 — Beta updates: `r' = 0.3 → α += 0.3, β += 0.7`; `r' = 1.0 → α += 1`; `r' = −1 → β += 1`
  (clipped `r'` of −1 is `≤ 1e-9` → failure branch); forgetting moves α toward 1.
* C-P4 — Meta-bandit credit uses the RAW reward: `reward = 5.0` (clip 2.0) credits candidate
  cumulative `+5.0` while weight deltas see `r' = 2.0` (§5 step 11).
* C-P5 — Forgetting-before-observation: first ever `record(c, r)` ⇒ `trials_c = 1`, `cum_c = r`
  exactly (no pre-scaling); second `record(d, 0)` ⇒ `cum_c = 0.999·r`.

**Selection / meta (§6–§7)**

* C-S1 — Untried options: Ucb1 and Gaussian-Thompson MUST pick an untried option first;
  `stats_score(untried) = −∞`.
* C-S2 — Ucb1 bonus at N=1000, `n_i = 10` equals `√(2·ln 1000/10)` (not `0.5·√(ln N/n)`).
* C-S3 — Softmax at `temperature 0.001` behaves as at 0.01 (floor).
* C-S4 — ε_eff: config ε 0.01 with defaults → 0.02 (min-exploration floor); with an active
  `change_boost_remaining = 25` → `0.02 + 0.125` … capped at 0.5 (`remaining ≥ 100` yields
  exactly 0.5 ceiling).
* C-S5 — Meta `p_explore`: rounds 0 → 1.0; N=5, rounds 100 → 0.5; rounds ≥ 10000 → 0.05 (floor);
  never > 1.0.
* C-S6 — Meta exploit tie on equal means → strictly fewer-trials candidate wins.
* C-S7 — LinUCB bonus caps at `10α` (raw bonus `31.6` at α=1 ⇒ score contribution exactly 10);
  LinTS on non-PD `A⁻¹` returns `x·θ`; `A⁻¹` rebuilt within ≤ 1000 updates of exact Gauss–Jordan
  inverse drift reset.

**OOD / refusal / conformal (§9)**

* C-O1 — Discrete: `total_rounds = 49` → every score 0 (incl. unseen); at 50+, unseen → 1.0;
  staleness at 999 → `0.999`, ≥ 1000 → 1.0.
* C-O2 — Feature: `n = 49` → score 0; cap 10.0; `Σ⁻¹` rebuilt within 100 records.
* C-O3 — Refusal boundary: `oodScore == oodThreshold` → refused (`≥`);
  `width == maxIntervalWidth` → NOT refused (`>`); `residuals = 29` → `insufficient_calibration_data`;
  30 → width computable with split correction `⌈(n+1)(1−α)⌉−1`.
* C-O4 — Warmup-phase requests are never refused regardless of `refusal.enabled`.

**RNG / persistence (§10–§11)**

* C-N1 — With `LYCAN_RNG_SEED = s`, successive `rand_f64` outputs MUST equal the SplitMix64
  reference stream; conformance vectors for `s ∈ {0, 1, 42}` (first 8 draws) are required test
  fixtures.
* C-N2 — Server started without the var uses the entropy fallback (non-reproducible); CLI execution
  is byte-for-byte unaffected by the variable; `/admin/rng/seed` with `{"seed": n}` then a restart
  with the same `LYCAN_RNG_SEED` reproduces the identical decision stream ONLY under sequential
  traffic.
* C-N3 — `memory.json` MUST be emitted with `"version": 7` regardless of input version; a v5
  sidecar with `conformityScores` MUST load residuals; a v7 file MUST load field-by-field without
  lossy rounding on the canonical fields (§10).
* C-N4 — `decision.jsonl` rotates at the 64 MiB boundary keeping exactly one `.1` generation; after
  rotation an old `decisionId` → 404 (§10).

## 13. Appendix A — Fact-report deviation table

Ground-truth pass of 2026-09-08 re-verified every claim of `local://spec-semantics.md` against the
working tree. Statuses: **CONFIRMED** (report right), **CORRECTED** (concept exists;
names/constants/order fixed in the normative text), **REFUTED** (claim false or absent). The
report's own "Deviations/gaps found" list (D1–D9) is included below with the same statuses.

| # | Report claim | Status → actual |
|---|---|---|
| — | Capability registry `io.httpGet`/`io.readFile`/`data.query`/`strategic.*`/`compute.*`/`memory.*` (:28-512) | **REFUTED** — none exist. Registry has 35 entries (`runtime.*`,`file.*`,`http.*`,`json.*`,`sql.sqliteQuery`,`stats.*`,`series.*`,`ops.*`,`comb.*`,`nav.*`,`astro.*`) at `registry.rs:69-564`; see capability-abi.md |
| 1 | No episodic/semantic/temporal modules; memory.json fields only | **REFUTED (stronger)** — neither modules NOR such memory.json fields exist; concepts absent entirely |
| 2 | No `Characterization` enum; real `RewardShape`/`PickedAlgorithm` | **CORRECTED** — enums real (`reward_characterization.rs:4,:12`); line refs and variant payloads differ (§13 tables below) |
| 3 | `Frozen` unreachable | **CONFIRMED** (§2.1) |
| 4 | publish not effect-enforced | **CONFIRMED** [GAP] — capability-abi.md §3 |
| 5 | `ExecutionPolicy::default()` permissive, unused server-side | **CONFIRMED** — capability-abi.md §5 |
| 6 | conformal refusal dead pre-`/calibrate`; `decision_score` never set | **REFUTED** — `decision_score` exists nowhere; live refusal gates on OOD + interval width with ≥30 feedback-fed residuals (§9.3) |
| 7 | `LYCAN_RNG_SEED` server-only | **CONFIRMED** (§11) |
| 8 | `version:7` fixed, no migration | **CONFIRMED** (§10) |
| 9 | graph-mirror skipped when `learned && memory_has_strategies && r≠0` | **REFUTED** — skipped iff decision carries `candidateId` (§5.6) |
| A-1 | Warmup: legacy `samples/targetSamples/status` keys; history retained on change-revert; ADWIN_DELTA const at `warmup.rs:17` | **REFUTED** — no legacy keys; history cleared; δ via config (`config.rs:242`, §2/§3) |
| A-2 | Binary → LinUCB skip; thresholds `max−min ≤ 0.001 && (min<0.4∥max>0.6)`, zeros ≥ 0.7 with Binary-first order reversed, `n≥100 && p<0.05` LinUCB rule, `mean>0.6 && std/√n<0.3 → LinUCB else UCB` | **REFUTED** — actual rules in §13.1/§13.2; `PickedAlgorithm` has no LinUCB variant at all |
| A-3 | Warmup `/decide` forces epsilonGreedy ε=0.3 | **REFUTED** — Weighted mode + uniform flattening (§2.3) |
| A-4 | Graph NOT saved during warmup | **REFUTED** — save gated on `learn=true` only (§2.3) |
| A-5 | SameOutput ±0.08 / WithinTolerance punish-only no-renorm / fallback +0.1 & ×0.9 / score clamped [0,1] / Branch `w+=0.08(r−w)` / Adapt `0.05` EMA / AdaptiveChoice lr `max(0.3/(1+tries/5),0.02)` | **REFUTED** — §4: −0.2 punish + 0.08·speed-score reward + renorm; fallback mean-time score on all options; Branch ±0.01 queue; Adapt no math; 0.3/(1+tries/5) is Strategy exploration ε; Feedback lr fixed 0.05 additive |
| A-6 | `WeightUpdate{node_id, old, new, reward}` journal struct | **REFUTED** — `JournalEntry` (§4) |
| A-7 | Forgetting blend `r·f + clip·(1−f)` on reward | **REFUTED** — multiplicative stat decay (§5.1.3) |
| A-8 | max_delta clamp BEFORE lr; weight bounds 0.001/0.999; beta `+|r|`/`+1−|r|` gated `|r|≤1`; new_w formula | **REFUTED / CORRECTED** — clamp AFTER lr (§5 step 6, C-P1); bounds 0.01/0.99; beta rule §5.2; min-exploration semantics §5.1.7 (value 0.02 confirmed) |
| A-9 | dropped ≥ 12 → `reset_weights_to_uniform`; `dropped ≥ 12` change threshold | **REFUTED** — no threshold on `dropped`; no uniform-reset method (§3.1) |
| A-10 | Ucb1 `w + 0.5√(lnN/n)`; gaussian TS `w+√(w(1−w)/10)·z`; Beta `(a+1,b+1)+0.5`; Softmax T=0.2 | **REFUTED** — §6.2 formulas; default Softmax T=1.0, floor 0.01 |
| A-11 | Candidate priority rules (`stats_score > w+0.01`, LinUcb cold-start, `cvar_aversion`) | **REFUTED** — absent; candidate gating is `contextSpec`-based (§7.1); CVaR blend is config `riskSensitive` (§6.2) |
| A-12 | LinUCB rebuild at `trace > 5000d` | **REFUTED** — count-based `rebuild_due(1000)` (§7.3); 10α cap and α=1.0 CONFIRMED |
| A-13 | Meta `p = √(5·total/N)` floor 0.05; portfolio gated on `sharedState.enabled`; lowercase JSON ids | **CORRECTED** — `p = √(N·5.0/rounds)` clamped [0.05, 1] (§7.2); portfolio gated on `contextSpec`; ids capitalized (§7.1). Forgetting-before-observation and leader-with-tie-to-fewer-trials: **CONFIRMED** |
| A-14 | Hierarchical clamp [0.01,0.99]; `per_context_stats` 0.02/obs decay; meta arm drives selection; keys `<node>:<lvl>:<name>` | **REFUTED** — floor 1e-6 + renorm; no per_context_stats; weighted-random arm draw; `d{n}|{path}` bucket keys (§8). lr 0.1 and `factor^(n−1−idx)`: **CONFIRMED** |
| A-15 | OOD discrete `(n−10)/20 + |0.5−mean|·2`, min_trials 10; feature warmup `max(2d,20)`; rebuild `max(50,20d)`; mean-shift `mean_shift²·d/3`; refusal 0.5 / 0.95 dual | **REFUTED** — §9.1/§9.2/§9.3. Mahalanobis²/(d+3√2d) shape CONFIRMED (cap 10, warmup 50) |
| A-16 | memory.json field map (`default_weights`, `StrategyContextBucket{mean_reward,total_reward}`, `candidate_contexts{mean,var,n,…}`, `conformity`, `last_reset`) | **REFUTED** — actual schema §10; report's §5 map matches no struct |
| A-17 | persistence (memory content-compare, tmp+fsync+rename, JSONL no-fsync rotation, decision-log 404 after rotation) | **CONFIRMED** (§10) with corrected line anchors `store.rs:637-656, 735-742, 522-544` |
| A-18 | SplitMix64 constants `0x9e3779b97f4a7c15 / 0xbf58476d1ce4e5b9 / 0x94d049bb133111eb`; server-only seed; admin endpoint; CLI ignores | **CONFIRMED** (§11) at corrected anchors `rng.rs:22-28`, `server/mod.rs:73-83`, `admin.rs:6-29` |

### 13.1 Characterization rules (actual) [verified `src/reward_characterization.rs:24-58`]

`MIN_SAMPLES = 30`; `BINARY_TOL = 1e-9`; `SPARSITY_THRESHOLD = 0.7` (strict `>`); population
variance throughout.

| Order | Test | Result |
|---|---|---|
| 0 | `len < 30` | `Unknown{reason}` |
| 1 | all `r` within 1e-9 of {0,1} | `Binary{positive_rate}` |
| 2 | zero-ratio `> 0.7` | `Sparse{density, nonzero_mean, nonzero_std}` (all-zero → `Unknown{"all zero"}`) |
| 3 | otherwise | `BoundedContinuous{min, max, mean, std}` |

### 13.2 `pick_algorithm` (actual) [verified `:60-73`]

| Shape | Choice |
|---|---|
| `Binary` | `Thompson{alpha:1.0, beta:1.0}` |
| `Sparse` | `UCB{c:3.0}` |
| `BoundedContinuous`, CV = `std/|mean|` (∞ if `|mean| ≤ 1e-9`) | CV > 0.5 → `UCB{c:2.0}`; else `Weighted{learning_rate:0.1}` |
| `Unknown` | `Weighted{learning_rate:0.05}` |

### 13.3 Learning-config default table [verified `src/learning/config.rs:249-296`]

| JSON key (path) | Default | Clamp at parse |
|---|---|---|
| `algorithm` | `simpleWeighted` | keys: `epsilonGreedy`(ε 0.1), `ucb1`, `thompsonSampling`/`thompson`, `softmax`(T 1.0) |
| `learningRate` | 0.05 | [0.0001, 0.5] |
| `safety.maxWeightDeltaPerFeedback` | 0.15 | — |
| `safety.minExploration` | 0.02 | — |
| `safety.rewardClip` | 2.0 | mode `highAssurance` overrides to 1.0; 0 disables |
| `safety.trimmedFraction` | 0.0 | [0, 0.49] |
| `safety.selectionMode` / `selectionEpsilon` | greedy / 0.10 | ε [0, 0.5] |
| `safety.optionStateForgetting` | 0.999 | [0, 1] |
| `safety.capsuleAdwinDelta` / `contextAdwinDelta` | 0.0005 / 0.002 | [1e-9, 0.5]; legacy `adwinDelta` feeds both |
| `safety.snapshotOnFeedback` / `journalOnFeedback` | true / true | mode `highThroughput` → false/false |
| `window.size` (off) | 100 | ≥ 1 |
| `changeDetection.*` (off) | threshold 5.0, minDrift 0.05, boost 0.25, duration 50, pageHinkley, k 2.5, frac 0.30 | — |
| `riskSensitive` (off) | α 0.10, blend 0.30 | α [0.01,0.99]; blend [0,1] |
| `corruptionRobust` (off) | budget 0.0 | ≥ 0 |
| `conformal` (off) | coverage 0.90, calibrationSize 100 | coverage [0.50,0.999]; size ≥ 10 |
| `refusal` (off) | coverage 0.95, maxIntervalWidth 0.5, oodThreshold 0.8 | coverage [0.5,0.999]; oodThreshold [0,10] |
| `sharedState` (off) | λ 1.0, α 1.0, scoreKind `ucb` | λ ≥ 1e-9; α ≥ 0; optionFeatures with wrong `dOption` silently dropped |
