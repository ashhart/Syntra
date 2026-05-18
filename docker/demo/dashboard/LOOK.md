# Syntra dashboard — visual reference

Written description of the dashboard at four lifecycle states. Use this
in place of screenshots when reviewing a remote change or evaluating
what the user will see without standing up the container.

Page background is near-black (#0a0a0a). All surfaces are dark grey
(#141414) on a 1px border (#262626). One screen, no scroll at 1440x900.

## Layout (constant across states)

A single 1600px-max column with 24px gutters:

```
+-----------------------------------------------------------+   <- top edge
| (1px poll-bar — visible only mid-poll, otherwise empty)   |
+-----------------------------------------------------------+
| HEADER (80px)                                             |
|  Syntra | demo/retry/router    capsule [router · ▼] [PILL]
|                                     decisions: N · refused: M · last update: Xs ago
+-----------------------------------------------------------+
|                                                           |
|  REWARD OVER TIME (large card, flex height)              |
|   Mean reward over time   last 5 minutes · per candidate |
|     1.00 ─────────────────────────────────────────────    |
|     0.50 - - - - - - - - - - - - - - - - - - - - - - -    |
|     0.00 ─────────────────────────────────────────────    |
|          -5m  -4:30m  -4m  …                 now          |
|   ● Thompson · 240 trials   ● Ucb · 240 trials   …       |
+-----------------------------------------------------------+
| LIVE KERNEL OUTPUTS (120px tall, full width grid)        |
|   forecast      | p95            | recommended_instances |
|   144.0         | 156.0          | 6                     |
|   ╱╲╱╲___╱╲     | ___╱──╲___╱──  | ──╱──╱──              |
+-----------------------------------------------------------+
| DISTRIBUTION (left half)        | RECENT DECISIONS (right) |
|  Choice distribution            |  Recent decisions        |
|   last 1000 decisions           |   newest first           |
|                                 |                          |
|  none    ████████████  482      |  3s   ●  triple via Ucb  |
|  triple  █████          178     |  5s   ●  none via Ucb    |
|  single  ███             95     |  7s   ●  REFUSED ood     |
|                                 |  9s   ●  single via Ucb  |
+-----------------------------------------------------------+
```

## State 1 — cold start

(Capsule just installed; zero decisions; warmup target is 30.)

- Header pill: amber background (low-opacity `#f59e0b`), text in dark
  amber. Reads `WARMUP   0 / 30`. The header meta line below reads
  `decisions: 0 · refused: 0 · last update: —`.
- Region 2 (chart): axes are drawn (Y ticks 0.00, 0.25, 0.50, 0.75,
  1.00; X ticks `-5m`, `-4:30m`, …, `now`; horizontal dashed-equivalent
  gridline at 0.50). No line paths yet. Legend is empty.
- Region 3 (distribution): empty card body shows the muted tertiary
  caption "No decisions yet — capsule is in warmup." in 12px text.
- Region 4 (feed): empty list shows "Waiting for the first decision."
  in tertiary text.
- Top of page: poll-bar pulse appears as a faint cyan sweep across the
  topmost 1px line each time a poll is in flight, then disappears.

## State 2 — mid-warmup

(About 15 of 30 baseline samples collected. Meta-bandit hasn't picked
a winner yet so candidates explore uniformly.)

- Header pill: same amber, now reads `WARMUP   15 / 30`. Meta line
  reads `decisions: 15 · refused: 0 · last update: 2s ago`.
- Region 2: short, near-flat lines at roughly y=0.5 for each candidate
  that has accumulated trials. The seven candidate names appear in the
  legend with stable colours (Thompson teal, Ucb lavender, EpsilonGreedy
  amber, Weighted rose, Greedy slate, LinUcb lime, LinTs sky). Trial
  counts are visible after each label like `· 3 trials`. No line is yet
  marked "leading" because the rewards are still indistinguishable;
  whichever happens to be momentarily highest renders thicker.
- Region 3: a few bars filling the card. The most-chosen option's bar
  is cyan accent; the others are muted slate. Counts on the right in
  monospace.
- Region 4: 15-ish rows. Each row is `Xs ago  ●  option_name  via Algo`,
  newest at top, with a 6px coloured dot matching the option's bar
  colour (cyan for the leader, slate otherwise). Faint dashed border
  between rows.

## State 3 — active, ~5 minutes in

(Warmup completed; algorithm picked; meta-bandit has had ~150 rounds
to differentiate candidates.)

- Header pill: cyan background (low-opacity `#22d3ee`), text in light
  cyan. Reads `ACTIVE   algorithm: ucb`. Meta line reads
  `decisions: 147 · refused: 2 · last update: 1s ago`.
- Region 2: seven lines now visibly diverge. UCB sits highest at
  ~0.78 mean reward, Thompson trails at ~0.72, the rest spread out
  between ~0.45 and ~0.65. The UCB path renders at 2.25px stroke
  (the "leading" treatment), the rest at 1.5px. Lines occupy the
  rightmost half of the chart densely; the leftmost section shows
  the older, flatter, exploration-era values. Legend trial counts
  read like `Thompson · 47 trials`, `Ucb · 51 trials`, etc.
- Region 3: clear winner — `none` (or whichever option won) has a long
  cyan bar at maybe 480 of the 1000 most recent decisions; the other
  retries trail in muted slate bars descending by length.
- Region 4: 20 rows. Eighteen of them show the normal `Xs ago  ●  name`
  pattern; two refused rows stand out with a red `●` dot, the text
  `REFUSED` in alert red, and the tertiary caption shows the reason
  (`ood` or `interval_too_wide`).
- New entries fade in from the top over 200ms; the older entries do
  not jump.

## State 4 — Phase 2, fully populated

(Three capsules installed; the predictive-autoscaler is selected; it
has been running long enough to fill its sparklines.)

- Header: wordmark, separator, `demo/autoscale/orders` capsule path,
  then a small uppercase tertiary label `CAPSULE` followed by a dark
  monospace dropdown reading `orders  ·  demo/autoscale/orders ▼`.
  The dropdown lists three options — `orders`, `router`, and
  `embeddings` — sorted by path. Right of the dropdown is the cyan
  `ACTIVE  algorithm: ucb` pill. Below: standard meta line.
- Region 2 (reward chart): seven candidate lines, UCB leading at
  ~0.78, others diverged. Same shape as State 3.
- Region 5 (kernel outputs): three accent-bottom-bordered cards in a
  responsive grid. Card 1 reads `forecast` (12px secondary) then
  `144.0` (18px monospace) then a cyan sparkline sweeping up-and-right
  across a 24px-tall band. Card 2 reads `p95`, value `156.0`, a flatter
  sparkline. Card 3 reads `recommended_instances`, value `6`, a step
  sparkline. The cards autoscale to span the full row.
- Region 3 (distribution): rows now have human-friendly option labels
  like `forecast_headroom`, `proactive_scale`, `hold`, `react`, with
  the leader (`forecast_headroom`) in cyan. (Note: for capsules compiled
  from `.lyc` binaries that don't preserve labels, the rows fall back
  to `option_0`, `option_1`, etc. — this is the placeholder behaviour
  noted in the README.)
- Region 4 (feed): rows like `3s  ●  forecast_headroom via Ucb`.
- Switching the dropdown to `embeddings` (the shared-state-action-
  embeddings capsule): Region 2 collapses to a single
  `SharedStateLinUcb` line, Region 5 displays a single muted dashed-
  bordered placeholder card spanning the full width with the message
  "This capsule does not publish kernel outputs. Call (!cap
  \"runtime.publish\" \"<name>\" <value>) in your .lycs program …",
  Region 3 shows `A`, `B`, `C`, `D`, `E`, `F` as the option labels.

## Mode switch — shared-state LinUCB

If the dashboard is pointed at a capsule whose `learning.json` has
`sharedState.enabled = true` (e.g. the `shared-state-action-embeddings`
example), Region 2 collapses to a single line labelled
`SharedStateLinUcb` in the accent cyan, the chart subtitle reads
"last 5 minutes · shared-state LinUCB", and the legend has a single
entry. Region 1, 3 and 4 are unchanged in shape — only the number of
lines in the chart differs.

## Hover / interaction notes

- Legend entries are clickable. Clicking dims that entry and hides its
  line (CSS `display: none` on the path). Click again to bring it back.
- The poll-bar is the only loading indicator; no spinners.
- Numbers transition with a brief 200ms fade rather than swapping
  instantly. Rows in the distribution and feed reorder smoothly when
  the order changes (the underlying nodes are reused).
