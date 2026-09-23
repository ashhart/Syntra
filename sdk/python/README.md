# syntra (Python)

Decisions that learn, made in-process in microseconds.

`LocalDecider` keeps a copy of a capsule's published model and runs the
same decision code as the Syntra server, so `decide()` costs about a
microsecond or two and makes no network call; it keeps working if the server
is unreachable. Decisions and rewards upload in the background. The server
replays every uploaded decision to verify it, logs it with the probability
it was chosen with (for off-policy evaluation), and learns from its rewards.

```python
from syntra import LocalDecider

with LocalDecider("http://localhost:8787", token=TOKEN,
                  tenant="acme", job="prod", capsule="router") as router:
    d = router.decide({"task": "code", "promptTokens": 812})
    answer = call_model(d.action)          # act on the decision
    router.reward(d.decision_id, score(answer),
                  detail={"latencyMs": 840, "costUsd": 0.0031})
```

- `decide(context, actions=None, exclude=None, baseline=None)` returns a
  `Decision` with `action`, `probability`, `ranking`, `decision_id` and
  `model_version`. `actions` overrides the spec's actions for one call
  (`[{"id": "gpt-small", "features": {"cost": 0.1}}, ...]`).
- `reward(decision_id, value, idempotency_key=None, detail=None)` queues an
  outcome. Under `rewards: "sum"` each reward gets its own idempotency key,
  so retried uploads never double count.
- `sync_interval` (default 1 s) sets how often the background thread uploads
  and picks up newer models; `None` means you call `flush()` and `sync()`.
- `close()` (or leaving the `with` block) uploads everything still queued.

`Client` is a small HTTP client for server-side decisions
(`decide`, `reward`, `put_spec`, `model`, `decision`) with no native code.

## Building

The extension is Rust (PyO3, abi3 for Python 3.10+). Wheels:

```bash
pip install maturin
maturin build --release -m sdk/python/Cargo.toml
```

For development, `sdk/python/scripts/develop.sh` builds it with cargo and
drops it into `python/syntra/`; then run the tests against a debug server:

```bash
cargo build --bin syntra
sdk/python/scripts/develop.sh
python3 -m unittest discover -s sdk/python/tests -v
```
