# Decision latency from Python: Syntra and Vowpal Wabbit

200,000 timed calls per variant after 20,000 warm-up calls, one thread, microseconds per call (time.perf_counter_ns around each call; includes the loop and timer, see the last row). Both models learned from the same 2,000 rounds of the `segments` environment first (Syntra model version 2000). Apple M5 Max (macOS), release build.

| Call | p50 | p90 | p99 | p99.9 | mean |
|---|---:|---:|---:|---:|---:|
| Syntra `LocalDecider.decide(dict)`, background sync every 1 s (default) | 0.83 | 0.96 | 2.04 | 4.67 | 0.90 |
| Syntra `LocalDecider.decide(dict)`, no background thread | 0.83 | 0.96 | 2.38 | 6.04 | 0.91 |
| VW `predict(lines)`, text built in advance | 8.00 | 8.21 | 11.75 | 23.58 | 8.17 |
| VW `predict(lines)` + drawing the action in Python | 8.25 | 8.46 | 11.25 | 22.79 | 8.35 |
| VW: build the text from the dict, then `predict` | 9.04 | 9.29 | 12.21 | 24.08 | 9.16 |
| VW `predict` on a pre-parsed example (no parsing; lower bound) | 2.42 | 2.54 | 3.17 | 4.00 | 2.44 |
| Empty call (timing-loop overhead) | 0.04 | 0.04 | 0.04 | 0.08 | 0.04 |

Every local decision was uploaded and replayed by the server: 440,000 accepted and 0 rejected of 440,000 made (both deciders, warm-up included).

System load average (1, 5, 15 min) before the timed loops 13.2, 21.2, 25.2, after 12.3, 20.8, 25.0 (18 logical CPUs). Wall time 10 s.
