# Syntra examples

Two audiences live here, kept clearly separate.

## Integration path — what most users want

[`retry-tuning/`](./retry-tuning/) — a Python package that wraps an HTTP client
with Syntra-driven retry policy selection. Read this first if you're trying to
use Syntra in a service.

```python
from syntra_retry import RetryClient
client = RetryClient(syntra_url=..., capsule_path=..., admin_key=...)
response = client.request("GET", "https://api.example.com/users")
```

Drop-in for `requests`. Falls back safely when Syntra is unreachable or refuses.

## Syntra-shaped demos

The user-level demos that ship with Syntra. They install YAML-authored capsules
and exercise the API:

- [`demo-static-policy-vs-syntra.sh`](./demo-static-policy-vs-syntra.sh) — installs
  a capsule, makes a decision with neutral weights, sends delayed feedback,
  restarts and proves the learned memory persisted.
- [`demo-llm-model-routing.sh`](./demo-llm-model-routing.sh) — the cleanest
  AI-app adoption demo: three model routes, two contexts, separate winners per
  context, persistence across restart.
- [`docker-quickstart/`](./docker-quickstart/) — disposable container, install,
  feedback, restart, persistence proof.
- [`curl/`](./curl/) — small curl-oriented walkthrough against an already-running
  Syntra server.
- [`authoring/`](./authoring/) — YAML capsule fixtures.
- [`quickstart_components_capsule/`](./quickstart_components_capsule/) — minimal
  capsule + run script.
- [`capsules/`](./capsules/) and [`proposals/`](./proposals/) — Syntra-side
  artifacts used by the demos above.

The two `.lyc` files at this level (`demo_takeaway_demand.lyc`,
`demo_llm_model_router.lyc`) are pre-compiled capsules the canonical demo
scripts install.

## Substrate demos — Lycan-level material

[`lycan-internals/`](./lycan-internals/) — Lycan-language source (`.lycs`) and
compiled binaries (`.lyc`) for substrate-level demos: orbit mechanics, fluid
dynamics, fraud detection, query planning, and so on. Plus the shell scripts
that compile and run them via the Lycan CLI.

These are working artifacts kept around for substrate-curious users.
**You don't need them to use Syntra.** Syntra users author capsules as YAML
and call the API; nothing in `lycan-internals/` is on that path.
