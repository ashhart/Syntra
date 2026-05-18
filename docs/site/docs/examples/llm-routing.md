# LLM model routing

Adaptive LLM model selection driven by a Syntra contextual-bandit
capsule.

Repository copy: [`examples/llm-routing/`](https://github.com/ashhart/Syntra/tree/main/examples/llm-routing).

## What it does

Most services route every LLM request to the same model — usually the
most capable one available. This is expensive: simple tasks that a
cheap-fast model handles well end up priced at premium rates, while
complex tasks that need a capable model are routed to a cheaper one
that produces lower-quality output.

`syntra_llm.LLMRouter` lets you ask Syntra to pick a model on a
per-request basis. Every call:

1. Computes the request context (prompt token count, caller-supplied
   task complexity, customer tier, hour of day).
2. Calls Syntra `/decide` with those features. Syntra returns an
   option index referring to one of three built-in routes
   (`cheap_fast`, `balanced`, `expensive_accurate`).
3. The caller invokes the chosen model and measures quality, latency,
   and cost.
4. Reports those measurements back to Syntra `/feedback` as a
   weighted reward.

Over many requests, Syntra's meta-bandit learns which model works
best for which combination of inputs, and converges on cost-efficient
routing.

## Quick start

```bash
docker run --rm -p 8787:8787 -p 8080:8080 syntra:demo
cd Syntra/examples/llm-routing
pip install -e .
```

```python
import os
from syntra_llm import LLMRouter

router = LLMRouter(
    syntra_url="http://localhost:8787",
    capsule_path="/tenants/myteam/jobs/llm/capsules/router",
    admin_key=os.environ["SYNTRA_ADMIN_KEY"],
)

decision = router.choose(
    prompt_token_count=1200,
    task_complexity=0.7,
    customer_tier="pro",
)
# call decision.model_name with your LLM client
router.report(
    decision_id=decision.decision_id,
    model_name=decision.model_name,
    quality=0.88,
    latency_ms=1100.0,
    cost_usd=0.012,
)
```

## Feature context

The capsule uses four features per request:

| Feature              | Type        | Range / Values                    |
|----------------------|-------------|-----------------------------------|
| `prompt_token_count` | continuous  | [0, 100000]                       |
| `task_complexity`    | continuous  | [0, 1] (caller-supplied estimate) |
| `customer_tier`      | categorical | "free", "pro", "enterprise"       |
| `hour`               | cyclic      | period 24 (UTC hour of day)       |

## Reward formula

```
reward = 0.6 * quality
       - 0.2 * clamp(latency_ms / 3000, 0, 1)
       - 0.2 * clamp(cost_usd   / 0.10, 0, 1)
reward = clamp(reward, -1, 1)
```

Weights are overridable via `LLMRouter` constructor arguments
(`quality_weight`, `latency_weight`, `cost_weight`).

## Model options

Three routes are installed by default:

| Option name          | Description                                      |
|----------------------|--------------------------------------------------|
| `cheap_fast`         | Low-cost, fast model for simple tasks            |
| `balanced`           | Mid-tier model balancing quality and cost        |
| `expensive_accurate` | High-quality model for complex tasks             |

Override the option list via the `SYNTRA_LLM_OPTIONS` environment
variable (comma-separated) before running `setup_capsule.py`:

```bash
export SYNTRA_LLM_OPTIONS="gpt35,gpt4o_mini,gpt4o,o3"
python setup_capsule.py
```

## Per-tier tracking

`LLMRouter` maintains an internal rolling window (last 100 calls) of
quality, latency, and cost per model name. Access it via
`router.tracker.features(name)` for monitoring dashboards or
alerting — it is informational and does not feed back into the
Syntra feature vector.

## Fail-safe behavior

- Syntra unreachable → use `fallback_model` (default: `balanced`).
- Syntra returns `refused: true` → use `fallback_model`; still post
  `/feedback` for audit purposes.
- Syntra returns a malformed response → use `fallback_model`.
- Feedback POST fails → silently swallowed.

## See also

- [HTTP retry tuning](retry-tuning.md) — the canonical Python
  integration pattern this mirrors.
- [Strategy node concept](../concepts/strategy-node.md) — what
  `/decide` is sampling from on each call.
