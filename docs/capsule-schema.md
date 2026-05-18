# Capsule schema reference

This document covers two distinct schemas that together configure a Syntra
capsule. The first, the **capsule YAML**, is consumed by `syntra author` at
install time; it declares the capsule's identity, action set, and reward shape.
The second, the **learning config**, is a JSON object you PUT to
`/tenants/{t}/jobs/{j}/capsules/{c}/learning` after installation; it governs
how the runtime algorithm learns and makes decisions. The two schemas are
separate because they have different lifecycles: the YAML is compiled into a
`.lyc` program that cannot change without reinstalling the capsule, whereas the
learning config is a mutable runtime parameter that can be adjusted at any time
without disrupting existing learned weights.

Related reading: [`concepts.md`](concepts.md) explains what a capsule is;
[`tutorial.md`](tutorial.md) walks through authoring and installing one end to
end; [`runbook.md`](runbook.md) covers runtime failures.

---

## 1. Capsule YAML — the install-time schema

### Overview

The capsule YAML is the single artifact you write when authoring a capsule. It
declares everything that is structurally fixed: what the capsule is called, what
options it can return, what its reward signal looks like, and an optional
algorithm hint. `syntra author` validates and compiles this file into a `.lyc`
binary that is subsequently installed via POST to the capsule path.

Discrete context dimensions (the `contexts` field) may be declared in the YAML,
but continuous feature contexts are attached separately via the learning config
`contextSpec` field after installation. This split is intentional: feature
schemas can be evolved without reinstalling the capsule binary, whereas the
option set and reward type cannot.

### Minimal worked example

The following capsule is taken from `docker/demo/capsule/retry_tuning.yaml`. It
is intentionally minimal to illustrate which fields are required.

```yaml
name: retry-tuning
options:
  - none
  - single
  - triple
  - exponential_fast
  - exponential_slow
contexts:
  - endpoint        # discrete dimension; the bandit tracks separate estimates per endpoint
reward:
  type: continuous
  range: [-1.0, 1.0]
```

### Field reference

#### `name`

```
type:     string
required: yes
```

A stable, human-readable identifier for this capsule. Used in log lines,
snapshot filenames, and the API path. Must not be empty or consist only of
whitespace. The value is not validated for uniqueness at authoring time; the
tenant/job/capsule path combination is what makes a capsule unique in the
running system.

#### `version`

```
type:    string
default: ""
```

An optional version tag. Not interpreted by the runtime; purely informational.
Useful for annotating which release of your application deployed a given capsule
binary. The empty default is acceptable; you do not need to set this field.

#### `options`

```
type:     list of string
required: yes
minimum:  2 entries
```

The action set: the exhaustive list of choices the capsule's `/decide` endpoint
can return. Every element must be a non-empty string. Duplicate entries are
technically not rejected at parse time but will produce undefined weighting
behaviour — treat option names as unique identifiers.

The minimum length is two. A single-option capsule is rejected at validation
with the message "options must contain at least two entries". There is no
documented upper limit, but the algorithms' per-option memory grows linearly
with option count; very large option sets (hundreds or more) are better
addressed by a hierarchical decomposition.

#### `contexts`

```
type:    list of string
default: []
```

Discrete context dimensions for the `contextSpec.discrete` mode. Each string
names a context key whose value, at decision time, is an opaque string. The
bandit maintains a separate reward estimate per (context-tuple, option) pair.
Providing zero contexts means all decisions are treated as drawn from the same
context.

This field is unrelated to the feature-vector context attached via the learning
config `contextSpec` field. The distinction is documented in section 2 under
[Context spec](#context-spec-contextspec). You should not declare both a
non-empty `contexts` list in the YAML and set `contextSpec.type: features` in
the learning config at the same time; the behaviour is that the feature-context
path takes precedence when it is active.

#### `reward` (block, required)

The `reward` block describes the shape of the scalar signal that the capsule
receives when you POST to `/feedback`. It has three sub-fields.

##### `reward.type`

```
type:   enum
values: bernoulli | continuous | sparse_continuous
```

`bernoulli` — the reward is binary. The caller reports 1.0 (success) or 0.0
(failure). The `auto` algorithm resolves to Thompson sampling with a Beta-
Bernoulli posterior in this mode. Appropriate for click/no-click, conversion,
any outcome that is a plain yes/no.

`continuous` — the reward is a real number in a declared range. Requires
`reward.range`. The `auto` algorithm resolves to the weighted-average
(`SimpleWeighted`) estimator in this mode. Use this for latency, quality
scores, or composite metrics that are always observed.

`sparse_continuous` — the reward is a real number in a range, but many
decisions never receive a feedback signal (the feedback is sparse or
intermittent). The `auto` algorithm resolves to UCB1, which is more robust to
low feedback density than weighted averaging. The `range` field is optional but
recommended; if omitted, no normalization is applied.

##### `reward.range`

```
type:     [float, float]
required: when type = continuous
```

A two-element array `[min, max]` specifying the declared bounds of the reward
signal. Validation rejects a `continuous` capsule that omits this field. For
`sparse_continuous` the field is optional but should be provided when the reward
has known bounds, as it enables reward clipping and normalization in the runtime.
Ignored for `bernoulli`.

Examples: `[-1.0, 1.0]`, `[0.0, 1.0]`, `[0, 100]`.

##### `reward.components`

```
type:    list of RewardComponent
default: []
```

An optional list of named sub-signals that compose the overall reward. When
non-empty, the caller is expected to report each component's raw value at
feedback time and the runtime computes the weighted scalar. When empty (the
default), the caller reports a single scalar reward directly.

Each component has five fields:

```
name       string     required
weight     float      required (must be finite)
normalize  enum       required: minmax | budget
range      [f, f]     required when normalize = minmax
budget     float      required when normalize = budget
```

`name` — a unique identifier for this sub-signal within the capsule. Duplicate
names are rejected by the validator.

`weight` — the signed multiplier applied to this component's normalized value
when computing the aggregate reward. Negative weights express costs (latency,
spend). The runtime checks that this value is finite; `NaN` and `Inf` are
rejected.

`normalize: minmax` — scales the raw component value linearly to [0, 1] using
the declared `range`. Requires `range`. Use for quality metrics, scores, or
any signal with known finite bounds.

`normalize: budget` — scales the raw value by dividing by `budget`, producing a
dimensionless "fraction of budget consumed" measure. Requires `budget` to be a
positive finite float. Use for cost or latency signals where the natural unit is
"how much of a target did this consume?"

A component with `normalize: minmax` that omits `range` is rejected. A
component with `normalize: budget` that omits `budget` is rejected. See section
5 for the full list of validation errors.

#### `algorithm` (block, optional)

```
default: { type: auto }
```

An optional hint directing which algorithm family to use.

##### `algorithm.type`

```
type:    enum
default: auto
values:  auto | thompson | ucb | epsilon_greedy | weighted
```

`auto` (default) — the platform selects the most appropriate algorithm based on
the reward type: Thompson sampling for `bernoulli`, weighted averaging for
`continuous`, UCB1 for `sparse_continuous`. This is the recommended setting for
most capsules.

`thompson` — Gaussian Thompson sampling. Samples the posterior mean reward
estimate, then picks the option with the highest sample. Works well for moderate
option counts and moderate feedback rates.

`ucb` — UCB1 upper-confidence-bound selection. Adds an exploration bonus
inversely proportional to visit count. More aggressive exploration than Thompson
for sparse feedback, more conservative than epsilon-greedy for dense feedback.

`epsilon_greedy` — with probability epsilon, pick uniformly at random;
otherwise pick the current best estimate. The `epsilon` parameter is set in the
learning config, not in the YAML.

`weighted` — pure weighted-average exploitation with exploration controlled
entirely by `safety.minExploration`. The simplest estimator; suitable when
feedback is dense and the reward landscape is stable.

Specifying any value other than `auto` overrides the `resolved_algorithm()`
selection for the life of the capsule. The override is baked into the compiled
`.lyc` program.

#### `learning` (block, optional)

```
default: { min_exploration: 0.02 }
```

A lightweight learning hint embedded in the capsule YAML. Most learning
parameters are set via the runtime learning config (section 2). Only
`min_exploration` is settable here.

##### `learning.min_exploration`

```
type:    float
default: 0.02
```

The floor probability that any option will be chosen during exploration, applied
at install time as the initial safety floor. This value seeds the
`safety.minExploration` field in the default learning config. If you subsequently
PUT a learning config with a different `safety.minExploration`, that value takes
over. Setting this too low risks reward starvation on rarely-chosen options;
setting it too high slows convergence.

---

## 2. Learning config — the runtime-update schema

The learning config is a JSON object PUT to:

```
PUT /tenants/{tenant}/jobs/{job}/capsules/{capsule}/learning
Content-Type: application/json
```

Every field is optional. Unrecognised fields are silently ignored. The server
merges the supplied JSON over the current live config; fields you omit retain
their previous values (or defaults on first install). All keys are camelCase.

The minimal learning config is `{}` (empty object), which applies all defaults.

### Algorithm selection

#### `algorithm`

```
type:    string
default: "simpleWeighted"
values:  "simpleWeighted" | "epsilonGreedy" | "ucb1" | "thompsonSampling" | "softmax"
```

Overrides the algorithm chosen by the capsule YAML's `algorithm.type`. The
string names map to the same algorithm families: `"simpleWeighted"` is the
weighted-average estimator, `"ucb1"` is UCB1, `"thompsonSampling"` uses
Gaussian posterior sampling, `"epsilonGreedy"` uses epsilon-greedy (see
`epsilon` below), `"softmax"` uses Boltzmann selection (see `temperature`).

Setting this field after installation changes the live decision policy
immediately, without restarting or losing accumulated statistics.

#### `epsilon`

```
type:    float
default: 0.1
applies: when algorithm = "epsilonGreedy"
```

The probability of a uniform random selection in epsilon-greedy mode. Values
must be in [0, 1]. Values outside this range are not clamped by the parser; pass
a sensible value. Typical range: 0.05 to 0.20.

#### `learningRate`

```
type:    float
default: 0.05
range:   [0.0001, 0.5] (clamped)
```

Step size for reward-weight updates in the weighted and linear estimators. The
parser clamps values to [0.0001, 0.5]; values outside that range are silently
clamped rather than rejected. Lower values produce more stable but slower
adaptation; higher values track non-stationary rewards faster but amplify noise.
See [`concepts.md`](concepts.md) for guidance on tuning this against your
feedback rate.

### Decay

#### `decay.enabled`

```
type:    boolean
default: false
```

When true, reward statistics are exponentially discounted over time so that
older observations count less than recent ones. Use when the reward landscape
drifts over time and you want the algorithm to forget distant history.

#### `decay.halfLifeFeedbacks`

```
type:    float
default: 200.0
```

The count-based half-life: the number of feedback events after which a past
observation is weighted at half its original value. Effective when traffic is
roughly steady and you want decay tied to observation count rather than wall
time.

#### `decay.halfLifeSeconds`

```
type:    float
default: 604800.0  (one week)
```

The wall-clock half-life in seconds. Useful for long-lived capsules with
irregular traffic (e.g., overnight batches) where count-based decay would
produce inconsistent forgetting across low- and high-traffic periods. Both
half-lives are active simultaneously when decay is enabled; whichever decays
faster dominates.

### Window

#### `window.enabled`

```
type:    boolean
default: false
```

When true, each option's reward estimate is computed only over the most recent
`window.size` feedback events for that option, discarding older observations
entirely. More aggressive than decay for abrupt environment changes; less
smooth. Incompatible with the retrospective analysis that `conformal` uses;
prefer decay if you have change detection enabled.

#### `window.size`

```
type:    integer
default: 100
minimum: 1 (floored to 1 if zero is supplied)
```

Number of recent feedbacks to retain per option. The parser floors this to 1 if
zero is provided.

### Safety

#### `safety.maxWeightDeltaPerFeedback`

```
type:    float
default: 0.15
```

The maximum absolute change to a reward weight in a single feedback update.
Acts as a gradient clip to prevent reward-spike poisoning from a single
anomalous observation. Lower values produce more stable but slower adaptation;
raise this if legitimate large rewards are being clipped. See also
`corruptionRobust` for a complementary budget-based approach.

#### `safety.minExploration`

```
type:    float
default: 0.02
```

The probability floor for any option in the selection distribution. No option
can receive less than this fraction of traffic regardless of its estimated
reward. Prevents reward starvation and maintains a minimum signal for detecting
future distributional shifts. The YAML's `learning.min_exploration` seeds this
on first install; a learning config PUT overrides it.

#### `safety.freezeLearning`

```
type:    boolean
default: false
```

When true, the algorithm stops updating reward estimates on feedback. The
capsule continues making decisions using its current learned weights, but new
feedback events do not change those weights. Use to lock in a configuration
after a controlled rollout, or to investigate a suspected data quality issue
without degrading the current policy. Can be toggled without reinstalling.

#### `safety.rewardClip`

```
type:    float
default: 2.0
```

Rewards are clipped to `[-rewardClip, +rewardClip]` (in standard-deviation
units relative to the observed reward distribution) before being incorporated
into estimates. Reduces the influence of outlier reward values. The `highAssurance`
mode preset (see the `mode` shortcut below) tightens this to 1.0.

#### `safety.trimmedFraction`

```
type:    float
default: 0.0
range:   [0.0, 0.49] (clamped)
```

The fraction of extreme reward observations to discard from each end of the
distribution before computing the mean estimate. A trimmed fraction of 0.1
drops the top 10% and bottom 10% of observations. Zero (the default) applies no
trimming. Values above 0.49 are clamped by the parser.

#### `safety.snapshotOnFeedback`

```
type:    boolean
default: true
```

When true, the algorithm state is persisted to disk after every feedback event.
This guarantees that no more than one feedback event's worth of learning is lost
on a crash. The `highThroughput` mode preset disables both `snapshotOnFeedback`
and `journalOnFeedback` for maximum write throughput at the cost of that
guarantee.

#### `safety.journalOnFeedback`

```
type:    boolean
default: true
```

When true, each feedback event is written to a structured journal before the
in-memory state is updated, enabling replay-based recovery. See
[`runbook.md`](runbook.md) for recovery procedures.

#### `safety.selectionMode`

```
type:    string
default: "greedy"
values:  "greedy" | "weighted" | "epsilonGreedy"
```

Controls how the selection distribution is formed from the estimated reward
weights, independently of the learning algorithm. `"greedy"` places all weight
on the current best option subject to `minExploration`. `"weighted"` distributes
selection probability proportionally to estimated reward weights (softened
exploitation). `"epsilonGreedy"` uses `selectionEpsilon` probability of uniform
random selection and picks the greedy best otherwise.

#### `safety.selectionEpsilon`

```
type:    float
default: 0.10
range:   [0.0, 0.5] (clamped)
```

The exploration fraction when `selectionMode` is `"epsilonGreedy"`. Clamped to
[0.0, 0.5] by the parser.

#### `safety.optionStateForgetting`

```
type:    float
default: 0.999
range:   [0.0, 1.0] (clamped)
```

An exponential smoothing factor applied to per-option statistics on each
feedback cycle. Values close to 1.0 retain nearly all history (slow forgetting);
lower values produce faster decay. At 0.999, half-life is approximately 693
feedback events. This provides a gentler form of decay than the `decay` block
and is always active.

### Change detection

#### `changeDetection.enabled`

```
type:    boolean
default: false
```

When true, the runtime monitors each option's reward stream for distributional
shifts and temporarily boosts exploration upon detecting one. Use for
deployments where the environment is known to shift discretely (e.g., upstream
model upgrades, seasonal patterns, incident events).

#### `changeDetection.threshold`

```
type:    float
default: 5.0
```

Detection sensitivity for the Page-Hinkley method. The test statistic must
exceed this value before a change point is declared. Higher values reduce false
positives at the cost of slower detection. For the model-surprise method this
parameter is not used directly.

#### `changeDetection.minDrift`

```
type:    float
default: 0.05
```

The minimum mean shift (in reward units) required to trigger a detection signal.
Acts as a noise floor to prevent spurious detections from small variance
fluctuations.

#### `changeDetection.explorationBoost`

```
type:    float
default: 0.25
```

The additional exploration probability added to the `minExploration` floor
immediately after a change point is detected. Applied for `boostDuration`
feedback events.

#### `changeDetection.boostDuration`

```
type:    integer
default: 50
```

The number of feedback events for which the exploration boost remains active
after a detected change point.

#### `changeDetection.method`

```
type:    string
default: "pageHinkley"
values:  "pageHinkley" | "modelSurprise"
```

`"pageHinkley"` — a sequential change-point test on the cumulative reward sum.
Low computational overhead; effective for gradual drifts.

`"modelSurprise"` — flags a change when recent observations deviate from the
model's prediction by more than `surpriseKSigma` standard deviations on more
than `surpriseFractionThreshold` of recent events. More sensitive to abrupt
structural shifts; more expensive to compute.

#### `changeDetection.surpriseKSigma`

```
type:    float
default: 2.5
applies: when method = "modelSurprise"
```

The number of standard deviations from the model's expected reward above which
a single observation is considered "surprising."

#### `changeDetection.surpriseFractionThreshold`

```
type:    float
default: 0.30
applies: when method = "modelSurprise"
```

The fraction of recent observations that must be surprising (as defined by
`surpriseKSigma`) before a change point is declared.

### Delayed feedback

#### `delayedFeedback.enabled`

```
type:    boolean
default: false
```

When true, the capsule supports multi-signal feedback where different sub-
signals may arrive at different times. Each signal is modelled as a noisy
observation of the latent true reward. The runtime fuses signals using a
Kalman-style posterior update.

#### `delayedFeedback.signals`

```
type:    array of SignalSpec
default: []
```

Each element declares one delayed signal channel. Fields per element:

```
name           string    required
noiseVariance  float     required; clamped to minimum 1e-6
bias           float     default 0.0
```

`name` identifies which signal channel this element describes. Must match the
key used in the feedback payload. `noiseVariance` specifies the assumed
observation noise variance for this signal; lower values give the signal more
weight in the posterior fusion. `bias` is an additive offset subtracted from
the raw signal value before fusion; useful when a signal is known to be
systematically shifted from the true reward.

### Risk-sensitive

#### `riskSensitive.enabled`

```
type:    boolean
default: false
```

When true, the selection criterion blends the estimated mean reward with a
risk-adjusted term (conditional value at risk at confidence level `alpha`). Use
when the cost of a bad outcome is asymmetrically high and you want the algorithm
to be conservative even if a risky option has a higher mean.

#### `riskSensitive.alpha`

```
type:    float
default: 0.10
range:   [0.01, 0.99] (clamped)
```

The tail probability used for the CVaR computation. Lower values (e.g., 0.05)
focus on the worst 5% of outcomes; higher values (e.g., 0.30) produce a
moderate risk adjustment.

#### `riskSensitive.blend`

```
type:    float
default: 0.30
range:   [0.0, 1.0] (clamped)
```

The linear interpolation weight between the mean estimate and the CVaR estimate.
A blend of 0.0 produces pure mean-based selection; 1.0 produces pure CVaR-based
selection. 0.30 gives a moderate risk penalty while retaining most of the signal
from the mean.

### Corruption-robust

#### `corruptionRobust.enabled`

```
type:    boolean
default: false
```

When true, the learner applies a budget-bounded outlier rejection scheme to
feedback values before incorporating them into reward estimates. Complementary
to `safety.maxWeightDeltaPerFeedback`; where the latter clips the weight update,
this approach identifies and rejects individually corrupt observations.

#### `corruptionRobust.budget`

```
type:    float
default: 0.0
minimum: 0.0 (floored)
```

The fraction of feedback events per option that are assumed to be potentially
corrupted. A budget of 0.05 means up to 5% of observations may be adversarially
corrupted and will be filtered. The parser floors negative values to 0.0.

### Conformal

#### `conformal.enabled`

```
type:    boolean
default: false
```

When true, the runtime maintains calibrated prediction intervals for each
option's reward using conformal prediction. These intervals are used to drive the
refusal mechanism (section on `refusal`) and are surfaced in the `/inspect`
response. See [`concepts.md`](concepts.md) for background on conformal
prediction in bandit settings.

#### `conformal.coverage`

```
type:    float
default: 0.90
range:   [0.50, 0.999] (clamped)
```

The nominal marginal coverage of the conformal intervals. At 0.90, the true
reward is expected to fall within the interval on 90% of future feedback events.
Higher coverage produces wider intervals.

#### `conformal.calibrationSize`

```
type:    integer
default: 100
minimum: 10 (floored)
```

The number of recent feedback events used to calibrate the conformal quantile.
The parser floors this to 10. Smaller calibration sets update faster but
produce less reliable coverage guarantees.

### Pareto

#### `pareto.enabled`

```
type:    boolean
default: false
```

When true, the selection policy favours options that are not Pareto-dominated
across the objectives listed in `pareto.objectives`. An option is
Pareto-dominated if there exists another option that is at least as good on
every objective and strictly better on at least one. Use for multi-objective
settings where no single scalar aggregation is appropriate.

#### `pareto.objectives`

```
type:    list of string
default: []
```

The names of the reward components to treat as separate objectives for Pareto
filtering. Each name must correspond to a component declared in the capsule
YAML's `reward.components` list. When `pareto.enabled` is true and this list is
empty, Pareto filtering is a no-op.

### Context spec

#### `contextSpec.type`

```
type:    string
default: "discrete"
values:  "discrete" | "features"
```

Declares how context is encoded at decision time. This field — together with the
rest of the `contextSpec` block — is part of the learning config, not the
capsule YAML. The distinction matters: the capsule YAML's `contexts` list
registers discrete dimension names at compile time, whereas `contextSpec`
attached via the learning config declares the full feature schema and may be
updated after installation.

`"discrete"` — the context is an opaque string key, as declared by the capsule
YAML's `contexts` field. The bandit tracks separate reward estimates per context
string. Appropriate when context cardinality is small and the context values are
enumerable (e.g., customer tier, region code).

`"features"` — the context is a named feature vector. The bandit uses a
contextual model (eligible for LinUCB-style updates) that generalises across
unseen context values. Required when context cardinality is large or continuous
(e.g., latency, user age, hour of day). Feature dimensions are declared in the
`features` array.

When you change `contextSpec` after install (for example, adding a new feature),
the existing learned weights for the old context shape are discarded and the
model restarts from an uninformative prior. This is expected behaviour: the
encoded feature vector changes dimension, so the old weights do not transfer.
Plan schema migrations accordingly — canary the new config on a small traffic
slice before full rollout.

#### `contextSpec.features`

```
type:    array of FeatureSpec
applies: when contextSpec.type = "features"
```

Each element declares one named feature. Fields:

```
name    string        required
type    FeatureType   required
```

`FeatureType` is a tagged union. The `kind` discriminator takes one of three
values:

`{ kind: "continuous", range: [min, max] }` — a real-valued feature.
`range` is optional; if supplied, the feature is linearly normalised to [0, 1]
and clamped at the boundaries before encoding. If omitted, the raw value is
passed through unchanged. Produces one encoded dimension.

`{ kind: "categorical", values: ["a", "b", "c", ...] }` — a nominal
categorical feature. Encoded as (n-1) one-hot dimensions (the first declared
value is the reference level, dropped per standard convention). At decision
time the caller must supply one of the declared `values` exactly; an
undeclared value is a hard error. Produces `len(values) - 1` encoded
dimensions.

`{ kind: "cyclic", period: N }` — a cyclic numeric feature. Encoded as
`[sin(2π·x/N), cos(2π·x/N)]`, preserving the topology of the cycle.
`period` must be positive. Appropriate for hour-of-day (period 24), day-of-week
(period 7), month-of-year (period 12). Produces two encoded dimensions.

The total encoded dimension is the sum of all feature dimensions plus one bias
term appended automatically by the encoder. You do not need to declare the bias
explicitly.

### Refusal

#### `refusal.enabled`

```
type:    boolean
default: false
```

When true, the `/decide` endpoint may return a refusal response (HTTP 200 with
`refused: true`) instead of committing to an option, in cases where the
algorithm is insufficiently confident to make a reliable recommendation. Refusal
requires `conformal.enabled: true` to function; enabling refusal without
conformal is accepted but has no effect.

#### `refusal.coverage`

```
type:    float
default: 0.95
range:   [0.50, 0.999] (clamped)
```

The minimum conformal coverage required before the algorithm will commit to an
option. If the best option's prediction interval width exceeds
`refusal.maxIntervalWidth` at this coverage level, the request is refused.

#### `refusal.maxIntervalWidth`

```
type:    float
default: 0.5
minimum: 0.0
```

The maximum tolerable width of the conformal prediction interval. Requests where
the best option's interval is wider than this threshold are refused. Narrower
thresholds (e.g., 0.2) produce more refusals in high-uncertainty periods; wider
thresholds (e.g., 0.8) produce fewer. Set this relative to your reward range.

#### `refusal.oodThreshold`

```
type:    float
default: 0.8
range:   [0.0, 10.0] (clamped)
```

Out-of-distribution detection threshold. If the incoming feature vector lies
more than `oodThreshold` standard deviations from the distribution of the
training contexts seen so far, the request is refused regardless of interval
width. Set higher to be more permissive about novel contexts; lower to refuse
aggressively on distributional shift.

### Mode shortcuts

The `mode` key is a convenience preset and is not part of the official schema
(it does not appear in the learning config's serialised form), but `from_json`
recognises it:

`"highThroughput"` — sets `safety.snapshotOnFeedback: false` and
`safety.journalOnFeedback: false`. Use when write latency is a constraint and
you accept the possibility of losing up to one feedback event on crash.

`"highAssurance"` — sets `safety.snapshotOnFeedback: true`,
`safety.journalOnFeedback: true`, and `safety.rewardClip: 1.0`. Use for
regulated or safety-adjacent deployments.

---

## 3. Reward function design

### Reward type selection

Choose `bernoulli` when the outcome of a decision is cleanly binary — a
conversion happened or it did not, an error was raised or it was not, a retry
succeeded or it failed. Thompson sampling with a Beta-Bernoulli posterior is
well-calibrated for this shape and converges quickly when feedback is dense.

Choose `continuous` when the outcome is a measured quantity with known bounds:
quality scores, latency penalties, user satisfaction ratings. The bounds must be
genuinely fixed across all traffic; if the range can vary (e.g., spend that
scales with request size), prefer normalising it first or use `normalize: budget`
in a reward component. The `continuous` type requires a `range` declaration, and
the `auto` algorithm resolves to weighted averaging, which is appropriate when
feedback is reliable and frequent.

Choose `sparse_continuous` when the continuous outcome is only sometimes
observed — for example, long-horizon outcomes that arrive days after the decision
and may never arrive for some decisions. UCB1's exploration bonus is more robust
to low feedback density than weighted averaging because it explicitly widens
confidence bounds for under-visited options.

### The reward-blindness pattern and how to avoid it

Any reward function that credits an outcome relative to a counterfactual derived
from the policy's own behaviour can become insensitive to the magnitude of what
it is supposed to be optimising. The mechanism is described in detail in
[`../../writeup_reward_blindness.md`](../../writeup_reward_blindness.md). The
practical consequence: five policies spanning a 4x difference in the outcome
they actually deliver can score within 0.056 points of each other if the reward
baseline shrinks when prevention works. An optimiser — or a bandit — cannot
learn to prefer the better policy from a reward signal this flat.

The diagnostic is a monotonicity check: construct a sequence of options or
configurations that you know, by construction, to be strictly better than each
preceding entry. The reward function must assign non-decreasing scores to this
sequence. If it does not, the baseline is policy-dependent and the reward is
blind to some or all of the quality difference you care about.

The fix is to compute credit against a fixed baseline — a pre-computed
counterfactual trajectory that does not depend on the policy being scored. In
the epidemiological example from the writeup, replacing
`counterfactual_deaths = true_new_cases / multiplier × cfr` (policy-dependent)
with `reference_no_intervention_deaths[region][week]` (pre-computed, fixed)
turned a 0.056-wide flat landscape into a 1.40-wide monotone one. One line of
code; the consequence is the difference between a reward function that ranks
policies correctly and one that cannot.

Applied to Syntra reward functions: when computing a `continuous` reward that
subtracts a prevented-event count from a baseline, ensure the baseline does not
shrink when the capsule chooses a more effective option. If your reward depends
on observed volumes (requests handled, errors seen, items queued), check whether
a policy that performs better causes the volume to fall, and if so, whether
dividing by that volume collapses the credit. The reliable pattern is to
normalise against a fixed external reference — a target SLA, a historical
baseline computed before the capsule was deployed, or an absolute count rather
than a fraction.

---

## 4. Worked examples

### 4.1 Simple Bernoulli A/B test

Three headline variants; no feature context; pure Thompson sampling.

**Capsule YAML:**

```yaml
name: headline-ab
version: 1.0.0
options:
  - control          # original headline copy
  - variant_a        # shorter, punchier copy
  - variant_b        # benefit-led copy
reward:
  type: bernoulli    # click = 1.0, no-click = 0.0
algorithm:
  type: thompson     # explicit; same as auto for bernoulli
```

**Learning config (JSON):**

```json
{
  "algorithm": "thompsonSampling",
  "safety": {
    "minExploration": 0.05,   // higher floor to ensure each variant stays visible
    "snapshotOnFeedback": true
  },
  "contextSpec": {
    "type": "discrete"        // no feature context; context key could be locale
  }
}
```

Notes: No `range` is needed for a Bernoulli reward. The 0.05 exploration floor
ensures each variant receives at least 5% of impressions indefinitely, which is
appropriate if you intend to do significance testing alongside the bandit. For a
pure bandit run, 0.02 (the default) is sufficient.

### 4.2 Multi-armed retry-policy bandit with feature context and refusal

Five retry policies; three continuous features; refusal enabled to avoid
recommending a policy when uncertainty is too high.

**Capsule YAML:**

```yaml
name: retry-tuning
options:
  - none
  - single
  - triple
  - exponential_fast
  - exponential_slow
reward:
  type: continuous
  range: [-1.0, 1.0]   # -1 = max cost/failure, +1 = fast success
algorithm:
  type: auto            # resolves to weighted for continuous reward
learning:
  min_exploration: 0.03 # slightly above default; 5 options need more probing
```

**Learning config (JSON):**

```json
{
  "learningRate": 0.03,
  "contextSpec": {
    "type": "features",
    "features": [
      {
        "name": "recent_failure_rate",
        "type": { "kind": "continuous", "range": [0.0, 1.0] }
        // normalised to [0,1]; 0 = no recent failures, 1 = all failing
      },
      {
        "name": "p99_latency_ms",
        "type": { "kind": "continuous", "range": [0.0, 5000.0] }
        // values above 5000ms are clamped to 1.0 after normalisation
      },
      {
        "name": "hour",
        "type": { "kind": "cyclic", "period": 24.0 }
        // sin/cos encoding; hour 0 and hour 24 map to the same point
      }
    ]
  },
  "conformal": {
    "enabled": true,
    "coverage": 0.90,
    "calibrationSize": 200   // larger than default; 5 options need more calibration events
  },
  "refusal": {
    "enabled": true,
    "coverage": 0.90,
    "maxIntervalWidth": 0.4, // refuse if best option's interval is wider than 0.4
    "oodThreshold": 1.5      // refuse if context is more than 1.5 std-devs out of distribution
  }
}
```

Notes: This YAML is the canonical demo from `docker/demo/capsule/retry_tuning.yaml`
with the feature context from `examples/retry-tuning/setup_capsule.py`. The cyclic
`hour` feature ensures that hour 23 and hour 0 are treated as adjacent by the
linear model.

### 4.3 Multi-objective reward with three reward components

An LLM routing capsule that balances output quality, latency, and cost.

**Capsule YAML:**

```yaml
name: llm-router
version: 0.2.0
options:
  - cheap_fast            # small, fast model
  - balanced              # mid-tier model
  - expensive_accurate    # frontier model
reward:
  type: continuous
  range: [-1.0, 1.0]
  components:
    - name: quality
      weight: 0.60          # primary objective
      normalize: minmax
      range: [0.0, 1.0]     # quality score already in [0,1]
    - name: latency_ms
      weight: -0.20         # cost; negative weight penalises high latency
      normalize: budget
      budget: 2000.0        # 2000 ms is the "full budget"; latency/2000 in feedback
    - name: cost_usd
      weight: -0.20         # cost; negative weight penalises high spend
      normalize: budget
      budget: 0.05          # $0.05 per call is the budget reference point
algorithm:
  type: auto                # resolves to weighted for continuous
```

**Learning config (JSON):**

```json
{
  "safety": {
    "minExploration": 0.02,
    "rewardClip": 1.5       // tighter than default; cost spikes should not dominate
  },
  "decay": {
    "enabled": true,
    "halfLifeFeedbacks": 500,  // model pricing changes slowly; longer memory is fine
    "halfLifeSeconds": 2592000  // 30 days in seconds
  },
  "contextSpec": {
    "type": "features",
    "features": [
      {
        "name": "task_type",
        "type": { "kind": "categorical", "values": ["summarise", "classify", "generate", "code"] }
        // first level ("summarise") is the reference; produces 3 one-hot dimensions
      },
      {
        "name": "customer_tier",
        "type": { "kind": "categorical", "values": ["free", "pro", "enterprise"] }
        // "free" is the reference level
      }
    ]
  }
}
```

Notes: Reward components are aggregated by the runtime into a single scalar.
The caller's feedback payload must supply a value for each component name. The
weighted sum is `0.60 * (quality/1.0) - 0.20 * (latency_ms/2000) - 0.20 *
(cost_usd/0.05)`. Both latency and cost are normalised by their budgets before
the weight is applied, so a latency of 2000 ms contributes -0.20 (full budget
consumed) and a cost of $0.025 contributes -0.10 (half budget).

### 4.4 Cyclic feature for hour-of-day routing

A content-routing capsule where the best option changes with time of day.

**Capsule YAML:**

```yaml
name: content-router
options:
  - news_feed             # topical content; performs well in morning
  - social_feed           # engagement content; performs well in evening
  - utility_feed          # transactional content; performs steadily all day
reward:
  type: bernoulli         # did the user engage with the selected feed?
```

**Learning config (JSON):**

```json
{
  "algorithm": "thompsonSampling",
  "contextSpec": {
    "type": "features",
    "features": [
      {
        "name": "hour_utc",
        "type": { "kind": "cyclic", "period": 24.0 }
        // encodes hour as [sin(2π·h/24), cos(2π·h/24)]
        // hour 0 and hour 24 are identical; hour 12 is maximally distant from hour 0
      },
      {
        "name": "day_of_week",
        "type": { "kind": "cyclic", "period": 7.0 }
        // day 0 (Monday) and day 7 are identical
      }
    ]
  },
  "decay": {
    "enabled": true,
    "halfLifeSeconds": 604800  // one week; old engagement patterns fade over time
  },
  "safety": {
    "minExploration": 0.05   // keep all three feeds visible; small option set
  }
}
```

Notes: Cyclic encoding is essential here. A naive continuous encoding would
treat hour 23 as far from hour 0, breaking the model's ability to learn
continuity across midnight. The `(sin, cos)` pair preserves circular distance:
the Euclidean distance between the encodings of hour 23 and hour 0 is
approximately 0.27, the same as the distance between hour 0 and hour 1.

---

## 5. Validation rules and common errors

### Install-time validation (`CapsuleSpec::validate`)

The following rules are enforced by `src/capsule_spec.rs::validate()` and
produce an error from `syntra author` before any `.lyc` file is written.

**Empty name.** `name` must not be empty or consist only of whitespace. Error
message: `"name is required"`.

**Fewer than two options.** `options` must contain at least two entries. Error
message: `"options must contain at least two entries"`. A single-option capsule
is not meaningful; the algorithm has nothing to choose between.

**Empty option string.** Each entry in `options` must be a non-empty, non-
whitespace string. Error message: `"options[N] must not be empty"` where N is
the zero-based index.

**Continuous reward without range.** When `reward.type` is `continuous`, the
`reward.range` field must be present. Error message: `"reward.range is required
when reward.type is continuous"`. The `bernoulli` and `sparse_continuous` types
do not trigger this check.

**Empty component name.** Each entry in `reward.components` must have a non-
empty `name`. Error message: `"reward.components[N].name must not be empty"`.

**Duplicate component name.** Two entries in `reward.components` may not share
the same `name`. Error message: `"duplicate reward component name: {name}"`.

**Non-finite component weight.** A reward component's `weight` must be a finite
float. Error message: `"reward.components[name].weight is not finite"`. `NaN`
and `±Inf` are rejected.

**Minmax component without range.** A reward component with `normalize: minmax`
must supply a `range`. Error message: `"reward.components[name] has normalize:
minmax but no range"`.

**Budget component without budget.** A reward component with `normalize: budget`
must supply a `budget`. Error message: `"reward.components[name] has normalize:
budget but no budget"`.

### Runtime errors

**Feature vector dimension mismatch on `/decide`.**
If the incoming feature map supplies the wrong number of values, or if the
feature schema has changed since the last request, the encoder returns an error.
The symptom is an HTTP 400 with a message of the form `"missing feature 'x'"`.
This typically means the feature schema was updated via a learning config PUT
but the calling code was not updated to match, or vice versa. Ensure that every
feature declared in `contextSpec.features` is present in every `/decide`
request.

**Unknown categorical value.**
If a categorical feature receives a value not listed in the feature spec's
`values` array, the encoder rejects it with: `"feature 'x' got value 'y', not
in declared values [...]"`. Add the new value to `values` in the learning config
and redeploy; or normalise the incoming values to the declared set before
calling `/decide`.

**Type mismatch on feature value.**
Supplying a string where a numeric feature is expected, or a number where a
categorical feature is expected, produces: `"feature 'x' expects number, got
category"` (or the reverse). Check that numeric features receive `float`/`int`
JSON values and categorical features receive `string` JSON values.

**Context spec change after install.**
Changing `contextSpec` via a learning config PUT is allowed and takes effect
immediately. However, because the encoded feature vector changes dimension
(or structure), the existing learned weights are not compatible with the new
schema and are discarded. The model restarts from an uninformative prior. This
is not an error, but the resulting reset in policy quality can be significant on
a high-traffic capsule. Best practice: canary the new schema on a low-traffic
capsule copy first, validate that the feature encoding is correct, then migrate
the production capsule. See [`runbook.md`](runbook.md) for the migration
procedure.

**`conformal.enabled: false` with `refusal.enabled: true`.**
Enabling refusal without enabling conformal prediction has no effect — the
runtime has no interval widths to test against the threshold. No error is
returned, but no refusals will be issued. If refusal requests are not appearing
in your `/inspect` output, check that `conformal.enabled` is `true`.
