# LLM model routing with Syntra

Adaptive LLM model selection driven by a Syntra contextual-bandit capsule.

## What it does

Most services route every LLM request to the same model — usually the most
capable one available. This is expensive: simple tasks that a cheap-fast model
handles well end up priced at premium rates, while complex tasks that need a
capable model are routed to a cheaper one that produces lower-quality output.

`syntra_llm.LLMRouter` lets you ask Syntra to pick a model on a per-request
basis. Every call:

1. Computes the request context (prompt token count, caller-supplied task
   complexity, customer tier, hour of day).
2. Calls Syntra `/decide` with those features. Syntra returns an option index
   referring to one of three built-in routes (`cheap_fast`, `balanced`,
   `expensive_accurate`).
3. The caller invokes the chosen model and measures quality, latency, and cost.
4. Reports those measurements back to Syntra `/feedback` as a weighted reward.

Over many requests, Syntra's meta-bandit learns which model works best for
which combination of inputs, and converges on cost-efficient routing.

## Layout

```
syntra_llm/__init__.py    # the library (LLMRouter, Model, RouteDecision, …)
setup.py                  # pip-installable as `syntra-llm-routing`
setup_capsule.py          # one-shot capsule installer for bare-metal Syntra
example_basic.py          # minimal usage demo (real or synthetic LLM calls)
tests/test_client.py      # 7 unit tests, run with `pytest tests/`
```

## Quick start

1. Run Syntra:

   ```bash
   docker run --rm -p 8787:8787 -p 8080:8080 syntra:demo
   ```

   For bare-metal Syntra, run `setup_capsule.py` to install a routing capsule
   at `/tenants/myteam/jobs/llm/capsules/router`.

2. Install the integration library:

   ```bash
   cd Syntra/examples/llm-routing
   pip install -e .
   ```

3. Use it:

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

Weights are overridable via `LLMRouter` constructor arguments (`quality_weight`,
`latency_weight`, `cost_weight`).

## Model options

Three routes are installed by default:

| Option name          | Description                                      |
|----------------------|--------------------------------------------------|
| `cheap_fast`         | Low-cost, fast model for simple tasks            |
| `balanced`           | Mid-tier model balancing quality and cost        |
| `expensive_accurate` | High-quality model for complex tasks             |

Override the option list via the `SYNTRA_LLM_OPTIONS` environment variable
(comma-separated) before running `setup_capsule.py`:

```bash
export SYNTRA_LLM_OPTIONS="gpt35,gpt4o_mini,gpt4o,o3"
python setup_capsule.py
```

## Per-tier tracking

`LLMRouter` maintains an internal rolling window (last 100 calls) of quality,
latency, and cost per model name. Access it via `router.tracker.features(name)`
for monitoring dashboards or alerting — it is informational and does not feed
back into the Syntra feature vector.

## Fail-safe behavior

Every Syntra interaction is wrapped to keep your service alive even when
Syntra is not:

- Syntra unreachable -> use `fallback_model` (default: `balanced`).
- Syntra returns `refused: true` -> use `fallback_model`; still post `/feedback`
  for audit purposes.
- Syntra returns a malformed response -> use `fallback_model`.
- Feedback POST fails -> silently swallowed; the route decision is unaffected.

## Capsule installer options

```bash
export SYNTRA_ADMIN_KEY=...
export SYNTRA_URL=http://my-syntra:8787   # optional
export SYNTRA_TENANT=myteam               # optional
export SYNTRA_JOB=llm                     # optional
export SYNTRA_CAPSULE=router              # optional
export SYNTRA_LLM_OPTIONS=cheap_fast,balanced,expensive_accurate  # optional
python setup_capsule.py
```

## Tests

```bash
cd Syntra/examples/llm-routing
pip install -e .
pytest tests/ -v
```

Seven unit tests cover the tracker math, model lookup, the happy-path
choose/report round-trip, Syntra-unreachable fallback, refusal fallback with
audit feedback, and feedback-failure tolerance. They mock `requests` and do not
need a running Syntra instance.

## License

Apache-2.0. See the top-level `LICENSE` file.
