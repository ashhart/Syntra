# LLM routing, on simulated models

`llm_routing.py` routes 4,000 requests to one of three model routes
(`small`, `medium`, `large`) with `syntra.llm.ModelRouter`, lets Syntra
learn which route pays off for which kind of request, then asks
`syntra evaluate --store` what other policies would have earned on the
same logged traffic.

**The models are simulated.** No model is called. Each route has a
made-up price, latency and answer quality per task (`ROUTES` and `TASKS`
in the script), and a simulated grader scores each answer with noise.
The numbers show how Syntra behaves on that simulation, not how any real
model performs. Everything on the Syntra side is real: a v2 server the
script starts on a fresh store, decisions made in-process by
`LocalDecider`, the server's replay check of every uploaded decision,
learning from the rewards, and the off-policy evaluation.

## Run it

```bash
cargo build --release                 # target/release/syntra
sdk/python/scripts/develop.sh         # builds the Python extension
python3 examples/llm-routing/llm_routing.py
```

It takes 10 to 15 seconds. Options: `--requests N`, `--rounds N`,
`--seed S`, and `--keep` to keep the store and print the command to
evaluate it yourself. `SYNTRA_BIN` points at another `syntra` binary.

## What it does

The traffic is half chat, 30% code and 20% summarization, each with a
prompt length drawn from a range. The reward is what an application might
trade: `quality - 5 * cost_usd - 0.04 * latency_s`. With the simulated
profiles that makes `small` best for chat, `large` best for code and
`medium` best for summaries.

For each request the script calls `router.completion(...)`.
`ModelRouter` derives request features from the messages (prompt size,
whether it looks like code), adds the `task` from the context, decides the
route with a `LocalDecider` (in-process, no network hop), calls the
completion function, and measures latency and cost. The quality arrives
later: the simulated grader answers 0 to 200 requests afterwards, through
`router.report_quality(...)`, and only then is the reward sent. Every 400
requests the script uploads the queued decisions and rewards (`flush`) and
fetches the model the server learned (`sync`); in a real service the
decider's background thread does this every second.

## What you should see

One run (seed 7):

```text
round requests  model  expected reward  routed to the best route for the task
    1      400      0            0.677  chat  32%  code  31%  summarize  30%
    2      800    310            0.760  chat  80%  code  91%  summarize  86%
    3     1200    712            0.758  chat  67%  code  88%  summarize  96%
    4     1600   1105            0.746  chat  19%  code  94%  summarize  93%
    5     2000   1499            0.767  chat  82%  code  94%  summarize  93%
   ...
   10     4000   3505            0.769  chat  91%  code  97%  summarize  95%

Uploaded and verified by replay on the server: decisions accepted 4000, decisions rejected 0, rewards applied 4000, rewards failed 0

Expected reward per request on the same traffic (from the simulation's
known profiles; the learner never sees these):
  Syntra, last round        0.769
  Syntra, whole run         0.755
  always small              0.617
  always medium             0.705
  always large              0.714
  uniform random            0.679
  best route per request    0.779
```

The first round is close to uniform (the model starts empty); by the
second, code and summaries mostly go to their best route. Chat wobbles
for a round or two because `small` beats `medium` on chat by only 0.044
in expected reward, so a few noisy grades can flip the model's choice;
the cost of that wobble is small. By the last round the router earns
0.769 per request against 0.714 for the best single route and 0.779 for
a router that knew the simulation's profiles.

Then the evaluation, read from the store while the server runs:

```text
policy           DR estimate     95% interval       lift over logged (DR)   truth
logged                 0.754   [0.751, 0.757]   +0.001 [-0.001, +0.003]   0.755
greedy                 0.754   [0.747, 0.761]   +0.000 [-0.007, +0.007]       -
constant:small         0.618   [0.605, 0.631]   -0.135 [-0.148, -0.123]   0.617
constant:medium        0.705   [0.696, 0.714]   -0.048 [-0.057, -0.041]   0.705
constant:large         0.714   [0.704, 0.724]   -0.040 [-0.049, -0.029]   0.714
```

The doubly robust estimates for the three constant policies land on the
values the simulation knows (`truth`), from logs in which each of those
routes was only one choice among three. The estimate for `greedy` (the
learned policy without exploration) varies from run to run; in five runs it
came out between 0.000 and 0.012 above what was served. By the end almost
everything served was already the learned choice, so the gap is roughly
what exploring costs.

Last comes a gated check with a clear answer. Would sending everything to
the large model have beaten the learned routing? The gates in
[`gates.yaml`](gates.yaml) ask for a positive lift at the 95% level and at
least 200 effective samples; the first fails, so with `--fail-on-gate`
the command exits 1, which is what a CI job or a deploy script would act
on:

```text
**Verdict:** FAIL: 1 of 2 gates fail (lift.dr.lower >= 0: -0.04999 vs 0). DR estimates constant:large at 0.7136 ...
```

## The settings, and why

- The capsule declares no actions: `ModelRouter` sends the model list
  with each decision, so adding a model is a code change.
- `reward.range` is `[-0.5, 1.0]` because the reward subtracts cost and
  latency from quality and can go below zero.
- `learner.learningRate` is 0.1. At the default 0.5, on rewards this noisy
  the learned choice swung between routes from one model to the next, even
  where one route was clearly better (code fell to 5% on its best route
  for a whole round); at 0.1 it holds steady.
- `seed` in the spec makes decisions repeatable, so runs with the same
  `--seed` agree closely.

To route real calls, give `ModelRouter` your completion function (for
example `litellm.completion`), a cost function and a quality signal (a
judge, a user rating or a later outcome through `report_quality`); see
[sdk/python/README.md](../../sdk/python/README.md).
