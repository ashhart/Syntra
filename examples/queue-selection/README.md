# Queue / backend selection with Syntra

Adaptive backend queue selection driven by a Syntra capsule.

## What it does

Most services route requests to backend queues using static rules (round-robin,
least-connections, or a fixed hash). These rules ignore signal that is already
available at request time: recent backend latency, error rate, current queue
depth, and request size.

`syntra_queue.QueueClient` lets Syntra pick the best backend on a per-request
basis. Every call:

1. Aggregates per-backend rolling stats (avg latency, error rate) plus the
   current queue depth and request size into a four-feature vector.
2. Calls Syntra `/decide` with those features. Syntra returns an option index
   referring to one of the configured backends (`backend_a`, `backend_b`,
   `backend_c` by default).
3. Returns a `BackendPick` with `backend_name` and `decision_id`.
4. After the request completes, `client.report(...)` sends the outcome (success +
   latency) to `/feedback`.

Over many requests Syntra's meta-bandit learns which backend works best under
which conditions and converges.

## Layout

```
syntra_queue/__init__.py    # the library (QueueClient, Backend, BackendPick, ...)
setup.py                    # pip-installable as `syntra-queue`
setup_capsule.py            # one-shot capsule installer for bare-metal Syntra
example_basic.py            # minimal usage example
tests/test_client.py        # 7 unit tests, run with `pytest tests/`
```

## Quick start

1. Run Syntra:

   ```bash
   docker run --rm -p 8787:8787 -p 8080:8080 syntra:demo
   ```

   For bare-metal Syntra, run `setup_capsule.py` to install the capsule:

   ```bash
   export SYNTRA_ADMIN_KEY=...
   python setup_capsule.py
   ```

   To install with a different number of backends:

   ```bash
   SYNTRA_QUEUE_N_BACKENDS=5 python setup_capsule.py
   ```

2. Install the integration library:

   ```bash
   cd Syntra/examples/queue-selection
   pip install -e .
   ```

3. Use it:

   ```python
   import os
   from syntra_queue import QueueClient

   client = QueueClient(
       syntra_url="http://localhost:8787",
       capsule_path="/tenants/myteam/jobs/queue/capsules/router",
       admin_key=os.environ["SYNTRA_ADMIN_KEY"],
   )

   pick = client.pick(
       request_size_kb=42.0,
       queue_depths={"backend_a": 5, "backend_b": 42, "backend_c": 0},
   )
   print(f"Routing to: {pick.backend_name}")

   # ... send the request ...

   client.report(pick.decision_id, pick.backend_name, success=True, latency_ms=87.0)
   ```

## Feature vector

The four features sent to Syntra on every `/decide` call:

| Feature | Range | Description |
|---|---|---|
| `avg_recent_latency_ms` | [0, 5000] | Rolling average latency across all backends |
| `recent_error_rate` | [0, 1] | Highest recent error rate across all backends |
| `current_queue_depth` | [0, 1000] | Mean queue depth across all backends |
| `request_size_kb` | [0, 10000] | Size of the current request payload |

## Reward formula

```
reward = clamp((1.0 if success else 0.0) - 0.0001 * latency_ms, -1.0, 1.0)
```

A successful 50 ms request yields a reward of 0.995. A failed request with
200 ms latency yields -0.02. The latency coefficient is small enough that fast
failures still score below slow successes.

## Fail-safe behavior

Every Syntra interaction is wrapped to keep your service alive even when
Syntra is not:

- Syntra unreachable → round-robin across known backends (no decisionId,
  so no feedback attempt).
- Syntra returns `refused: true` → round-robin, but the decisionId is kept
  and `/feedback` is still posted for the audit log.
- Syntra returns a malformed response → round-robin, no feedback.
- Feedback POST fails → silently swallowed; the request has already completed.

## Capsule configuration

The default three-backend setup is installed by `setup_capsule.py`. Two
override points:

- **Number of backends** — set `SYNTRA_QUEUE_N_BACKENDS` before running
  `setup_capsule.py`. Backends are named `backend_a`, `backend_b`, ...,
  `backend_z` (up to 26).
- **Backend list in code** — pass a custom `backends=[Backend("my-q"), ...]`
  list to `QueueClient`. Keep it in sync with the capsule YAML.

Path overrides (all optional):

| Env var | Default |
|---|---|
| `SYNTRA_URL` | `http://localhost:8787` |
| `SYNTRA_ADMIN_KEY` | _(required)_ |
| `SYNTRA_TENANT` | `myteam` |
| `SYNTRA_JOB` | `queue` |
| `SYNTRA_CAPSULE` | `router` |
| `SYNTRA_QUEUE_N_BACKENDS` | `3` |

## Tests

```bash
cd Syntra/examples/queue-selection
pip install -e .
pytest tests/
```

Seven unit tests cover: tracker neutral defaults, rolling-window stat math,
`Backend.from_option` OOB fallback, successful pick+report round-trip,
Syntra-unreachable round-robin fallback, refusal audit feedback, and
feedback-failure tolerance. All tests mock the `requests` module and do not
need a running Syntra.

## License

Apache-2.0. See top-level `LICENSE`.

## See also

- `../retry-tuning/` — adaptive HTTP retry policy selection (the canonical
  integration example this package mirrors).
- Top-level `../../README.md` — Syntra platform overview.
