# Syntra demo image

A single self-hosted Docker image that ships:

- the **Syntra server** (`syntra serve` on port `8787`),
- the **dashboard** (port `8080`) — a read-only single-screen UI with a
  capsule switcher, live reward chart, decision distribution, recent
  decisions feed, and a kernel-output panel,
- **five flagship capsules** installed at boot, one per adaptive
  flavor plus a multi-decision example:
  - `predictive-autoscaling`         (`demo/autoscale/orders`)        — meta-bandit
  - `anomaly-routing`                (`demo/routing/api`)             — meta-bandit
  - `seasonal-fraud-threshold`       (`demo/fraud/threshold`)         — meta-bandit
  - `shared-state-action-embeddings` (`demo/embeddings/router`)       — shared-state LinUCB
  - `hierarchical-region-routing`    (`demo/region/router`)           — hierarchical bandit
- a **traffic generator** that drives one of the five capsules with a
  semi-realistic stream of `/decide` + `/feedback` rounds (1 Hz by
  default).

The dashboard never calls `/decide` or `/feedback`; it observes the
running capsule via `/memory`, `/decisions`, and the on-disk
`warmup.json` / `learning.json`. The traffic generator is the only
thing producing decisions inside the container — switch it via the
`SYNTRA_DEMO_CAPSULE` env var.

## Build

From the repo root (the directory containing `Lang/` and `Syntra/`):

```bash
docker build -t syntra:demo -f Syntra/docker/Dockerfile.demo .
```

Build time on an M2 Mac (cold Docker cache, no prior builds of the
Lang/Syntra crates): roughly **6–8 minutes**, almost all of it the
Rust release build. Subsequent builds (after editing only demo helpers
/ capsule YAMLs) take under a minute because the Rust layer is cached.
The verification run in `demo/VERIFIED.md` captures the actual elapsed
time observed during this image's last build.

The image is ~600 MB (debian:bookworm-slim base + the two release
binaries + the Python demo helpers).

## Run

```bash
docker run --rm -p 8787:8787 -p 8080:8080 syntra:demo
```

- Port `8787` — the Syntra HTTP API (bearer token printed in the
  container logs as `[demo] admin key: demo-key-<unix-ts>`).
- Port `8080` — the dashboard. Open `http://localhost:8080`.

Persistent store (decision log + capsule weights) lives at
`/syntra/data` inside the container. Mount a host directory there if
you want to keep state across runs:

```bash
docker run --rm -v $PWD/syntra-data:/syntra/data \
  -p 8787:8787 -p 8080:8080 syntra:demo
```

## Switching the active capsule

The dashboard's **dropdown** in the top-right of the header switches
which capsule the UI is viewing — it works on a running container with
no restart. Selecting a capsule rewrites the URL hash
(`#capsule=tenant/job/capsule`), so the choice is shareable / refresh-
persistent.

The **traffic generator**, on the other hand, only ever drives one
capsule per container. Pick which one with `SYNTRA_DEMO_CAPSULE`:

```bash
docker run --rm -e SYNTRA_DEMO_CAPSULE=anomaly-routing \
  -p 8787:8787 -p 8080:8080 syntra:demo
```

Valid values:

| `SYNTRA_DEMO_CAPSULE`                | Path the traffic generator hits          | Adaptive flavor          |
| ------------------------------------ | ---------------------------------------- | ------------------------ |
| `predictive-autoscaling` *(default)* | `demo/autoscale/orders`                  | meta-bandit              |
| `anomaly-routing`                    | `demo/routing/api`                       | meta-bandit              |
| `seasonal-fraud-threshold`           | `demo/fraud/threshold`                   | meta-bandit              |
| `shared-state-action-embeddings`     | `demo/embeddings/router`                 | shared-state LinUCB      |
| `hierarchical-region-routing`        | `demo/region/router`                     | hierarchical bandit      |

The other four capsules are still installed and visible in the
dropdown — they just sit idle in `warmup` until a caller posts
decisions to them.

## What to look for on the dashboard

The page is one screen, divided into five regions.

**Region 1 — header.** Wordmark, capsule path, lifecycle pill
(Warmup → Active → Frozen), capsule switcher dropdown. The pill is
amber during warmup, cyan once active, dim once frozen.

**Region 2 — reward chart.** A line per meta-bandit candidate. For
`meta-bandit` capsules, that's up to seven lines (Thompson, Ucb,
EpsilonGreedy, Weighted, Greedy, LinUcb, LinTs). For the shared-state
capsule the chart collapses to a single `SharedStateLinUcb` line. For
the **hierarchical** capsule the chart renders one line per HierState
bucket — root + one per branch — each labelled with the bucket key
(`d0|`, `d1|0`, `d1|1`) and the meta-bandit candidate currently
leading that level. The leading candidate within each chart context
renders 1.5x wider than the others. Clicking a legend entry toggles
that series.

**Region 3 — decision distribution.** Stacked bar showing how the
total decisions break down across this capsule's option labels. Labels
come from the capsule's `options[]` array (resolved via
`/api/capsules`) — for meta-bandit capsules they show as `option_0`,
`option_1`, … because the compiled `.lyc` doesn't preserve symbolic
names; the shared-state capsule shows the real names (`A`–`F`).

**Region 4 — recent decisions.** Last ~60 decisions: chosen option,
context-key hash, the algorithm that produced the pick, refusal status,
and how long ago. Refused decisions render in red.

**Region 5 — live kernel outputs.** A card per key that the capsule's
`.lycs` program publishes via `(!cap "runtime.publish" "<key>"
<value>)`. The card shows the current value (numbers formatted by
magnitude; strings rendered in accent cyan) and a 24px sparkline of
the last 60 samples.

Per-capsule expectations for Region 5:

- **predictive-autoscaling** — `forecast`, `p95`,
  `recommended_instances` update every 2 s as new load arrives.
- **anomaly-routing** — `lat_mean`, `lat_stddev`, `z_score` driven by
  the latency window.
- **seasonal-fraud-threshold** — `fraud_mean`, `fraud_p95`,
  `fraud_forecast`.
- **shared-state-action-embeddings** — placeholder card explaining how
  to wire up `runtime.publish`. This capsule intentionally doesn't
  publish anything (its job is to demonstrate the shared-state LinUCB
  path, not kernel outputs).
- **hierarchical-region-routing** — placeholder card. Hierarchical
  capsules do not execute their `.lycs` program in v1 (selection
  happens entirely outside the executor), so `runtime.publish` calls
  inside the program body would not fire. The dashboard tells you
  this rather than silently showing an empty Region 5.

## How to read the run

- **At ~10 s.** The header pill should already say *Warmup*.
  Region 3 has a small bar (~10 decisions). Region 4 is starting to
  fill. Region 2 is still empty (the meta-bandit hasn't picked a
  candidate yet; that requires warmup to complete first). Region 5
  has values populated and the sparklines are 5–10 points long.

- **At ~30 s.** ~30 decisions total. Region 2 may still be empty or
  showing just a couple of candidate samples — the meta-bandit only
  populates `/memory` once the per-capsule warmup target is reached.

- **At ~2 min.** The pill flips to *Active* once warmup completes
  (the predictive-autoscaling capsule's warmup target is around
  30–40 rounds). Region 2 shows multiple candidates with non-zero
  trial counts. Region 5's sparklines are filled to 60 points.

- **At ~5 min.** ~300 decisions. The meta-bandit will usually have
  picked a leader (look at the algorithm tag on most recent decisions
  in Region 4). The decision distribution in Region 3 is no longer
  uniform.

## Known rough edges

Honest list:

- **First build is slow.** Rust release build of two crates + their
  dependency closure (rusqlite, tracing-subscriber, ureq, …). Expect
  6–8 minutes on first build. The verification image in
  `demo/VERIFIED.md` was built incrementally so the time logged there
  is faster than a cold cache build.

- **Four of the five capsules sit idle.** The traffic generator only
  drives the one capsule chosen by `SYNTRA_DEMO_CAPSULE`. The other
  four show up in the dropdown but their dashboards report
  `lifecycle: unknown` and zero decisions until something posts to
  them. If you want to drive all five at once, run five containers
  side by side on different ports.

- **No refusal events.** Every capsule's `learning.json` ships with
  `refusal.enabled = false`. The dashboard's refusal counter will
  stay at zero for a full 5-minute demo run. (The flagship capsules
  are read-only for this image; the demo doesn't override them.)

- **Meta-bandit capsules show generic option labels.** Region 3 and
  Region 4 show `option_0`, `option_1`, … rather than `hold` /
  `forecast_match` / etc. The `.lyc` binary doesn't preserve symbolic
  option names; `/admin/capsules` returns positional placeholders.
  The shared-state capsule (which carries `optionFeatures` keys in
  `learning.json`) does show real names (`A`–`F`).

- **Shared-state Region 5 is a placeholder.** The
  `shared-state-action-embeddings` program is intentionally minimal —
  it doesn't call `runtime.publish`. Region 5 shows a muted
  placeholder card explaining how to wire one up.

- **Dashboard takes a few seconds to first-paint after container
  boot.** Flask's dev server takes ~2 s to start, and the first
  `/api/state` poll is empty if it arrives before any decisions have
  landed. Refresh in 5 seconds if the first load looks bare.

- **Container can't bind 8787 if another `syntra` is already
  running.** The image binds 0.0.0.0:8787 inside the container; on
  the host any other process on `:8787` will fail the `docker run`
  with "port is already allocated."
