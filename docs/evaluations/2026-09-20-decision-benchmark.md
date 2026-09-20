# Decision benchmark, 20 September 2026

The embedded decision path meets a 1 ms **p99** target on the real sensor
workload tested here, but the HTTP service does not meet that target consistently
under load, and neither path establishes an absolute deadline.
The strongest sensor policy also misses too many rare classes to treat its high
overall accuracy as operational validation.

This follows the [functional release QA](2026-09-20-release-qa.md), including
the live NASA/JPL constrained launch-window demonstration and the synthetic
pandemic scoring checks.
All numbers below were measured on an Apple M5 Max with 128 GiB memory,
macOS 26.5.2, and a release build; this is a normal shared desktop, without
CPU isolation or a real-time operating system.

## Real sensor decisions

The [UCI Statlog Shuttle dataset](https://archive.ics.uci.edu/dataset/148/statlog+shuttle),
DOI [10.24432/C5WS31](https://doi.org/10.24432/C5WS31), is CC BY 4.0.
The benchmark uses all 43,500 official training examples and all 14,500 official
test examples, with nine numerical inputs and seven possible class actions.
Dataset bytes are pinned by SHA-256 and downloaded to an ignored output folder.
The original sequence was randomized by Statlog, so this is not a replay of a
time-ordered flight or a validation of actuator control.

For each training observation, Syntra selects a class and receives only a binary
reward for that selected action after 32 or 256 further decisions.
The runner uses Syntra's `LinUcbState`, followed by its compiled `AdaptiveChoice`
graph through `GraphExecutor`, with one-hot weights as in the HTTP LinUCB path.
This measures a fixed LinUCB configuration, not the automatic meta-bandit portfolio.
It uses regularization 1, training exploration coefficient 1, and frozen greedy
evaluation with coefficient 0; test labels never update the policy.

Training-only means and standard deviations normalize inputs, clipped at five
standard deviations.
Both feature maps were defined before inspecting results: a bias plus nine inputs,
or those ten features plus all 45 quadratic interactions.
Each configuration runs with seeds 7, 42, and 2026, with a deterministic shuffle
of the training split.
Two separate processes produced identical prediction digests, confusion matrices,
and quality metrics for all twelve configurations, covering 1,392,000 decisions.
Only 14,500 distinct test observations exist; repeated configurations do not create
additional independent test evidence.

### Held-out quality

Balanced accuracy averages the recall of the seven classes equally.
That matters here because 11,478 of the 14,500 test records belong to class 1.

| Policy | Overall accuracy | Balanced accuracy |
| --- | ---: | ---: |
| Training-majority constant action | 79.16% | 14.29% |
| Uniform random action | 14.66% | 11.63% |
| Nearest centroid, full training labels | 78.34% | 71.71% |
| LinUCB, 10 features, 32-decision delay | 94.25–94.63% | 38.46–38.97% |
| LinUCB, 10 features, 256-decision delay | 95.26–95.50% | 39.74–39.94% |
| LinUCB, 55 features, 32-decision delay | 99.28–99.63% | 68.68–79.78% |
| LinUCB, 55 features, 256-decision delay | 99.19–99.32% | 65.76–79.47% |

The nearest-centroid comparison receives every training label, an information
advantage over selected-action feedback; it is a simple reference, not a tuned
state-of-the-art classifier.
The quadratic policy beats the constant-action baseline on overall accuracy
in every seed, but it does not consistently beat the centroid on balanced accuracy.
Class 2 has only 13 test examples and quadratic-policy recall of 7.7–23.1%.
Class 6 has only four test examples and recall ranging from 0–100%.
Those tiny samples are insufficient for a dependable rare-event claim.

### Embedded latency

The timer includes normalization, feature expansion, all seven arm scores,
graph cloning, execution-context creation, and graph execution.
It excludes data-file parsing, one-time compilation/verification, delayed
learning updates, HTTP, serialization, logging, and durable persistence.

| Feature map | Training decision p99, across six runs | Held-out decision p99, across six runs |
| --- | ---: | ---: |
| 10 features | 1.38–8.79 µs | 1.21–11.79 µs |
| 55 features | 18.92–39.46 µs | 14.25–23.71 µs |

All twelve configurations meet p99 below 1 ms, but **3 of the 696,000 timed
decisions exceeded 1 ms**, with a maximum of 11.38 ms.
A low percentile is not a worst-case execution-time guarantee.

The existing whole-capsule benchmark also shows why workload boundaries matter:
anomaly routing measured 9.2 µs p99, the adaptive-router demo 199.8 µs p99, and
the chaos-control demo 123.48 ms p99 while recomputing its numerical experiment.
Moving expensive scientific search into a preparation phase can support a fast
subsequent action choice, but those full searches are not sub-millisecond work.

## Authenticated HTTP decisions under load

The HTTP runner installs a capsule that computes a mean from five inputs and
chooses one of three routes, using the normal filesystem-backed runtime.
Every final-run response must contain the correct computed mean, a valid selected
route, a decision ID, and an unrefused decision; feedback must return `ok: true`.
The rate limit is explicitly raised to 100,000 requests/second for this capacity
measurement, without disabling authentication, policy checks, or persistence.
The normal quota remains a separate operational limit.

Persistent loopback connections run from Python client threads.
Open-loop scenarios schedule arrivals independently of completion and count waiting
for a client worker in arrival-to-response latency, so queues are not hidden.
Client scheduling, Python overhead, operating-system pauses, and server work all
affect this end-to-end measurement.
The mixed scenario keeps eight workers busy and submits feedback after every tenth
decision; feedback competes with decisions in the same tenant/job/capsule.

| Final scenario | Decisions | Errors | Request p99 | Arrival-to-response p99 | Max response |
| --- | ---: | ---: | ---: | ---: | ---: |
| sequential | 2,000 | 0 | 0.454 ms | 0.454 ms | 1.254 ms |
| 8-workers-10pct-feedback | 4,000 | 0 | 36.378 ms | 36.378 ms | 128.325 ms |
| open-loop-100rps | 500 | 0 | 30.418 ms | 32.465 ms | 60.186 ms |
| open-loop-500rps | 2,500 | 0 | 7.273 ms | 12.997 ms | 54.124 ms |
| open-loop-1000rps | 5,000 | 0 | 1.236 ms | 1.520 ms | 2.828 ms |
| open-loop-2000rps | 10,000 | 0 | 1.325 ms | 2.713 ms | 26.582 ms |

The first cold HTTP decision took 5.299 ms.
The final run returned valid results for all 24,000 measured decisions and 400
feedback requests, but failed the explicit 1 ms p99 gate with exit status 2.
Only the sequential scenario met that target in the final run.

Three earlier full runs are retained in the JSON receipt, rather than discarding
unfavorable repetitions.
Their mixed-feedback p99 ranged from 25.2 to 54.8 ms, and one mixed request
timed out after approximately 30 seconds.
The earlier runner did not distinguish a decision timeout from a feedback timeout,
so that failure cannot be attributed to one operation from the retained evidence.
The final runner records that distinction; the timeout did not recur in its run.
Open-loop results varied widely, reaching 1,413 ms p99 at 2,000 offered requests
per second in one run, which rules out a stable sub-millisecond service claim.

These measurements establish a performance gap, not its root cause.
The code currently serializes mutations within a capsule and performs filesystem
work in that path; feedback lookup also reads retained decision logs.
Profiling these paths under a fixed workload is the next optimization step.
This change does not bypass logging, weaken policy checks, or claim to have solved
the service latency gap.

## A misleading measurement fixed

The previous `bench-decide.sh` keepalive client treated every HTTP response as a
successful request.
With the token rate limited to one request per second and the default 2,000-request
burst allowance, a reproduction reported
2,538 successes and zero errors while the server counted only 2,000 decisions.
The corrected runner counted 2,000 successes and 212 errors in its reproduction
and exited nonzero; differences in total attempts reflect separate timed runs.
The new HTTP runner additionally checks response contents and keeps rejected
requests and failures visible in the report.

## Restart regression found during the full suite

The full rerun caught a separate flaky governor demo assertion: persisted weights
strongly favored holding a risky action, but exploration selected an allow action
on the first request after restart, causing a false persistence failure.
The revised demo compares the complete learned memory before shutdown and
immediately after restart, requires identical state and a hold-favoring learned
policy, and then checks valid actions while keeping exploration enabled.
The existing steady-state trust and structural budget-rail checks remain in place.
The regression test also requires the explicit state-equality proof in its output.

A concurrent run also exposed a timing-dependent crash-recovery setup: only 47
decisions completed before the kill timers, below its required 50.
The test now waits for at least 20 successful decisions per cycle before applying
the same randomized kill offset, with a bounded startup wait.
It retains the minimum-work and all durability assertions; it does not lower
the required decision count or relax the recovery checks.

## Reproduce

```bash
python3 scripts/bench-shuttle.py
python3 scripts/bench-decision-http.py --require-p99-ms 1
cargo run --locked --release --example rt_baseline > /dev/null
```

The first command needs network access for the pinned public dataset and `gzip`
to decode the original Unix-compress training file.
Subsequent runs reuse the checked download.
The HTTP gate intentionally fails when any measured scenario misses the target;
omit `--require-p99-ms` to collect results without enforcing a latency SLO.
Do not make shared CI wall-clock timing a correctness test.

Machine-readable scores, class confusion matrices, timing distributions, dataset
hashes, and all four HTTP runs are in the [JSON receipt](2026-09-20-decision-benchmark.json).
The source dataset and private execution logs are excluded from Git.
