# Demo verification log

Captured during the Part 3 verification run on 2026-05-18.

## Environment

- Host: macOS (Darwin 25.5.0), Apple Silicon
- Docker version 29.4.0
- Repo root: `/path/to/project`

## Build

```bash
cd /path/to/project
docker build -t syntra:demo -f Syntra/docker/Dockerfile.demo .
```

**Elapsed**: build completed during the prior agent's run. The Rust
release-build stage dominates; the agent's README estimates 6–8 min on
an M2 Mac cold-cache. Subsequent builds (with cargo's layer cached)
finish in well under a minute when only the demo helpers / capsule
YAMLs change.

**Image size**: ~600 MB (`debian:bookworm-slim` base + the two release
binaries + the Python demo helpers).

## Default run (predictive-autoscaling)

```bash
docker run --rm -d --name syntra-demo-test \
    -p 8787:8787 -p 8080:8080 \
    syntra:demo
```

### Observed at boot

```bash
docker logs syntra-demo-test 2>&1 | head -10
```

All four flagship capsules installed in sequence:

- `predictive-autoscaling`         → `demo/autoscale/orders`
- `anomaly-routing`                → `demo/routing/api`
- `seasonal-fraud-threshold`       → `demo/fraud/threshold`
- `shared-state-action-embeddings` → `demo/embeddings/router`

`GET /api/capsules` confirms all four are listed with correct
`scoringMode`:

```json
{
  "capsules": [
    {"name":"orders",    "path":"demo/autoscale/orders",  "scoringMode":"meta-bandit",
     "options":["option_0","option_1","option_2","option_3"]},
    {"name":"router",    "path":"demo/embeddings/router", "scoringMode":"shared-state-linucb",
     "options":["A","B","C","D","E","F"]},
    {"name":"threshold", "path":"demo/fraud/threshold",   "scoringMode":"meta-bandit",
     "options":["option_0","option_1","option_2","option_3"]},
    {"name":"api",       "path":"demo/routing/api",       "scoringMode":"meta-bandit",
     "options":["option_0","option_1","option_2","option_3"]}
  ]
}
```

### Observed at ~3 minutes (default predictive-autoscaling traffic)

```
capsulePath: demo/autoscale/orders
scoringMode: meta-bandit
lifecycle: active                  ← warmup→active transition complete
algorithm: weighted                 ← picked from BoundedContinuous characterization
totalDecisions: 196
totalMetaRounds: 166                ← 30-decision gap from warmup, as expected
publishedLatest: {"forecast": 25.99, "p95": 46.56, "recommended_instances": 1}
publishedSeries.forecast: 60 samples (ring buffer cap reached)
publishedSeries.p95:      60 samples
publishedSeries.recommended_instances: 60 samples

candidates (all 7 populated, sorted by trials desc):
  Greedy          trials=65.5  meanReward=0.913   ← current leader
  EpsilonGreedy   trials=19.5  meanReward=0.885
  Weighted        trials=18.1  meanReward=0.585
  Thompson        trials=14.5  meanReward=0.701
  Ucb             trials=11.9  meanReward=0.903
  LinTs           trials=11.9  meanReward=0.522
  LinUcb          trials=11.8  meanReward=0.673
```

### Observed at ~5 minutes (default predictive-autoscaling traffic)

```
totalDecisions: 227
totalMetaRounds: 197                ← +31 over 2 min, ~1 round/sec
refusedCount: 0                     ← no refusals (see "Rough edges" below)
algorithm: weighted
publishedLatest: {"forecast": 18.34, "p95": 43.10, "recommended_instances": 1}
publishedSeries: all three at 60 samples (capped)

top-3 candidates by trials:
  Greedy          trials=83.2  meanReward=0.911
  Weighted        trials=20.5  meanReward=0.609
  EpsilonGreedy   trials=18.9  meanReward=0.885

top-3 decision counts (by optionIndex):
  optionIndex=1 (forecast_match):     164
  optionIndex=2 (forecast_headroom):   34
  optionIndex=0 (hold):                21
```

The meta-bandit is exploring all seven candidates with Greedy in the lead.
Weight movement in the strategy node is visible: option index 1
(`forecast_match`) is winning the bulk of selections, reflecting that
the synthetic diurnal-load traffic isn't currently in a high-trend
window where `forecast_headroom` would dominate.

### Each capsule end-to-end (sanity)

`/api/capsules` lists all four. Switching the dashboard to each one
(via `#capsule=<path>` in the URL hash) returns correct `capsulePath`
in the next `/api/state` poll. The shared-state capsule correctly
flips `scoringMode` to `shared-state-linucb`.

## Capsule switch test (env var)

```bash
docker run --rm -d --name syntra-demo-test2 \
    -e SYNTRA_DEMO_CAPSULE=anomaly-routing \
    -p 8787:8787 -p 8080:8080 \
    syntra:demo
```

### Observed at ~60 seconds (anomaly-routing traffic)

```
capsulePath: demo/routing/api       ← env var routed correctly
scoringMode: meta-bandit
lifecycle: active
algorithm: ucb                       ← picked for the latency-anomaly reward shape
totalDecisions: 59
publishedLatest: {
  "lat_mean":   232.77,
  "lat_stddev": 203.16,
  "z_score":   -0.4846
}
publishedSeries keys: ['lat_mean', 'lat_stddev', 'z_score']
```

Traffic generator pivoted correctly: it now drives an
anomaly-routing-shaped context (latency window with outliers) instead
of the predictive-autoscaling order-volume pattern.

The dashboard's Region 5 sparklines for this capsule would show
`lat_mean`, `lat_stddev`, and `z_score` instead of
`forecast / p95 / recommended_instances` — confirmed via the
`publishedSeries` keys in `/api/state`.

## Rough edges

Honest list — things a user should know before running the demo:

1. **Cold build is slow.** First-time `docker build` takes 6–8 minutes
   on an M2 Mac due to the Rust release build of both Lang and Syntra
   from source. Hot rebuilds (only changing demo helpers) are under a
   minute. The Dockerfile is multi-stage so the Rust layer caches
   correctly. If you're iterating on demo helpers, don't rebuild from
   scratch.

2. **Image is ~600 MB.** That's the cost of bundling two release
   binaries plus the Python helpers on a slim Debian base. Acceptable
   for a demo; not optimised for production deployment.

3. **No refusal events fire in the default 5-minute run.** Refusal is
   opt-in via `refusal.enabled = true` in `learning.json` and none of
   the four flagship capsules has it enabled. The
   `refusedCount` stays at `0` for the entire run. If you want to
   exercise the refusal display, enable refusal on one of the demo
   capsules' `learning.json` before building the image. Documenting
   here rather than silently shipping a misleading default.

4. **Meta-bandit option labels are placeholders.** `/api/capsules`
   returns `option_0..option_{n-1}` for meta-bandit capsules because
   `.lyc` binaries don't preserve option labels. The shared-state
   capsule gets real labels (`A..F`) because they live in
   `learning.json::sharedState.optionFeatures`. The dashboard renders
   what's available; labels are correct for the shared-state capsule
   and generic for the others. This is a known v1 limitation
   documented in `Syntra/docs/known-issues.md` (capsule-label
   preservation is a separate follow-up).

5. **`/report` still returns `algorithm: None` / `warmup: None`** as
   documented in `Syntra/docs/known-issues.md`. The dashboard works
   around this by reading `warmup.json` from the volume and
   `metaBandit` from `/memory`. Anyone debugging from `/report`
   directly will see the gap.

6. **The dashboard's first /api/state response after boot can take
   2–3 seconds** because it cold-loads `/memory` and parses the
   decision JSONL. Subsequent polls (2-second cadence) are fast (<200
   ms). The 1px loading bar at the top of the page covers the
   first-load gap.

7. **The traffic generator is bounded to ~1 Hz** (one `/decide` +
   `/feedback` per second). At higher rates the dashboard's 2-second
   poll cadence misses detail. This is by design — the demo is meant
   to be glanceable, not load-test-worthy.

## What was NOT verified

- **Real-browser rendering.** Verification was at the API layer
  (`curl /api/state`, `curl /api/capsules`) and the Docker boot
  pipeline. The actual SVG chart rendering, the legend toggle
  interaction, the fade-in animation, and the 1440×900 no-scroll
  constraint were not validated in a browser session. Source-level
  inspection of `dashboard.js`, `chart.js`, `style.css`, and
  `index.html` shows the spec being followed; behaviour in a real
  browser is the next sanity check.

- **Refusal display.** Not exercised because refusal isn't enabled in
  any of the four flagship capsules' learning configs. The dashboard's
  Region 4 code path for refused entries (red dot + REFUSED label +
  refusal reason) was inspected at the source level only.

## Summary

The demo builds, runs, installs all four flagship capsules at boot,
drives traffic against the selected capsule via the
`SYNTRA_DEMO_CAPSULE` env var, surfaces published kernel outputs in
`/api/state.publishedLatest` and `publishedSeries`, and the dashboard
serves all four regions of UI through the API contract. The
meta-bandit reaches `lifecycle: active` after ~30 feedback rounds and
populates all seven candidate algorithm trials; Greedy emerged as the
current leader on the predictive-autoscaling traffic pattern.

For first-time evaluators, the default flow (no env var) drops you on
the most demonstrative capsule and is the right starting point.

---

## Follow-up verification — 2026-05-18 (all five capsules + hierarchical)

Captured after Phase I followups 4–12 closed hierarchical bandits end
to end and added the hierarchical demo capsule to the Docker image.
The original Part-3 observations above are preserved as the historical
record of a four-capsule run; this section documents what an operator
now sees with the five-capsule line-up.

### Boot install

`install.py` now installs five capsules in sequence. Sample stdout:

```
[install] installing 5 flagship capsules into http://127.0.0.1:8787
[install] predictive-autoscaling         -> demo/autoscale/orders   hash=… + learning.json
[install] anomaly-routing                -> demo/routing/api        hash=… + learning.json
[install] seasonal-fraud-threshold       -> demo/fraud/threshold    hash=… + learning.json
[install] shared-state-action-embeddings -> demo/embeddings/router  hash=… + learning.json
[install] hierarchical-region-routing    -> demo/region/router      hash=… + learning.json + hierarchical_spec.json
[install] all capsules installed
```

The new hierarchical capsule includes an extra `PUT
/hierarchical_spec` step, annotated in the log line as
`+ hierarchical_spec.json`.

### `GET /admin/capsules` (post-install, no traffic yet)

```
demo/autoscale/orders     scoringMode=meta-bandit          options=[option_0..option_3]
demo/embeddings/router    scoringMode=shared-state-linucb  options=[A, B, C, D, E, F]
demo/fraud/threshold      scoringMode=meta-bandit          options=[option_0..option_3]
demo/region/router        scoringMode=hierarchical         options=[us_small, us_medium, us_large, eu_small, eu_medium, eu_large]
demo/routing/api          scoringMode=meta-bandit          options=[option_0..option_3]
```

All three adaptive flavors are distinguishable at the listing layer.
The hierarchical capsule gets real leaf labels (one notch better than
the meta-bandit's `option_N` placeholders) because the tree carries
proper names.

### `SYNTRA_DEMO_CAPSULE=hierarchical-region-routing` run

```bash
docker run --rm -d --name syntra-demo-test \
    -e SYNTRA_DEMO_CAPSULE=hierarchical-region-routing \
    -p 8787:8787 -p 8080:8080 \
    syntra:demo
```

The reward function in `traffic/generate.py` for this capsule
encodes a clean per-level signal: `region_bonus = us:0.30 / eu:0.00`
+ `size_bonus = small:0.00 / medium:0.40 / large:0.20`.

**~106 decisions, ~8 seconds at 0.05s tick interval** (faster than
the 1Hz default for fast verification):

- Leaf histogram: us_large=23, us_medium=23, us_small=18, eu_large=15,
  eu_medium=14, eu_small=13. US-side picks 60% of total — region
  preference correctly tilting toward us.
- `hierarchical_state.json` buckets:
  - `d0|` (root): weights `[0.67, 0.33]` — us preferred at 67%.
  - `d1|0` (us subtree): `[0.22, 0.45, 0.34]` — medium > large > small,
    matching the size bonuses.
  - `d1|1` (eu subtree): `[0.15, 0.55, 0.30]` — same medium-first
    ordering. Both subtrees independently converged on the size
    signal even though the regions differ — that's the
    hierarchical-bandit benefit of credit sharing within each parent.

Dashboard's Region 2 renders three lines (one per bucket) labelled
`d0| [Thompson]`, `d1|0 [Thompson]`, `d1|1 [Thompson]`, with heights
matching each bucket's leader-mean.

### Rough edges added since the original verification

- **Hierarchical capsules don't execute their `.lycs` graph in v1**,
  so `runtime.publish` calls inside a hierarchical capsule's program
  body do not fire. Region 5 (Live kernel outputs) shows a
  placeholder card. This is consistent with the v1 limitations
  documented in `Syntra/docs/roadmap.md` "Future polish".
- The `chosen_option` field on a hierarchical decision response is
  always `0` (legacy parity); the actual chosen leaf is in
  `decisions[0].leafName`. Traffic generator handles both — clients
  consuming `/decide` directly need to know which adaptive flavor
  they're hitting.
