# HTTP retry tuning

Adaptive HTTP retry policy selection driven by a Syntra capsule. The
canonical Python integration pattern — every other domain pack
(`fraud-tuning`, `llm-routing`, `queue-selection`) mirrors this shape.

Repository copy: [`examples/retry-tuning/`](https://github.com/ashhart/Syntra/tree/main/examples/retry-tuning).

## What it does

Most services use a single retry policy for every endpoint — 3 retries
with exponential backoff, applied uniformly. This is a compromise: too
aggressive for some endpoints (extra latency on a cleanly-failing
4xx), not aggressive enough for others (a flaky upstream that benefits
from longer backoff).

`syntra_retry.RetryClient` lets you ask Syntra to pick a retry policy
on a per-request basis. Every call:

1. Computes features describing the current state of the destination
   (recent failure rate, p99 latency, hour of day).
2. Calls Syntra `/decide` with those features. Syntra returns an
   option index referring to one of five built-in policies (`none`,
   `single`, `triple`, `exponential_fast`, `exponential_slow`).
3. Executes your HTTP call with that policy applied.
4. Reports the outcome (success / total latency) back to `/feedback`.

Over many requests, Syntra's meta-bandit learns which policy works
best for which endpoint conditions, and converges.

## Layout

```
syntra_retry/__init__.py    # the library (RetryClient, RetryPolicy, …)
setup.py                    # pip-installable as `syntra-retry`
setup_capsule.py            # one-shot capsule installer for bare-metal Syntra
example_basic.py            # minimal usage against httpbin.org
tests/test_client.py        # 6 unit tests, run with `pytest tests/`
```

## Quick start

1. Run Syntra. The fastest path is the demo image:

   ```bash
   docker run --rm -p 8787:8787 -p 8080:8080 syntra:demo
   ```

   That installs a retry-tuning capsule at
   `/tenants/demo/jobs/retry/capsules/router`. For bare-metal Syntra,
   run `setup_capsule.py` instead to install one at
   `/tenants/myteam/jobs/retry/capsules/router`.

2. Install the integration library:

   ```bash
   cd Syntra/examples/retry-tuning
   pip install -e .
   ```

3. Use it:

   ```python
   import os
   from syntra_retry import RetryClient

   client = RetryClient(
       syntra_url="http://localhost:8787",
       capsule_path="/tenants/myteam/jobs/retry/capsules/router",
       admin_key=os.environ["SYNTRA_ADMIN_KEY"],
   )

   response = client.request("GET", "https://api.example.com/users")
   ```

   `client.request(...)` accepts the same `method, url, **kwargs` as
   `requests.request` and returns the final `requests.Response`.

## What's happening under the hood

The library tracks rolling-window stats per destination host (success
rate and p99 latency over the last 100 requests), uses those plus
current hour of day as the feature vector, and sends them as JSON to
Syntra `/decide`.

The capsule installed by `setup_capsule.py` declares a
feature-context learning config, so Syntra runs all six meta-bandit
candidates including LinUCB. After warmup (~30 feedback rounds), the
meta-bandit will start to favor whichever candidate is doing the best
job of mapping features → optimal retry policy.

## Fail-safe behavior

Every Syntra interaction is wrapped to keep your service alive even
when Syntra isn't:

- Syntra unreachable → use `fallback_policy` (default: `single`).
- Syntra returns `refused: true` → use `fallback_policy`.
- Syntra returns a malformed response → use `fallback_policy`.
- Feedback POST fails → silently swallowed; the user's request already
  succeeded.

You should monitor Syntra availability separately from your service.
A Syntra outage degrades adaptive retry to "always fall back" until
it recovers — it does not break the request flow.

## Customizing for your service

The default five-policy set is reasonable but probably not optimal
for your service. Two override points:

- **Capsule options** — edit `CAPSULE_SPEC` in `setup_capsule.py` to
  list your own policy names, then re-run `setup_capsule.py`.
- **`RetryPolicy.from_option`** — in your application, replace the
  default mapping with your own (the option list in `__init__.py` is
  just a reasonable starting set).

The feature set in `LEARNING_CONFIG` is also customizable. Adding
features you already track in your service (current queue depth,
recent error rate from your monitoring system, customer tier)
usually improves convergence — Syntra will figure out which features
actually matter and put weight on them via the LinUCB candidate.

## Tests

```bash
cd Syntra/examples/retry-tuning
pip install -e .
pytest tests/
```

Six unit tests cover the endpoint tracker math, policy lookup, the
happy path, Syntra-unreachable fallback, refusal fallback, and
feedback-failure tolerance. They mock the `requests` module and do
not need a running Syntra.

## See also

- [Language clients](language-clients.md) — Go, Node, Java, Rust
  versions of the same pattern.
- [LLM model routing](llm-routing.md) — same pattern, different
  domain.
- [Refusal](../concepts/refusal.md) — the confidence-based fallback
  signal the client checks.
