# Adaptive API Router Resilience Benchmark

**Date:** 2026-05-15 21:46 UTC
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
| syntra ** | 90.3ms | 165.5ms | 238.0ms | 98.83% | 1.01% | 1.24% | $105.84 | 20 | 19.06 |
| round_robin | 263.1ms | 742.0ms | 5144.1ms | 95.01% | 2.99% | 7.56% | $125.91 | 600 | -2.42 |
| random | 265.0ms | 751.9ms | 5159.4ms | 95.04% | 2.93% | 7.59% | $125.87 | 601 | -2.47 |
| lowest_current_latency | 150.1ms | 180.1ms | 4781.4ms | 97.53% | 1.19% | 3.52% | $111.85 | 297 | 3.34 |
| lowest_error_rate | 88.3ms | 128.9ms | 206.7ms | 99.56% | 0.29% | 0.45% | $181.46 | 7 | 16.01 |
| ewma_latency | 88.9ms | 134.2ms | 202.0ms | 99.25% | 0.60% | 0.82% | $139.69 | 2 | 17.98 |
| circuit_breaker | 97.6ms | 108.5ms | 1147.5ms | 98.58% | 0.91% | 2.00% | $113.68 | 73 | 13.63 |
| weighted_static | 409.1ms | 3096.6ms | 5409.9ms | 91.59% | 4.85% | 13.25% | $112.27 | 1196 | -8.59 |
| oracle | 70.1ms | 103.7ms | 169.2ms | 99.41% | 0.46% | 0.60% | $127.95 | 0 | 19.54 |

## Per-Phase: Syntra vs Oracle

| Phase | Syntra Resilience | Oracle Resilience | Gap |
|-------|-------------------|-------------------|-----|
| phase1_normal | 23.35 | 23.35 | +0.00 |
| phase2_degradation | 16.54 | 20.04 | -3.50 |
| phase3_attack | 16.48 | 19.47 | -2.99 |
| phase4_telemetry | 19.42 | 16.66 | +2.76 |
| phase5_recovery | 19.53 | 18.21 | +1.32 |
| phase6_novel | 19.60 | 18.95 | +0.65 |

## Syntra vs Baselines (seed-by-seed win rate)

- **vs round_robin:** Syntra wins 30/30 (100%)
- **vs random:** Syntra wins 30/30 (100%)
- **vs lowest_current_latency:** Syntra wins 30/30 (100%)
- **vs lowest_error_rate:** Syntra wins 30/30 (100%)
- **vs ewma_latency:** Syntra wins 28/30 (93%)
- **vs circuit_breaker:** Syntra wins 29/30 (97%)
- **vs weighted_static:** Syntra wins 30/30 (100%)

## Oracle Regret

- **Mean final cumulative regret:** -28.2
- **Mean midpoint cumulative regret:** 105.1
- **Regret per request (final):** -0.0028

## Pass/Fail Criteria

- **c1_beats_weak_baselines:** PASS (100.0% vs >=80%)
  - Syntra beat round_robin+random+weighted_static in 30/30 seeds
- **c2_beats_adaptive_baselines:** PASS (100.0% vs >=65%)
  - Syntra beat lowest_latency+lowest_error in 30/30 seeds
- **c3_p99_vs_ewma_attack:** FAIL (6.7% vs >=60%)
  - Syntra p99 < EWMA p99 in attack phase in 2/30 seeds
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

### Overall Verdict: **FAIL** (7/7 criteria passed)

## Notable Failures

- Seed 1000: worst phase = phase3_attack (resilience=15.81)
- Seed 1001: worst phase = phase2_degradation (resilience=15.85)
- Seed 1002: worst phase = phase3_attack (resilience=16.37)
- Seed 1003: worst phase = phase2_degradation (resilience=16.80)
- Seed 1004: worst phase = phase2_degradation (resilience=16.52)

## Cases Where Baselines Beat Syntra

- Seed 1010: ewma_latency (19.42) > syntra (19.17)
- Seed 1019: ewma_latency (19.34) > syntra (19.10)
- Seed 1029: circuit_breaker (19.08) > syntra (18.90)

## Syntra Weight Evolution (first seed)

| Request | Phase | provider_a | provider_b | provider_c | provider_d | provider_e |
|---------|-------|------------|------------|------------|------------|------------|
| 0 | phase1_normal | 0.200 | 0.200 | 0.200 | 0.200 | 0.200 |
| 100 | phase1_normal | 0.578 | 0.106 | 0.106 | 0.106 | 0.106 |
| 200 | phase1_normal | 0.580 | 0.105 | 0.105 | 0.105 | 0.105 |
| 300 | phase1_normal | 0.570 | 0.107 | 0.107 | 0.107 | 0.107 |
| 400 | phase1_normal | 0.563 | 0.109 | 0.109 | 0.109 | 0.109 |
| 500 | phase1_normal | 0.565 | 0.109 | 0.109 | 0.109 | 0.109 |
| 600 | phase1_normal | 0.583 | 0.104 | 0.104 | 0.104 | 0.104 |
| 700 | phase1_normal | 0.583 | 0.104 | 0.104 | 0.104 | 0.104 |
| 800 | phase1_normal | 0.570 | 0.107 | 0.107 | 0.107 | 0.107 |
| 900 | phase1_normal | 0.582 | 0.104 | 0.104 | 0.104 | 0.104 |
| 1000 | phase1_normal | 0.547 | 0.113 | 0.113 | 0.113 | 0.113 |
| 1100 | phase1_normal | 0.583 | 0.104 | 0.104 | 0.104 | 0.104 |
| 1200 | phase1_normal | 0.577 | 0.106 | 0.106 | 0.106 | 0.106 |
| 1300 | phase1_normal | 0.584 | 0.104 | 0.104 | 0.104 | 0.104 |
| 1400 | phase1_normal | 0.584 | 0.104 | 0.104 | 0.104 | 0.104 |
| 1500 | phase1_normal | 0.575 | 0.106 | 0.106 | 0.106 | 0.106 |
| 1600 | phase1_normal | 0.573 | 0.107 | 0.107 | 0.107 | 0.107 |
| 1700 | phase1_normal | 0.581 | 0.105 | 0.105 | 0.105 | 0.105 |
| 1800 | phase1_normal | 0.571 | 0.107 | 0.107 | 0.107 | 0.107 |
| 1900 | phase1_normal | 0.583 | 0.104 | 0.104 | 0.104 | 0.104 |
| 2000 | phase2_degradation | 0.583 | 0.104 | 0.104 | 0.104 | 0.104 |
| 2100 | phase2_degradation | 0.581 | 0.105 | 0.105 | 0.105 | 0.105 |
| 2200 | phase2_degradation | 0.550 | 0.112 | 0.112 | 0.112 | 0.112 |
| 2300 | phase2_degradation | 0.573 | 0.107 | 0.107 | 0.107 | 0.107 |
| 2400 | phase2_degradation | 0.576 | 0.106 | 0.106 | 0.106 | 0.106 |
| 2500 | phase2_degradation | 0.567 | 0.108 | 0.108 | 0.108 | 0.108 |
| 2600 | phase2_degradation | 0.572 | 0.107 | 0.107 | 0.107 | 0.107 |
| 2700 | phase2_degradation | 0.546 | 0.114 | 0.114 | 0.114 | 0.114 |
| 2800 | phase2_degradation | 0.516 | 0.121 | 0.121 | 0.121 | 0.121 |
| 2900 | phase2_degradation | 0.550 | 0.113 | 0.113 | 0.113 | 0.113 |
| 3000 | phase2_degradation | 0.529 | 0.118 | 0.118 | 0.118 | 0.118 |
| 3100 | phase2_degradation | 0.548 | 0.113 | 0.113 | 0.113 | 0.113 |
| 3200 | phase2_degradation | 0.528 | 0.118 | 0.118 | 0.118 | 0.118 |
| 3300 | phase2_degradation | 0.504 | 0.124 | 0.124 | 0.124 | 0.124 |
| 3400 | phase2_degradation | 0.525 | 0.119 | 0.119 | 0.119 | 0.119 |
| 3500 | phase3_attack | 0.425 | 0.144 | 0.144 | 0.144 | 0.144 |
| 3600 | phase3_attack | 0.111 | 0.556 | 0.111 | 0.111 | 0.111 |
| 3700 | phase3_attack | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 3800 | phase3_attack | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 3900 | phase3_attack | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 4000 | phase3_attack | 0.111 | 0.557 | 0.111 | 0.111 | 0.111 |
| 4100 | phase3_attack | 0.111 | 0.558 | 0.111 | 0.111 | 0.111 |
| 4200 | phase3_attack | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 4300 | phase3_attack | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 4400 | phase3_attack | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 4500 | phase3_attack | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 4600 | phase3_attack | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 4700 | phase3_attack | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 4800 | phase3_attack | 0.112 | 0.551 | 0.112 | 0.112 | 0.112 |
| 4900 | phase3_attack | 0.119 | 0.522 | 0.119 | 0.119 | 0.119 |
| 5000 | phase4_telemetry | 0.114 | 0.543 | 0.114 | 0.114 | 0.114 |
| 5100 | phase4_telemetry | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 5200 | phase4_telemetry | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 5300 | phase4_telemetry | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 5400 | phase4_telemetry | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 5500 | phase4_telemetry | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 5600 | phase4_telemetry | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 5700 | phase4_telemetry | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 5800 | phase4_telemetry | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 5900 | phase4_telemetry | 0.115 | 0.542 | 0.115 | 0.115 | 0.115 |
| 6000 | phase4_telemetry | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 6100 | phase4_telemetry | 0.110 | 0.558 | 0.110 | 0.110 | 0.110 |
| 6200 | phase4_telemetry | 0.110 | 0.561 | 0.110 | 0.110 | 0.110 |
| 6300 | phase4_telemetry | 0.110 | 0.561 | 0.110 | 0.110 | 0.110 |
| 6400 | phase4_telemetry | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 6500 | phase5_recovery | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 6600 | phase5_recovery | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 6700 | phase5_recovery | 0.116 | 0.537 | 0.116 | 0.116 | 0.116 |
| 6800 | phase5_recovery | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 6900 | phase5_recovery | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 7000 | phase5_recovery | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 7100 | phase5_recovery | 0.115 | 0.541 | 0.115 | 0.115 | 0.115 |
| 7200 | phase5_recovery | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 7300 | phase5_recovery | 0.111 | 0.557 | 0.111 | 0.111 | 0.111 |
| 7400 | phase5_recovery | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 7500 | phase5_recovery | 0.120 | 0.519 | 0.120 | 0.120 | 0.120 |
| 7600 | phase5_recovery | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 7700 | phase5_recovery | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 7800 | phase5_recovery | 0.118 | 0.530 | 0.118 | 0.118 | 0.118 |
| 7900 | phase5_recovery | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 8000 | phase5_recovery | 0.109 | 0.562 | 0.109 | 0.109 | 0.109 |
| 8100 | phase5_recovery | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 8200 | phase6_novel | 0.110 | 0.562 | 0.110 | 0.110 | 0.110 |
| 8300 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 8400 | phase6_novel | 0.113 | 0.547 | 0.113 | 0.113 | 0.113 |
| 8500 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 8600 | phase6_novel | 0.114 | 0.544 | 0.114 | 0.114 | 0.114 |
| 8700 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 8800 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 8900 | phase6_novel | 0.109 | 0.563 | 0.109 | 0.109 | 0.109 |
| 9000 | phase6_novel | 0.116 | 0.536 | 0.116 | 0.116 | 0.116 |
| 9100 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9200 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9300 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9400 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9500 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9600 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9700 | phase6_novel | 0.110 | 0.561 | 0.110 | 0.110 | 0.110 |
| 9800 | phase6_novel | 0.109 | 0.564 | 0.109 | 0.109 | 0.109 |
| 9900 | phase6_novel | 0.112 | 0.553 | 0.112 | 0.112 | 0.112 |
| 9999 | phase6_novel | 0.114 | 0.546 | 0.114 | 0.114 | 0.114 |

## Interpretation

This benchmark evaluates Syntra's contextual bandit against 7 baselines
plus an oracle with perfect knowledge. The simulation includes 6 distinct
regime phases with adversarial scenarios (early winner trap, noisy p99,
hidden queue collapse, false recovery, poisoned telemetry).

Syntra **failed** 1 criteria: c3_p99_vs_ewma_attack.
This indicates areas where the current bandit configuration needs improvement.

## Suggested Improvements

1. Test with Thompson Sampling and UCB1 algorithms
2. Add context-aware routing (use phase/load as context key)
3. Implement sliding-window reward computation
4. Test with delayed feedback (10-100 request lag)
5. Add multi-objective optimization (latency vs cost tradeoff)
6. Test larger provider pools (10-20 providers)
7. Add batch routing decisions
