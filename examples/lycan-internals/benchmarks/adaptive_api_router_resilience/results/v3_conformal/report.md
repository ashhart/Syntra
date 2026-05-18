# Adaptive API Router Resilience Benchmark

**Date:** 2026-05-15 23:00 UTC
**Seeds:** 30
**Requests per seed:** 10000
**Total decisions evaluated:** 2,700,000

## Benchmark Setup

Tests whether Syntra (contextual bandit, epsilon-greedy) can outperform
static and conventional adaptive routing baselines when provider conditions
change unpredictably across 6 regime phases.

**Syntra runtime:** Live Docker instance on localhost:8787
**Syntra algorithm:** epsilon-greedy (epsilon=0.10)
**Capsule:** 5-provider AdaptiveChoice node (router_5provider.lyc)
**Reward function:** Explicit multi-signal composite (success, latency, p99,
error, timeout, queue, SLA, cost, instability, recovery, graceful degradation)

## Policies Tested

| Policy | Type | Description |
|--------|------|-------------|
| syntra | Adaptive (bandit) | Live Syntra with epsilon-greedy learning |
| round_robin | Static | Cycles through providers sequentially |
| random | Static | Uniform random selection |
| lowest_current_latency | Reactive | Picks lowest observed latency |
| lowest_error_rate | Reactive | Picks lowest observed error rate |
| ewma_latency | Adaptive | Exponentially weighted moving average |
| circuit_breaker | Adaptive | Opens circuit after N failures |
| weighted_static | Static | Fixed weights (40/25/15/12/8) |
| oracle | Perfect | Knows true provider state (regret baseline) |

## Regime Schedule

| Phase | Requests | Description |
|-------|----------|-------------|
| 1. Normal | 0-2000 | provider_a fastest/cheapest |
| 2. Degradation | 2000-3500 | provider_a latency creeps up, queue grows |
| 3. Attack | 3500-5000 | provider_a fails, provider_d queue amplification |
| 4. Telemetry | 5000-6500 | 20-40% corrupted metrics |
| 5. Recovery | 6500-8200 | provider_a partial recovery, false recovery trap |
| 6. Novel | 8200-10000 | provider_e optimal under high traffic |

## Scoring Function

```
reward =
  +1.0 * success
  -0.3 * min(1, latency_ms / 1000)
  -0.4 * min(1, max(0, latency_ms - 500) / 2000)   [p99 penalty]
  -0.5 * error
  -0.8 * timeout
  -0.2 * min(0.3, queue_depth / 5000)
  -0.3 * sla_violated
  -min(0.2, cost * 10)
  -0.5 * instability
  +0.2 * recovery_bonus
  +0.1 * graceful_degradation_bonus
```

Resilience score: composite of success rate (30), latency (15),
p99 (15), SLA violations (15), cost (10), instability (10), queue collapse (5)

## Aggregate Results (mean across seeds)

| Policy | Mean Lat | p95 Lat | p99 Lat | Success | Error | SLA Viol | Cost | Instab | Resilience |
|--------|----------|---------|---------|---------|-------|----------|------|--------|------------|
| syntra ** | 126.8ms | 145.0ms | 1699.0ms | 98.29% | 1.10% | 2.34% | $123.43 | 141 | 5.19 |
| round_robin | 263.1ms | 742.0ms | 5144.1ms | 95.01% | 2.99% | 7.56% | $125.91 | 600 | -2.42 |
| random | 263.3ms | 740.0ms | 5153.5ms | 94.98% | 3.02% | 7.56% | $125.98 | 597 | -2.43 |
| lowest_current_latency | 150.1ms | 180.1ms | 4781.4ms | 97.53% | 1.19% | 3.52% | $111.85 | 297 | 3.34 |
| lowest_error_rate | 88.3ms | 128.9ms | 206.7ms | 99.56% | 0.29% | 0.45% | $181.46 | 7 | 16.01 |
| ewma_latency | 88.9ms | 134.2ms | 202.0ms | 99.25% | 0.60% | 0.82% | $139.69 | 2 | 17.98 |
| circuit_breaker | 97.6ms | 108.5ms | 1147.5ms | 98.58% | 0.91% | 2.00% | $113.68 | 73 | 13.63 |
| weighted_static | 408.5ms | 3048.2ms | 5397.3ms | 91.54% | 4.90% | 13.28% | $112.15 | 1208 | -8.60 |
| oracle | 70.1ms | 103.7ms | 169.2ms | 99.41% | 0.46% | 0.60% | $127.95 | 0 | 19.54 |

## Per-Phase: Syntra vs Oracle

| Phase | Syntra Resilience | Oracle Resilience | Gap |
|-------|-------------------|-------------------|-----|
| phase1_normal | 20.21 | 23.35 | -3.14 |
| phase2_degradation | 18.21 | 20.04 | -1.82 |
| phase3_attack | 1.25 | 19.47 | -18.22 |
| phase4_telemetry | -0.67 | 16.66 | -17.34 |
| phase5_recovery | 12.47 | 18.21 | -5.74 |
| phase6_novel | 17.74 | 18.95 | -1.21 |

## Syntra vs Baselines (seed-by-seed win rate)

- **vs round_robin:** Syntra wins 30/30 (100%)
- **vs random:** Syntra wins 30/30 (100%)
- **vs lowest_current_latency:** Syntra wins 26/30 (87%)
- **vs lowest_error_rate:** Syntra wins 0/30 (0%)
- **vs ewma_latency:** Syntra wins 0/30 (0%)
- **vs circuit_breaker:** Syntra wins 4/30 (13%)
- **vs weighted_static:** Syntra wins 30/30 (100%)

## Oracle Regret

- **Mean final cumulative regret:** 396.2
- **Mean midpoint cumulative regret:** 242.9
- **Regret per request (final):** 0.0396

## Conformal Prediction Sets

Calibration of the conformal sets emitted on `/decide`. Three reports.

- **Posterior-mean coverage (chosen-action):** 87.9% (nominal 90%)
- **Oracle containment:** 68.6% (quality, not calibration)

### Per-phase calibration and set width

| Phase | Mean width | P90 width | Mean band radius | Posterior-mean coverage | Oracle containment |
|-------|-----------:|----------:|------------------:|------------------------:|-------------------:|
| phase1_normal | 1.47 | 2.9 | 0.034 | 85.8% | 66.3% |
| phase2_degradation | 1.44 | 2.7 | 0.040 | 87.2% | 69.8% |
| phase3_attack | 1.56 | 2.7 | 0.068 | 87.9% | 87.8% |
| phase4_telemetry | 2.07 | 3.6 | 0.120 | 88.9% | 54.5% |
| phase5_recovery | 2.06 | 3.4 | 0.120 | 88.9% | 66.7% |
| phase6_novel | 2.08 | 3.5 | 0.089 | 89.1% | 67.8% |

Calibration is computed against the chosen-action residual distribution.
Conformal sets are not weighted-conformal (Tibshirani et al. 2019), so the
guarantee is for the chosen action's reward, not independently for each
in-set option. The conformity buffer is sliding-window only (no
change-triggered flush); coverage drops at phase boundaries are expected
under exchangeability violation — see Gibbs & Candès 2021 (ACI) for the
principled fix.

## Surrogate-Index OPE (counterfactual)

Athey et al. (2019) surrogate-index estimator. Uses immediate latency as
a surrogate for resilience score to estimate what Syntra's score would
be if it had imitated each baseline. Same trace, no re-simulation.

| Baseline | Mean observed score | Counterfactual Syntra-imitating-baseline score |
|----------|---------------------|-------------------------------------------------|
| circuit_breaker | 13.63 | 7.23 |
| ewma_latency | 17.98 | 7.99 |
| lowest_current_latency | 3.34 | 5.37 |
| lowest_error_rate | 16.01 | 7.60 |
| oracle | 19.54 | 11.06 |
| random | -2.43 | 7.19 |
| round_robin | -2.42 | 7.19 |
| weighted_static | -8.60 | 9.97 |

## Pass/Fail Criteria

- **c1_beats_weak_baselines:** PASS (100.0% vs >=80%)
  - Syntra beat round_robin+random+weighted_static in 30/30 seeds
- **c2_beats_adaptive_baselines:** FAIL (0.0% vs >=65%)
  - Syntra beat lowest_latency+lowest_error in 0/30 seeds
- **c3_p99_vs_ewma_attack:** FAIL (0.0% vs >=60%)
  - Syntra p99 < EWMA p99 in attack phase in 0/30 seeds
- **c4_queue_collapse_vs_cb:** PASS (100.0% vs >=60%)
  - Syntra queue_collapse <= circuit_breaker in 30/30 seeds
- **c5_regret_decreasing:** PASS (100.0% vs >=50% (regret rate decreases post-shift))
  - Regret rate decreased after regime shift in 30/30 seeds
- **c6_no_provider_lock:** PASS (100.0% vs >=80%)
  - Syntra did not lock onto provider_a in late phases in 30/30 seeds
- **c7_telemetry_recovery:** PASS (100.0% vs >=60%)
  - Syntra recovered from telemetry corruption in 30/30 seeds
- **c8_audit_adaptation:** PASS (100.0% vs >=80%)
  - Weight evolution shows meaningful adaptation in 30/30 seeds

### Overall Verdict: **FAIL** (6/8 criteria passed)

## Notable Failures

- Seed 1000: worst phase = phase4_telemetry (resilience=-0.99)
- Seed 1001: worst phase = phase4_telemetry (resilience=-0.97)
- Seed 1002: worst phase = phase3_attack (resilience=-1.09)
- Seed 1003: worst phase = phase4_telemetry (resilience=-0.62)
- Seed 1004: worst phase = phase4_telemetry (resilience=-1.06)

## Cases Where Baselines Beat Syntra

- Seed 1000: lowest_error_rate (15.90) > syntra (3.53)
- Seed 1001: lowest_error_rate (16.12) > syntra (4.28)
- Seed 1002: lowest_error_rate (15.73) > syntra (4.19)
- Seed 1003: lowest_error_rate (16.03) > syntra (10.59)
- Seed 1004: lowest_error_rate (15.91) > syntra (3.90)
- Seed 1005: lowest_error_rate (15.78) > syntra (7.54)
- Seed 1006: lowest_error_rate (16.18) > syntra (12.95)
- Seed 1007: lowest_current_latency (3.71) > syntra (3.69)
- Seed 1008: lowest_error_rate (16.17) > syntra (4.71)
- Seed 1009: lowest_current_latency (3.24) > syntra (3.10)
- Seed 1010: lowest_error_rate (16.14) > syntra (11.35)
- Seed 1011: lowest_error_rate (16.08) > syntra (5.19)
- Seed 1012: lowest_error_rate (15.87) > syntra (4.16)
- Seed 1013: lowest_error_rate (16.15) > syntra (4.40)
- Seed 1014: lowest_error_rate (16.01) > syntra (4.19)
- Seed 1015: lowest_current_latency (3.74) > syntra (3.20)
- Seed 1016: lowest_error_rate (15.94) > syntra (4.63)
- Seed 1017: lowest_error_rate (15.95) > syntra (10.24)
- Seed 1018: lowest_error_rate (16.04) > syntra (4.55)
- Seed 1019: lowest_error_rate (15.55) > syntra (3.71)
- Seed 1020: lowest_error_rate (15.80) > syntra (6.36)
- Seed 1021: lowest_error_rate (16.23) > syntra (4.22)
- Seed 1022: lowest_error_rate (16.15) > syntra (4.96)
- Seed 1023: lowest_error_rate (16.20) > syntra (3.91)
- Seed 1024: lowest_error_rate (15.91) > syntra (4.11)
- Seed 1025: lowest_current_latency (3.21) > syntra (3.18)
- Seed 1026: lowest_error_rate (16.14) > syntra (4.19)
- Seed 1027: lowest_error_rate (15.84) > syntra (3.51)
- Seed 1028: lowest_error_rate (16.05) > syntra (3.65)
- Seed 1029: lowest_error_rate (16.24) > syntra (3.60)

## Syntra Weight Evolution (first seed)

| Request | Phase | provider_a | provider_b | provider_c | provider_d | provider_e |
|---------|-------|------------|------------|------------|------------|------------|
| 0 | phase1_normal | 0.200 | 0.200 | 0.200 | 0.200 | 0.200 |
| 100 | phase1_normal | 0.154 | 0.400 | 0.117 | 0.214 | 0.116 |
| 200 | phase1_normal | 0.115 | 0.533 | 0.109 | 0.115 | 0.128 |
| 300 | phase1_normal | 0.110 | 0.556 | 0.109 | 0.115 | 0.110 |
| 400 | phase1_normal | 0.272 | 0.356 | 0.121 | 0.097 | 0.154 |
| 500 | phase1_normal | 0.140 | 0.483 | 0.114 | 0.140 | 0.122 |
| 600 | phase1_normal | 0.119 | 0.140 | 0.503 | 0.118 | 0.120 |
| 700 | phase1_normal | 0.129 | 0.119 | 0.498 | 0.122 | 0.131 |
| 800 | phase1_normal | 0.139 | 0.127 | 0.469 | 0.123 | 0.143 |
| 900 | phase1_normal | 0.122 | 0.129 | 0.496 | 0.124 | 0.128 |
| 1000 | phase1_normal | 0.158 | 0.116 | 0.481 | 0.119 | 0.126 |
| 1100 | phase1_normal | 0.130 | 0.135 | 0.473 | 0.134 | 0.128 |
| 1200 | phase1_normal | 0.181 | 0.131 | 0.449 | 0.120 | 0.119 |
| 1300 | phase1_normal | 0.406 | 0.154 | 0.205 | 0.112 | 0.122 |
| 1400 | phase1_normal | 0.538 | 0.120 | 0.127 | 0.110 | 0.105 |
| 1500 | phase1_normal | 0.554 | 0.121 | 0.109 | 0.111 | 0.105 |
| 1600 | phase1_normal | 0.545 | 0.106 | 0.128 | 0.112 | 0.109 |
| 1700 | phase1_normal | 0.140 | 0.117 | 0.495 | 0.128 | 0.121 |
| 1800 | phase1_normal | 0.139 | 0.130 | 0.484 | 0.118 | 0.129 |
| 1900 | phase1_normal | 0.354 | 0.089 | 0.353 | 0.114 | 0.090 |
| 2000 | phase2_degradation | 0.474 | 0.123 | 0.133 | 0.135 | 0.135 |
| 2100 | phase2_degradation | 0.372 | 0.372 | 0.092 | 0.081 | 0.082 |
| 2200 | phase2_degradation | 0.370 | 0.095 | 0.082 | 0.082 | 0.371 |
| 2300 | phase2_degradation | 0.370 | 0.092 | 0.084 | 0.085 | 0.370 |
| 2400 | phase2_degradation | 0.372 | 0.372 | 0.093 | 0.081 | 0.083 |
| 2500 | phase2_degradation | 0.494 | 0.139 | 0.123 | 0.115 | 0.130 |
| 2600 | phase2_degradation | 0.125 | 0.117 | 0.518 | 0.122 | 0.117 |
| 2700 | phase2_degradation | 0.405 | 0.214 | 0.147 | 0.116 | 0.119 |
| 2800 | phase2_degradation | 0.113 | 0.539 | 0.122 | 0.116 | 0.110 |
| 2900 | phase2_degradation | 0.126 | 0.528 | 0.118 | 0.118 | 0.110 |
| 3000 | phase2_degradation | 0.109 | 0.561 | 0.111 | 0.110 | 0.109 |
| 3100 | phase2_degradation | 0.119 | 0.540 | 0.115 | 0.115 | 0.111 |
| 3200 | phase2_degradation | 0.110 | 0.558 | 0.113 | 0.110 | 0.110 |
| 3300 | phase2_degradation | 0.110 | 0.542 | 0.121 | 0.116 | 0.111 |
| 3400 | phase2_degradation | 0.357 | 0.357 | 0.084 | 0.116 | 0.086 |
| 3500 | phase3_attack | 0.091 | 0.360 | 0.094 | 0.361 | 0.093 |
| 3600 | phase3_attack | 0.110 | 0.541 | 0.110 | 0.126 | 0.113 |
| 3700 | phase3_attack | 0.110 | 0.530 | 0.111 | 0.121 | 0.127 |
| 3800 | phase3_attack | 0.114 | 0.449 | 0.170 | 0.149 | 0.119 |
| 3900 | phase3_attack | 0.109 | 0.529 | 0.111 | 0.137 | 0.114 |
| 4000 | phase3_attack | 0.109 | 0.551 | 0.116 | 0.113 | 0.111 |
| 4100 | phase3_attack | 0.083 | 0.525 | 0.125 | 0.140 | 0.127 |
| 4200 | phase3_attack | 0.108 | 0.494 | 0.142 | 0.126 | 0.130 |
| 4300 | phase3_attack | 0.071 | 0.376 | 0.083 | 0.376 | 0.094 |
| 4400 | phase3_attack | 0.089 | 0.542 | 0.124 | 0.124 | 0.122 |
| 4500 | phase3_attack | 0.106 | 0.471 | 0.126 | 0.148 | 0.149 |
| 4600 | phase3_attack | 0.059 | 0.365 | 0.104 | 0.106 | 0.366 |
| 4700 | phase3_attack | 0.065 | 0.461 | 0.161 | 0.144 | 0.170 |
| 4800 | phase3_attack | 0.040 | 0.457 | 0.191 | 0.167 | 0.145 |
| 4900 | phase3_attack | 0.110 | 0.498 | 0.134 | 0.140 | 0.118 |
| 5000 | phase4_telemetry | 0.057 | 0.366 | 0.109 | 0.367 | 0.101 |
| 5100 | phase4_telemetry | 0.108 | 0.471 | 0.130 | 0.132 | 0.160 |
| 5200 | phase4_telemetry | 0.059 | 0.472 | 0.140 | 0.137 | 0.192 |
| 5300 | phase4_telemetry | 0.104 | 0.484 | 0.124 | 0.170 | 0.118 |
| 5400 | phase4_telemetry | 0.056 | 0.367 | 0.104 | 0.368 | 0.105 |
| 5500 | phase4_telemetry | 0.092 | 0.511 | 0.139 | 0.133 | 0.126 |
| 5600 | phase4_telemetry | 0.090 | 0.404 | 0.161 | 0.176 | 0.170 |
| 5700 | phase4_telemetry | 0.076 | 0.492 | 0.133 | 0.131 | 0.168 |
| 5800 | phase4_telemetry | 0.110 | 0.463 | 0.154 | 0.145 | 0.129 |
| 5900 | phase4_telemetry | 0.121 | 0.431 | 0.154 | 0.171 | 0.123 |
| 6000 | phase4_telemetry | 0.053 | 0.255 | 0.143 | 0.349 | 0.200 |
| 6100 | phase4_telemetry | 0.093 | 0.347 | 0.097 | 0.348 | 0.116 |
| 6200 | phase4_telemetry | 0.078 | 0.478 | 0.128 | 0.166 | 0.150 |
| 6300 | phase4_telemetry | 0.309 | 0.309 | 0.145 | 0.114 | 0.122 |
| 6400 | phase4_telemetry | 0.103 | 0.489 | 0.132 | 0.146 | 0.130 |
| 6500 | phase5_recovery | 0.096 | 0.374 | 0.191 | 0.169 | 0.170 |
| 6600 | phase5_recovery | 0.142 | 0.460 | 0.125 | 0.128 | 0.145 |
| 6700 | phase5_recovery | 0.125 | 0.289 | 0.185 | 0.290 | 0.112 |
| 6800 | phase5_recovery | 0.108 | 0.463 | 0.153 | 0.136 | 0.141 |
| 6900 | phase5_recovery | 0.134 | 0.483 | 0.157 | 0.122 | 0.103 |
| 7000 | phase5_recovery | 0.149 | 0.460 | 0.139 | 0.138 | 0.114 |
| 7100 | phase5_recovery | 0.162 | 0.396 | 0.140 | 0.134 | 0.168 |
| 7200 | phase5_recovery | 0.119 | 0.447 | 0.138 | 0.137 | 0.159 |
| 7300 | phase5_recovery | 0.103 | 0.326 | 0.327 | 0.141 | 0.104 |
| 7400 | phase5_recovery | 0.145 | 0.505 | 0.112 | 0.117 | 0.120 |
| 7500 | phase5_recovery | 0.136 | 0.452 | 0.140 | 0.131 | 0.141 |
| 7600 | phase5_recovery | 0.152 | 0.459 | 0.112 | 0.145 | 0.131 |
| 7700 | phase5_recovery | 0.104 | 0.437 | 0.158 | 0.145 | 0.156 |
| 7800 | phase5_recovery | 0.111 | 0.479 | 0.142 | 0.136 | 0.132 |
| 7900 | phase5_recovery | 0.137 | 0.490 | 0.115 | 0.143 | 0.115 |
| 8000 | phase5_recovery | 0.102 | 0.460 | 0.156 | 0.147 | 0.135 |
| 8100 | phase5_recovery | 0.139 | 0.424 | 0.127 | 0.148 | 0.162 |
| 8200 | phase6_novel | 0.146 | 0.132 | 0.414 | 0.182 | 0.127 |
| 8300 | phase6_novel | 0.159 | 0.305 | 0.304 | 0.098 | 0.134 |
| 8400 | phase6_novel | 0.161 | 0.119 | 0.271 | 0.140 | 0.309 |
| 8500 | phase6_novel | 0.104 | 0.331 | 0.332 | 0.111 | 0.122 |
| 8600 | phase6_novel | 0.141 | 0.422 | 0.166 | 0.138 | 0.134 |
| 8700 | phase6_novel | 0.115 | 0.449 | 0.137 | 0.161 | 0.137 |
| 8800 | phase6_novel | 0.135 | 0.469 | 0.121 | 0.112 | 0.163 |
| 8900 | phase6_novel | 0.163 | 0.412 | 0.138 | 0.152 | 0.135 |
| 9000 | phase6_novel | 0.116 | 0.329 | 0.330 | 0.115 | 0.111 |
| 9100 | phase6_novel | 0.138 | 0.403 | 0.150 | 0.118 | 0.191 |
| 9200 | phase6_novel | 0.112 | 0.483 | 0.125 | 0.128 | 0.153 |
| 9300 | phase6_novel | 0.132 | 0.335 | 0.093 | 0.336 | 0.104 |
| 9400 | phase6_novel | 0.134 | 0.464 | 0.133 | 0.145 | 0.124 |
| 9500 | phase6_novel | 0.151 | 0.468 | 0.112 | 0.136 | 0.134 |
| 9600 | phase6_novel | 0.126 | 0.490 | 0.120 | 0.153 | 0.112 |
| 9700 | phase6_novel | 0.129 | 0.141 | 0.469 | 0.122 | 0.140 |
| 9800 | phase6_novel | 0.112 | 0.124 | 0.524 | 0.116 | 0.124 |
| 9900 | phase6_novel | 0.148 | 0.148 | 0.290 | 0.122 | 0.291 |
| 9999 | phase6_novel | 0.160 | 0.162 | 0.342 | 0.138 | 0.198 |

## Interpretation

This benchmark evaluates Syntra's contextual bandit against 7 baselines
plus an oracle with perfect knowledge. The simulation includes 6 distinct
regime phases with adversarial scenarios (early winner trap, noisy p99,
hidden queue collapse, false recovery, poisoned telemetry).

Syntra **failed** 2 criteria: c2_beats_adaptive_baselines, c3_p99_vs_ewma_attack.
This indicates areas where the current bandit configuration needs improvement.

## Suggested Improvements

1. Test with Thompson Sampling and UCB1 algorithms
2. Add context-aware routing (use phase/load as context key)
3. Implement sliding-window reward computation
4. Test with delayed feedback (10-100 request lag)
5. Add multi-objective optimization (latency vs cost tradeoff)
6. Test larger provider pools (10-20 providers)
7. Add batch routing decisions
