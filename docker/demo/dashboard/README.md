# Syntra demo dashboard

Read-only single-screen dashboard for the Syntra demo container. Polls
the Syntra server for `/memory`, `/decisions`, and `/admin/capsules`,
reads `warmup.json` and `learning.json` directly off the on-disk store,
and renders a product-grade view of the capsule's lifecycle, the
meta-bandit's candidate algorithms (or the shared-state LinUCB line
when that mode is on), the live values its kernels publish via
`runtime.publish`, the choice distribution, and a feed of recent
decisions.

The dashboard never calls `/decide` or `/feedback`. It is a pure
observer.

See `LOOK.md` for a written visual reference at four lifecycle states
(cold start, mid-warmup, active, and fully-populated Phase 2).

## Files

| File | Role |
| ---- | ---- |
| `app.py`            | Flask backend. Serves `/`, `/static/*`, `/api/state`, `/api/capsules`. |
| `static/index.html` | Single-screen layout: header (incl. capsule switcher), reward chart, kernel-output panel, distribution, feed. |
| `static/style.css`  | Design tokens + layout. No external CSS dependency. |
| `static/dashboard.js` | Polls `/api/state` every 2s, reconciles DOM. Loads `/api/capsules` on boot + once a minute. |
| `static/chart.js`   | Hand-rolled SVG line chart (no Chart.js, no D3) plus a `renderSparkline` helper used by Region 5. |
| `README.md`         | This file. |
| `LOOK.md`           | Written visual description in lieu of screenshots. |

## How it works

1. Browser requests `/`. `app.py` serves `static/index.html`.
2. The page boots `dashboard.js`, which:
   - calls `GET /api/capsules` once to populate the switcher and cache
     the per-capsule `options[]` array used as option labels in
     Regions 3 and 4.
   - resolves the active capsule from the URL hash
     (`#capsule=tenant/job/capsule`), falling back to the first
     capsule returned by `/api/capsules` (alphabetical by path).
   - starts polling `GET /api/state?capsule=<path>` every 2 seconds.
3. `/api/state` does the work:
   - reads `warmup.json` for the capsule lifecycle (Warmup / Active /
     Frozen) — no `/decide` call needed
   - reads `learning.json` to detect `sharedState.enabled` — switches
     `scoringMode` between `meta-bandit` and `shared-state-linucb`
   - fetches `GET /memory` to enumerate candidate algorithms and their
     cumulative reward / trial counts
   - fetches `GET /decisions` (NDJSON, capped at the last 1000 lines)
     to compute choice counts, the refused count, the 60-row recent
     feed (most recent ~60 events), and the `publishedSeries` /
     `publishedLatest` derived from each entry's `published` map.
4. The reward chart appends each candidate's current mean to a 200-slot
   ring buffer per series. Y axis is 0..1 mean reward; X axis is the
   last 5 minutes sliding window.
5. The kernel-output panel reads `publishedLatest` to draw a card per
   key with the current value, and `publishedSeries[key]` (oldest-first,
   ≤60 entries) to render a 24px sparkline beneath each value.

## Capsule switcher

The header carries a dropdown listing every installed capsule. It is
fed by `/api/capsules`, which the dashboard proxies from Syntra's
`/admin/capsules` endpoint (the browser never holds the admin bearer —
the Python layer is the auth gateway).

Selecting a capsule:

- updates the URL hash to `#capsule=tenant/job/capsule`
- clears the reward-chart ring buffers and Region 5 sparkline cells
- re-polls `/api/state?capsule=<path>` immediately

The hash is the source of truth — paste a URL with a hash and the
matching capsule is auto-selected on load. If no hash is present, the
dashboard picks the first capsule by path (the same order
`/admin/capsules` sorts in). **Known limitation:** Phase 2 v1 picks
"first by path" rather than "most-active". A future revision could
sort by recent decision throughput.

When `/api/capsules` returns an empty list (no capsules installed
yet), the dropdown shows `(no capsules installed)` and the dashboard
falls back to the env-defined `DEMO_TENANT/DEMO_JOB/DEMO_CAPSULE`
path. This keeps the standalone-dev flow usable before any capsule
has been wired up.

## Live kernel outputs (Region 5)

Any capsule that calls `runtime.publish` inside its `.lycs` program
will surface those key/value pairs here. The card layout is:

- small label at the top — the published key name
- a large value below — the latest published value (number formatted
  to 0/2/4 decimals based on magnitude; strings shown directly in
  accent cyan)
- a 24px sparkline at the bottom — the last 60 samples for that key
- a 1px accent-coloured bottom border

Non-numeric publishes (e.g. status strings like `"smoke-ok"`) render
the value text but skip the sparkline. Capsules that publish nothing
get a single muted placeholder card explaining how to wire up
`runtime.publish`. This is the expected state for the
`shared-state-action-embeddings` example.

## Standalone development

You can run the dashboard against a local Syntra without the
container:

```bash
# 1. Bring up a Syntra server in dev mode.
syntra serve --dev-mode --addr 127.0.0.1:8787 --store /tmp/dash-test

# 2. Install one or more capsules. /admin/capsules requires at least
#    one capsule installed — otherwise the dropdown is empty and
#    the dashboard falls back to env defaults.
curl -X POST http://127.0.0.1:8787/tenants/ops/jobs/scale/capsules/autoscaler/install \
  -H "Authorization: Bearer dev" \
  -H "Content-Type: application/octet-stream" \
  --data-binary @/tmp/autoscaler.lyc

# 3. Point the dashboard at it.
export SYNTRA_URL=http://127.0.0.1:8787
export LYCAN_ADMIN_KEY=dev
export LYCAN_STORE_ROOT=/tmp/dash-test
# DEMO_TENANT/JOB/CAPSULE are only the *fallback* now — the URL hash
# and dropdown can switch capsules at runtime.
export DEMO_TENANT=ops DEMO_JOB=scale DEMO_CAPSULE=autoscaler

# 4. Start the dashboard on :8080.
python3 docker/demo/dashboard/app.py
```

Then open `http://localhost:8080`. Drive a few `/decide`+`/feedback`
rounds (any small bash loop will do) and the UI will start populating
within a poll or two.

To exercise the shared-state mode end to end, install the
`shared-state-action-embeddings` capsule and switch to it via the
dropdown — the chart will collapse to a single `SharedStateLinUcb`
line and Region 5 will show the muted placeholder card (this capsule
does not call `runtime.publish`).

To exercise the kernel-output panel, install any capsule that calls
`runtime.publish`. The Phase 2a smoke fixture at `/tmp/publish_smoke.lyc`
(produced by the `runtime-publish` work) is a useful stand-in: it
publishes `forecast`, `p95`, and `marker` from a six-element load
history.

## Design conventions

The page is one screen. There is no scroll at 1440x900, and the layout
degrades gracefully down to 1024x768 (header shrinks, card padding
reduces, Region 5 cards drop to 96px tall, but nothing wraps).

### Tokens (defined in `style.css`)

```
--bg            #0a0a0a    page background
--surface       #141414    card background
--border        #262626    1px borders, gridlines
--text-primary  #e5e5e5    titles, big numbers
--text-secondary #a0a0a0   labels
--text-tertiary #666666    captions, axis labels
--accent        #22d3ee    leader / active state / kernel-output highlight
--warmup        #f59e0b    warmup pill
--alert         #ef4444    refused decisions
```

### Typography

- Body: `Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif`
- Numbers / monospace: `"JetBrains Mono", "SF Mono", ui-monospace, monospace`
- 14px base, 12px captions, 18px wordmark / big numbers, 11px tertiary

### Spacing

- 24px gutters between regions
- 16px padding inside cards
- 8px between elements inside a card

### Charts

- No external library — the line chart is hand-rolled SVG in `chart.js`.
- Per-algorithm colour palette is stable across capsule switches
  (colour is keyed by algorithm name, not slot index).
- The leading candidate (highest current mean) renders 1.5x stroke
  width. Clicking a legend entry toggles that line's visibility.
- Region 5 sparklines reuse the same module — see
  `SyntraChart.renderSparkline(svg, values)`. They auto-scale to the
  series' min/max and render into a fixed `0 0 100 24` viewBox.

## Two scoring modes

`scoringMode` in `/api/state` is one of:

- `meta-bandit` (default) — Region 2 shows up to seven lines, one per
  candidate algorithm from `Lycan/src/meta_bandit.rs::CandidateId`:
  Thompson, Ucb, EpsilonGreedy, Weighted, Greedy, LinUcb, LinTs.
- `shared-state-linucb` — Region 2 shows a single line labelled
  `SharedStateLinUcb`. Engaged when the capsule's `learning.json`
  carries `sharedState.enabled = true`.

The dashboard detects the mode each poll from `learning.json` on disk;
the chart clears its buffers if the mode changes mid-session so stale
series don't bleed in.

## Phase 2 status

- **Option names are dynamic.** Region 3 (distribution) and Region 4
  (recent feed) pull labels from the selected capsule's `options[]`
  array, fetched via `/api/capsules`. The Phase 1 `OPTIONS_BY_CAPSULE`
  hardcoded table is gone. For meta-bandit capsules `/admin/capsules`
  returns placeholder labels (`option_0`, `option_1`, …) because the
  `.lyc` binary does not preserve symbolic option names; shared-state
  capsules return real labels (`A`, `B`, … from
  `learning.json::sharedState.optionFeatures`).
- **Capsule switcher works.** Dropdown + URL hash drive the active
  capsule per browser tab; no env-var restart required.
- **Kernel-output panel works.** Region 5 reads
  `publishedLatest` / `publishedSeries` from `/api/state`, which
  derives them from the `published` field that the runtime now
  injects into every decision event.
- **Decision-log entries still have no on-disk timestamp.** The
  dashboard records the wall-clock when it first observes each
  decision id and surfaces that as `observedAt`. The relative-time
  display in Region 4 is therefore "first seen by the dashboard,"
  not "the moment the capsule decided." For the demo's polling
  cadence (2s) the skew is bounded; for off-line replays it would
  not be.

## Endpoints

- `GET /` — serves `static/index.html`
- `GET /static/*` — static assets (CSS, JS)
- `GET /api/capsules` — proxies Syntra `/admin/capsules`. Shape:

  ```jsonc
  {
    "capsules": [
      {
        "path": "demo/autoscale/orders",
        "name": "orders",
        "options": ["option_0", "option_1", "option_2"],
        "scoringMode": "meta-bandit"
      }
    ]
  }
  ```

  On error: `{"capsules": [], "error": "..."}` with HTTP 502.

- `GET /api/state?capsule=tenant/job/capsule` — JSON snapshot.
  `?capsule=` is optional; falls back to env defaults. Shape:

  ```jsonc
  {
    "capsulePath": "t/j/smoke",
    "scoringMode": "meta-bandit",         // or "shared-state-linucb"
    "lifecycle": "warmup",                // | active | frozen | unknown
    "warmupProgress": { "collected": 12, "target": 30 },   // null once Active
    "algorithm": null,                    // "ucb" etc. once Active
    "candidates": [
      { "id": "Thompson", "trials": 12.0, "meanReward": 0.53,
        "cumulativeReward": 6.4 }
    ],
    "sharedState": null,                  // or { trials, meanReward }
    "decisionCounts": [
      { "optionIndex": 0, "count": 41 }   // option labels resolved client-side
    ],
    "recentDecisions": [
      { "id": "...", "observedAt": 1731792345123, "optionIndex": 2,
        "refused": false, "refusalReason": null,
        "algorithm": "Ucb", "contextKey": "default",
        "published": {"forecast": 142.7, "p95": 155.0, "marker": "smoke-ok"} }
    ],
    "lastDecision": { /* same shape */ },
    "lastUpdateAt": 1731792345123,
    "totalDecisions": 234,
    "refusedCount": 5,
    "totalMetaRounds": 234,
    "publishedSeries": {
      "forecast": [141.5, 142.7, 144.0, ...],  // oldest-first, ≤60 entries
      "p95":      [155.0, 155.0, 156.0, ...]
    },
    "publishedLatest": { "forecast": 144.0, "p95": 156.0 },
    "serverNow": 1731792347456,
    "errors": []
  }
  ```
