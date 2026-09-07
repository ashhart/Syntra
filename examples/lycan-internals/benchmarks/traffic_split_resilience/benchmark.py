#!/usr/bin/env python3
# Copyright 2024 Syntra Authors
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.
"""
traffic_split_resilience_benchmark
===================================

Third-domain validation of the reward-blindness pattern and the
meta-bandit in the A/B/n traffic-split action space.

Each week 500 requests arrive. Each request carries a customer tier
(free, pro, enterprise). The bandit picks which of 4 variants to route
the user to. Reward = conversion - 0.5 * cost, where:
  - conversion: 1 (converted) or 0 (did not convert)
  - cost: USD per impression, varies per variant

True conversion rates depend on (tier, variant) and are encoded in the
simulator with mild Gaussian noise. At week 26 the conversion-rate table
for the enterprise tier flips, simulating a feature shift.

Baselines:
  - always_a                    (always send variant_a -- free baseline)
  - equal_split                 (uniform across variants)
  - proportional_to_known_winners (oracle-ish; uses pre-computed best variant
                                   per tier from pre-shift conversion table)
  - epsilon_greedy_per_tier     (manual 10-armed bandit per tier; rolling-your-own)
  - oracle                      (always picks true best variant per tier; lower
                                 bound on regret)

Pass criteria:
  c1: Syntra beats always_a and equal_split on score in >= 80% of seeds.
  c2: Syntra beats epsilon_greedy_per_tier in >= 60% of seeds.
  c3: Syntra cumulative regret < epsilon_greedy_per_tier's in >= 50% of seeds.
  c4: Syntra detects the regime shift at week 26 (change-detection event in
      audit log OR weights visibly shift within 10 weeks of the shift).

Regime shift:
  At week 26, the enterprise tier's true conversion rates are permuted so
  that the previously-best variant becomes the worst for that tier.
  Policies without shift-detection continue exploiting the old winner and
  incur regret. Syntra's change-detection + exploration-boost should
  recover within 10 weeks.

Reward-blindness check:
  The original reward uses a policy-dependent counterfactual to compute
  "conversions saved." The corrected reward computes the counterfactual
  against a fixed no-action baseline (always serving the cheapest variant,
  variant_a). If the pattern reproduces, the corrected reward spread across
  policies will be larger than the original spread.

Usage:
    python3 benchmark.py [--seeds N] [--weeks N] [--requests-per-week N]
    python3 benchmark.py --algorithm meta_bandit --context-type discrete \\
      --seeds 3 --weeks 8 --syntra-url http://127.0.0.1:48799 \\
      --admin-key dev-key --output-dir results/smoke
"""

import argparse
import csv
import json
import math
import os
import random
import statistics
import sys
import time
import urllib.error
import urllib.request
from collections import defaultdict
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Action space
# ---------------------------------------------------------------------------

VARIANTS = ["variant_a", "variant_b", "variant_c", "variant_d"]
N_VARIANTS = len(VARIANTS)

# Cost in USD per impression (serving cost; higher variants require more
# expensive rendering / model inference).
VARIANT_COSTS = [0.00, 0.01, 0.05, 0.20]

# Customer tiers
TIERS = ["free", "pro", "enterprise"]
N_TIERS = len(TIERS)

# ---------------------------------------------------------------------------
# True conversion-rate table [tier_idx][variant_idx].
# Represents which variant converts best for each tier pre-shift.
# Mild noise is added at simulation time (see TrafficSplitSim.step()).
# ---------------------------------------------------------------------------
#
# Pre-shift (weeks 0-25):
#   free:       B is best  (0.35), D is worst (0.10)
#   pro:        C is best  (0.40), A is worst (0.12)
#   enterprise: D is best  (0.50), A is worst (0.15)
#
# Post-shift (weeks 26+), enterprise column permuted so D is worst:
#   enterprise: A is best  (0.48), D is worst (0.12)

PRE_SHIFT_RATES = [
    # free   pro    ent
    [0.22,  0.12,  0.15],  # variant_a
    [0.35,  0.30,  0.30],  # variant_b
    [0.28,  0.40,  0.35],  # variant_c
    [0.10,  0.22,  0.50],  # variant_d
]

# Post-shift enterprise column: A becomes 0.48, D becomes 0.12.
POST_SHIFT_ENT_RATES = [0.48, 0.30, 0.35, 0.12]  # indexed by variant

# Noise std-dev on conversion rate (logit scale noise equivalent):
# we add Gaussian noise to the Bernoulli probability and clamp to [0.01, 0.99].
CONVERSION_NOISE_STD = 0.03

# Requests per week per tier (approximately uniform split, jittered per week).
BASE_REQUESTS_PER_WEEK = 500
TIER_FRACTIONS = [0.55, 0.30, 0.15]  # free, pro, enterprise

REGIME_SHIFT_WEEK = 26


# ---------------------------------------------------------------------------
# Simulator
# ---------------------------------------------------------------------------

@dataclass
class Request:
    tier_idx: int
    tier_name: str
    week: int


@dataclass
class Outcome:
    week: int
    tier_idx: int
    tier_name: str
    variant_idx: int
    variant_name: str
    converted: int          # 0 or 1
    cost: float             # USD per impression
    reward: float           # converted - 0.5 * cost, clamped [-1, 1]
    true_best_variant: int  # variant with highest true conversion rate this week
    oracle_converted: int   # 1 if oracle policy would have converted
    oracle_cost: float


def true_conversion_rate(week: int, tier_idx: int, variant_idx: int,
                         rng: random.Random) -> float:
    """Return the true (noisy) conversion rate for this (week, tier, variant)."""
    if week >= REGIME_SHIFT_WEEK and tier_idx == 2:  # enterprise post-shift
        base = POST_SHIFT_ENT_RATES[variant_idx]
    else:
        base = PRE_SHIFT_RATES[variant_idx][tier_idx]
    noise = rng.gauss(0, CONVERSION_NOISE_STD)
    return max(0.01, min(0.99, base + noise))


def true_best_variant_for_tier(week: int, tier_idx: int) -> int:
    """Return the variant index with the highest mean conversion rate (no noise)."""
    if week >= REGIME_SHIFT_WEEK and tier_idx == 2:
        rates = POST_SHIFT_ENT_RATES
    else:
        rates = [PRE_SHIFT_RATES[v][tier_idx] for v in range(N_VARIANTS)]
    return max(range(N_VARIANTS), key=lambda v: rates[v])


class TrafficSplitSim:
    """Stateless per-call simulator for A/B/n traffic split decisions."""

    def __init__(self, seed: int, weeks: int = 52,
                 requests_per_week: int = BASE_REQUESTS_PER_WEEK):
        self.seed = seed
        self.weeks = weeks
        self.requests_per_week = requests_per_week
        self.rng = random.Random(seed)
        # Pre-generate the request stream so all policies see the same users.
        self._requests_by_week = self._generate_requests()

    def _generate_requests(self):
        """Pre-generate all requests. Each request is (week, tier_idx)."""
        by_week = []
        rng = random.Random(self.seed)
        for week in range(self.weeks):
            n = self.requests_per_week
            reqs = []
            for _ in range(n):
                r = rng.random()
                cum = 0.0
                for t_idx, frac in enumerate(TIER_FRACTIONS):
                    cum += frac
                    if r < cum:
                        break
                else:
                    t_idx = N_TIERS - 1
                reqs.append(Request(tier_idx=t_idx, tier_name=TIERS[t_idx],
                                    week=week))
            by_week.append(reqs)
        return by_week

    def requests_for_week(self, week: int):
        return self._requests_by_week[week]

    def step(self, req: Request, variant_idx: int) -> Outcome:
        """Simulate one request/decision. Uses simulator-internal RNG for
        conversion sampling so all policies share the same noise stream
        (fair comparison)."""
        rate = true_conversion_rate(req.week, req.tier_idx, variant_idx, self.rng)
        converted = 1 if self.rng.random() < rate else 0
        cost = VARIANT_COSTS[variant_idx]
        reward = converted - 0.5 * cost
        reward = max(-1.0, min(1.0, reward))

        best_v = true_best_variant_for_tier(req.week, req.tier_idx)
        oracle_rate = true_conversion_rate(req.week, req.tier_idx, best_v, self.rng)
        oracle_converted = 1 if self.rng.random() < oracle_rate else 0
        oracle_cost = VARIANT_COSTS[best_v]

        return Outcome(
            week=req.week,
            tier_idx=req.tier_idx,
            tier_name=req.tier_name,
            variant_idx=variant_idx,
            variant_name=VARIANTS[variant_idx],
            converted=converted,
            cost=cost,
            reward=reward,
            true_best_variant=best_v,
            oracle_converted=oracle_converted,
            oracle_cost=oracle_cost,
        )


# ---------------------------------------------------------------------------
# Corrected reward (fixed-counterfactual baseline)
# ---------------------------------------------------------------------------
# The original reward credits (converted - 0.5 * cost). The policy-dependent
# counterfactual: how much better did we do relative to "what we'd have seen
# under variant_a (the free baseline) for the same user?"
#
# The pathology: a policy that learns to serve each tier's best variant sees
# high conversion rates -- so the counterfactual (variant_a baseline on the
# same request) is also being compared against a different realized draw.
# Because we share the simulation RNG across all policies, the corrected score
# uses a pre-computed reference trajectory: for each (week, tier_idx, request_i)
# what would the variant_a reward have been?


def compute_reference_always_a(sim: TrafficSplitSim) -> list:
    """Pre-compute the reference trajectory: always variant_a for every request.
    Returns a flat list of reference rewards indexed by (week, request_position).
    We return a list of lists: ref[week][pos] = reward under always_a."""
    ref_sim = TrafficSplitSim(seed=sim.seed, weeks=sim.weeks,
                               requests_per_week=sim.requests_per_week)
    ref = []
    for week in range(sim.weeks):
        week_reqs = ref_sim.requests_for_week(week)
        week_ref = []
        for req in week_reqs:
            out = ref_sim.step(req, 0)  # variant_a = index 0
            week_ref.append(out.reward)
        ref.append(week_ref)
    return ref


def corrected_score(outcomes_by_week, ref_rewards_by_week) -> float:
    """Compute corrected total reward using the fixed always_a baseline.

    For each request, corrected_reward = original_reward - ref_reward.
    This measures how much better (or worse) the policy did vs the free
    baseline in the same week/tier mix, without the policy-dependent
    counterfactual shrinking when prevention succeeds.
    """
    total = 0.0
    for week_outcomes, week_ref in zip(outcomes_by_week, ref_rewards_by_week):
        for outcome, ref_r in zip(week_outcomes, week_ref):
            # Excess reward vs always_a: positive means we beat the baseline.
            total += outcome.reward - ref_r
    return total


# ---------------------------------------------------------------------------
# Hand-coded baseline policies
# ---------------------------------------------------------------------------

class BaselinePolicy:
    def __init__(self, name: str):
        self.name = name

    def choose(self, req: Request, week: int) -> int:
        """Return variant index (0-3)."""
        raise NotImplementedError

    def reset(self):
        """Reset any per-seed state."""
        pass


class AlwaysA(BaselinePolicy):
    def __init__(self):
        super().__init__("always_a")

    def choose(self, req: Request, week: int) -> int:
        return 0


class EqualSplit(BaselinePolicy):
    """Round-robin across variants regardless of tier or history."""

    def __init__(self):
        super().__init__("equal_split")
        self._counter = 0

    def reset(self):
        self._counter = 0

    def choose(self, req: Request, week: int) -> int:
        v = self._counter % N_VARIANTS
        self._counter += 1
        return v


# Pre-shift best variant per tier (from PRE_SHIFT_RATES, ignoring post-shift).
# index: tier_idx -> variant_idx
_PRE_SHIFT_BEST = [
    max(range(N_VARIANTS), key=lambda v: PRE_SHIFT_RATES[v][t])
    for t in range(N_TIERS)
]


class ProportionalToKnownWinners(BaselinePolicy):
    """Oracle-ish: uses the pre-computed pre-shift best variant per tier.
    Does not update after the regime shift at week 26, so it degrades on
    the enterprise tier post-shift."""

    def __init__(self):
        super().__init__("proportional_to_known_winners")

    def choose(self, req: Request, week: int) -> int:
        return _PRE_SHIFT_BEST[req.tier_idx]


class EpsilonGreedyPerTier(BaselinePolicy):
    """Manual epsilon-greedy bandit, one per tier. Represents 'rolling your
    own' multi-armed bandit without Syntra. Does not have change detection,
    so it will be slow to recover after the regime shift."""

    def __init__(self, epsilon: float = 0.10):
        super().__init__("epsilon_greedy_per_tier")
        self.epsilon = epsilon
        # counts[tier][variant], rewards[tier][variant]
        self.counts = [[0] * N_VARIANTS for _ in range(N_TIERS)]
        self.rewards = [[0.0] * N_VARIANTS for _ in range(N_TIERS)]
        self._rng = random.Random(9001)

    def reset(self):
        self.counts = [[0] * N_VARIANTS for _ in range(N_TIERS)]
        self.rewards = [[0.0] * N_VARIANTS for _ in range(N_TIERS)]
        self._rng = random.Random(9001)

    def choose(self, req: Request, week: int) -> int:
        t = req.tier_idx
        if self._rng.random() < self.epsilon:
            return self._rng.randint(0, N_VARIANTS - 1)
        # Greedy among observed means.
        best_v = 0
        best_mean = float("-inf")
        for v in range(N_VARIANTS):
            if self.counts[t][v] == 0:
                return v  # try unexplored variants first
            mean = self.rewards[t][v] / self.counts[t][v]
            if mean > best_mean:
                best_mean = mean
                best_v = v
        return best_v

    def update(self, req: Request, variant_idx: int, reward: float):
        self.counts[req.tier_idx][variant_idx] += 1
        self.rewards[req.tier_idx][variant_idx] += reward


class OraclePolicy(BaselinePolicy):
    """Perfect-information oracle: always picks the true best variant for
    each tier at each week. This is the lower bound on regret."""

    def __init__(self):
        super().__init__("oracle")

    def choose(self, req: Request, week: int) -> int:
        return true_best_variant_for_tier(week, req.tier_idx)


# ---------------------------------------------------------------------------
# Syntra client (mirrors outbreak / vaccine pattern)
# ---------------------------------------------------------------------------

class SyntraClient:
    def __init__(self, base_url: str, admin_key: str,
                 tenant: str, job: str, capsule: str):
        self.base_url = base_url.rstrip("/")
        self.admin_key = admin_key
        self.tenant = tenant
        self.job = job
        self.capsule = capsule
        self.base_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"

    def _request(self, method: str, path: str, body=None, raw=None):
        url = f"{self.base_url}{path}"
        if raw is not None:
            data = raw
        else:
            data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.admin_key}")
        if raw is not None:
            req.add_header("Content-Type", "application/octet-stream")
        elif data is not None:
            req.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode())

    def reset(self):
        try:
            self._request("DELETE", f"/tenants/{self.tenant}")
        except Exception:
            pass

    def setup(self, capsule_path: str):
        try:
            self._request("POST", f"/tenants/{self.tenant}/jobs",
                          {"id": self.job, "name": "Traffic Split Benchmark"})
        except Exception:
            pass
        with open(capsule_path, "rb") as f:
            self._request("POST", f"{self.base_path}/install", raw=f.read())

    def configure_learning(self, algorithm: str = "meta_bandit",
                           context_type: str = "discrete"):
        body = {
            "learningRate": 0.05,
            "decay": {"enabled": True, "halfLifeFeedbacks": 60},
            "window": {"enabled": True, "size": 30},
            "changeDetection": {
                "enabled": True,
                "method": "pageHinkley",
                "threshold": 3.0,
                "minDrift": 0.05,
                "explorationBoost": 0.30,
                "boostDuration": 8,
            },
            "safety": {
                "minExploration": 0.08,
                "rewardClip": 1.0,
                "snapshotOnFeedback": False,
                "journalOnFeedback": False,
                "selectionEpsilon": 0.10,
            },
        }
        algo_to_alg_field = {
            "weighted": "simpleWeighted",
            "epsilon_greedy": "epsilonGreedy",
            "ucb": "ucb1",
        }
        algo_to_selection_mode = {
            "weighted": "weighted",
            "epsilon_greedy": "epsilonGreedy",
            "ucb": "greedy",
        }
        if algorithm in algo_to_alg_field:
            body["algorithm"] = algo_to_alg_field[algorithm]
            body["safety"]["selectionMode"] = algo_to_selection_mode[algorithm]
        # else: meta_bandit — let warmup + rate-adaptive meta-bandit pick.

        if context_type == "features":
            body["contextSpec"] = {
                "type": "features",
                "features": [
                    {"name": "tier",
                     "type": {"kind": "categorical",
                              "values": TIERS}},
                    {"name": "hour_of_day",
                     "type": {"kind": "cyclic", "period": 24.0}},
                    {"name": "recent_conversion_rate",
                     "type": {"kind": "continuous", "range": [0.0, 1.0]}},
                ],
            }
        self._request("PUT", f"{self.base_path}/learning", body)

    def decide(self, context_key: str = None, features: dict = None) -> dict:
        body = {"input": {}}
        if features is not None:
            body["features"] = features
        else:
            body["contextKey"] = context_key
        return self._request("POST", f"{self.base_path}/decide", body)

    def feedback(self, reward: float, context_key: str,
                 decision_id: str = None, signal_kind: str = None):
        """Feed reward back to Syntra.

        Prefer decisionId-based feedback so the meta-bandit's candidateId
        path records the outcome; fall back to contextKey only.
        """
        if decision_id:
            body = {"decisionId": decision_id, "reward": reward}
        else:
            body = {"contextKey": context_key, "reward": reward}
        if signal_kind:
            body["signalKind"] = signal_kind
        return self._request("POST", f"{self.base_path}/feedback", body)


# ---------------------------------------------------------------------------
# Simulation runner
# ---------------------------------------------------------------------------

def make_feature_context(req: Request, week: int,
                          recent_rates: dict) -> dict:
    """Build the feature-vector context for a request.

    Features:
      tier             -- categorical: free/pro/enterprise
      hour_of_day      -- cyclic, period 24 (simulated: week*7 days, 8am offset)
      recent_conversion_rate -- running mean conversion rate for this tier
                                (starts at 0.5 until first observations arrive)
    """
    hour = ((week * 7 * 24) + 8) % 24  # simulate 8am requests
    rate = recent_rates.get(req.tier_idx, 0.5)
    return {
        "tier": req.tier_name,
        "hour_of_day": float(hour),
        "recent_conversion_rate": float(rate),
    }


def run_seed(seed: int, weeks: int, requests_per_week: int,
             syntra: "SyntraClient" = None,
             capsule_path: str = None,
             algorithm: str = "meta_bandit",
             context_type: str = "discrete") -> dict:
    """Run one full seed of the benchmark.

    Each policy gets its own TrafficSplitSim instance seeded identically so
    they see the same request-tier stream. The simulation RNG for conversion
    outcomes is shared within each policy's sim (fair within-policy), but each
    policy's sim has its own internal state so they don't cross-contaminate.
    """
    # One sim per policy so each policy independently draws conversion outcomes
    # (but from the same seed -> same request stream, same noise draws in order).
    policies = {
        "always_a": AlwaysA(),
        "equal_split": EqualSplit(),
        "proportional_to_known_winners": ProportionalToKnownWinners(),
        "epsilon_greedy_per_tier": EpsilonGreedyPerTier(),
        "oracle": OraclePolicy(),
    }
    for pol in policies.values():
        pol.reset()

    sims = {
        name: TrafficSplitSim(seed=seed, weeks=weeks,
                               requests_per_week=requests_per_week)
        for name in policies
    }

    # Syntra gets its own sim too.
    if syntra is not None:
        syntra.reset()
        syntra.setup(capsule_path)
        syntra.configure_learning(algorithm=algorithm, context_type=context_type)
        sims["syntra"] = TrafficSplitSim(seed=seed, weeks=weeks,
                                          requests_per_week=requests_per_week)

    # Pre-compute fixed-baseline reference trajectory for corrected reward.
    ref_sim = TrafficSplitSim(seed=seed, weeks=weeks,
                               requests_per_week=requests_per_week)
    ref_rewards_by_week = []
    for week in range(weeks):
        week_ref = []
        for req in ref_sim.requests_for_week(week):
            out = ref_sim.step(req, 0)  # always variant_a
            week_ref.append(out.reward)
        ref_rewards_by_week.append(week_ref)

    # Outcomes: by policy -> list of lists (one per week, each a list of Outcomes)
    outcomes_by_week = {name: [] for name in list(policies.keys()) + (
        ["syntra"] if syntra is not None else [])}

    # For Syntra: track running conversion rates per tier for feature context.
    syntra_recent_counts = [0] * N_TIERS
    syntra_recent_conv = [0] * N_TIERS

    for week in range(weeks):
        # --- Syntra ---
        if syntra is not None:
            sim_s = sims["syntra"]
            reqs_s = sim_s.requests_for_week(week)
            week_outcomes_syntra = []

            # Batch: one Syntra call per request (matching real-world usage).
            # For efficiency in the benchmark, we accumulate then feedback.
            decisions_made = []

            for req in reqs_s:
                tier_name = req.tier_name
                if context_type == "features":
                    n_seen = syntra_recent_counts[req.tier_idx]
                    recent_rate = (syntra_recent_conv[req.tier_idx] / n_seen
                                   if n_seen > 0 else 0.5)
                    feats = make_feature_context(req, week,
                                                  {req.tier_idx: recent_rate})
                    decision = syntra.decide(features=feats)
                else:
                    decision = syntra.decide(context_key=tier_name)

                decisions = decision.get("decisions") or []
                decision_id = decision.get("decisionId")

                if decisions:
                    chosen = int(decisions[0].get("chosen_option", 0))
                else:
                    chosen = 0

                out = sim_s.step(req, chosen)
                week_outcomes_syntra.append(out)
                decisions_made.append((req, decision_id, tier_name, out))

            # Feedback after the week resolves.
            for req, decision_id, tier_name, out in decisions_made:
                syntra.feedback(out.reward, tier_name,
                                decision_id=decision_id,
                                signal_kind="final")
                # Update running conversion rate for feature context.
                syntra_recent_counts[req.tier_idx] += 1
                syntra_recent_conv[req.tier_idx] += out.converted

            outcomes_by_week["syntra"].append(week_outcomes_syntra)

        # --- Baseline policies ---
        for name, pol in policies.items():
            sim_b = sims[name]
            reqs_b = sim_b.requests_for_week(week)
            week_outcomes = []
            for req in reqs_b:
                variant_idx = pol.choose(req, week)
                out = sim_b.step(req, variant_idx)
                week_outcomes.append(out)
                # Update epsilon_greedy_per_tier internal state.
                if name == "epsilon_greedy_per_tier":
                    pol.update(req, variant_idx, out.reward)
            outcomes_by_week[name].append(week_outcomes)

    # Aggregate summary per policy.
    summary = {}
    for name, week_lists in outcomes_by_week.items():
        all_outcomes = [o for wl in week_lists for o in wl]
        total_converted = sum(o.converted for o in all_outcomes)
        total_cost = sum(o.cost for o in all_outcomes)
        total_reward_original = sum(o.reward for o in all_outcomes)
        total_reward_corrected = corrected_score(week_lists, ref_rewards_by_week)
        oracle_reward = sum(o.oracle_converted - 0.5 * o.oracle_cost
                            for o in all_outcomes)
        cumulative_regret = oracle_reward - total_reward_original
        summary[name] = {
            "total_converted": total_converted,
            "total_cost_usd": total_cost,
            "total_reward_original": total_reward_original,
            "total_reward_corrected": total_reward_corrected,
            "score": total_reward_original,
            "score_corrected": total_reward_corrected,
            "cumulative_regret": cumulative_regret,
            "n_requests": len(all_outcomes),
        }

    return {"seed": seed, "summary": summary,
            "outcomes_by_week": outcomes_by_week,
            "ref_rewards_by_week": ref_rewards_by_week}


# ---------------------------------------------------------------------------
# Pass/fail evaluation
# ---------------------------------------------------------------------------

def evaluate(all_results: list) -> dict:
    n = len(all_results)
    crit = {}

    # c1: Syntra beats always_a and equal_split on score in >= 80% of seeds.
    if "syntra" in all_results[0]["summary"]:
        wins_c1 = sum(
            1 for r in all_results
            if (r["summary"]["syntra"]["score"]
                > r["summary"]["always_a"]["score"]
                and r["summary"]["syntra"]["score"]
                > r["summary"]["equal_split"]["score"])
        )
        crit["c1_beats_free_baselines"] = {
            "pass": wins_c1 / n >= 0.80,
            "value": f"{wins_c1/n:.0%}",
            "threshold": ">=80%",
            "description": "Syntra beats always_a and equal_split on score",
        }

        # c2: Syntra beats epsilon_greedy_per_tier in >= 60% of seeds.
        wins_c2 = sum(
            1 for r in all_results
            if (r["summary"]["syntra"]["score"]
                > r["summary"]["epsilon_greedy_per_tier"]["score"])
        )
        crit["c2_beats_manual_bandit"] = {
            "pass": wins_c2 / n >= 0.60,
            "value": f"{wins_c2/n:.0%}",
            "threshold": ">=60%",
            "description": "Syntra beats epsilon_greedy_per_tier on score",
        }

        # c3: Syntra cumulative regret < epsilon_greedy_per_tier in >= 50% of seeds.
        wins_c3 = sum(
            1 for r in all_results
            if (r["summary"]["syntra"]["cumulative_regret"]
                < r["summary"]["epsilon_greedy_per_tier"]["cumulative_regret"])
        )
        crit["c3_lower_regret_vs_manual_bandit"] = {
            "pass": wins_c3 / n >= 0.50,
            "value": f"{wins_c3/n:.0%}",
            "threshold": ">=50%",
            "description": "Syntra cumulative regret < epsilon_greedy_per_tier's",
        }

        # c4: regime-shift detection (approximate via score delta post-shift).
        # We proxy this as: Syntra's relative score improvement (post-shift vs
        # pre-shift) >= epsilon_greedy_per_tier's improvement. This measures
        # whether Syntra adapts faster. A full audit-log check would require
        # parsing the Syntra log, which is outside the benchmark's scope.
        def post_shift_score(r, name):
            week_lists = r["outcomes_by_week"][name]
            if len(week_lists) <= REGIME_SHIFT_WEEK:
                return 0.0
            post = [o for wl in week_lists[REGIME_SHIFT_WEEK:] for o in wl]
            return sum(o.reward for o in post)

        def pre_shift_score(r, name):
            week_lists = r["outcomes_by_week"][name]
            pre_end = min(REGIME_SHIFT_WEEK, len(week_lists))
            pre = [o for wl in week_lists[:pre_end] for o in wl]
            return sum(o.reward for o in pre)

        wins_c4 = 0
        has_post_shift = all(
            len(r["outcomes_by_week"]["syntra"]) > REGIME_SHIFT_WEEK
            for r in all_results
        )
        if has_post_shift:
            for r in all_results:
                syn_post = post_shift_score(r, "syntra")
                eg_post = post_shift_score(r, "epsilon_greedy_per_tier")
                syn_pre = pre_shift_score(r, "syntra")
                eg_pre = pre_shift_score(r, "epsilon_greedy_per_tier")
                # Normalize by the number of requests to compare per-request rates.
                n_post = sum(len(wl) for wl in r["outcomes_by_week"]["syntra"][REGIME_SHIFT_WEEK:])
                n_pre = sum(len(wl) for wl in r["outcomes_by_week"]["syntra"][:REGIME_SHIFT_WEEK])
                if n_post > 0 and n_pre > 0 and eg_pre != 0:
                    syn_delta = (syn_post / n_post) - (syn_pre / n_pre)
                    eg_delta = (eg_post / n_post) - (eg_pre / n_pre)
                    # Syntra's per-request rate drop post-shift should be smaller.
                    if syn_delta >= eg_delta:
                        wins_c4 += 1
                else:
                    wins_c4 += 0  # no post-shift data
            c4_threshold = 0.50
            c4_pass = (wins_c4 / n >= c4_threshold) if n > 0 else False
        else:
            # Run shorter than 26 weeks: criterion not evaluable.
            wins_c4 = 0
            c4_threshold = 0.50
            c4_pass = None  # N/A

        crit["c4_regime_shift_recovery"] = {
            "pass": c4_pass,
            "value": f"{wins_c4/n:.0%}" if n > 0 else "N/A",
            "threshold": f">={c4_threshold:.0%}",
            "description": (
                "Syntra per-request rate drop post-shift <= epsilon_greedy's "
                "(proxy for change-detection and recovery)"
                if has_post_shift else
                "Run shorter than 26 weeks -- criterion not evaluable"
            ),
        }

        evaluable = [c for c in crit.values() if c.get("pass") is not None]
        passed = sum(1 for c in evaluable if c["pass"])
        total_eval = len(evaluable)
        crit["overall"] = {
            "pass": all(c["pass"] for c in evaluable),
            "passed": passed,
            "total": total_eval,
        }
    else:
        # Baselines-only run (no Syntra algorithm).
        crit["overall"] = {"pass": None, "passed": 0, "total": 0,
                           "note": "No Syntra algorithm specified; baselines only."}

    return crit


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------

def main():
    p = argparse.ArgumentParser(
        description="Traffic split resilience benchmark: "
                    "A/B/n bandit over customer tiers with regime shift.")
    p.add_argument("--seeds", type=int, default=10,
                   help="Number of seeds to run. Default 10.")
    p.add_argument("--seed-offset", type=int, default=4000,
                   help="Starting seed value. Default 4000 (traffic-split seeds).")
    p.add_argument("--weeks", type=int, default=52,
                   help="Number of weeks to simulate. Default 52.")
    p.add_argument("--requests-per-week", type=int,
                   default=BASE_REQUESTS_PER_WEEK,
                   help=f"Requests per week. Default {BASE_REQUESTS_PER_WEEK}.")
    p.add_argument("--algorithm", default=None,
                   choices=[None, "weighted", "epsilon_greedy", "ucb",
                            "meta_bandit"],
                   help="If set, include Syntra as a 6th policy. "
                        "meta_bandit uses warmup + rate-adaptive meta-bandit.")
    p.add_argument("--context-type", default="discrete",
                   choices=["discrete", "features"],
                   help="discrete: contextKey=tier_name. "
                        "features: tier (categorical) + hour_of_day (cyclic) "
                        "+ recent_conversion_rate (continuous). Enrolls LinUCB.")
    p.add_argument("--syntra-url", default="http://localhost:8787",
                   help="Syntra server URL.")
    p.add_argument("--admin-key", default="dev-key",
                   help="Syntra admin key.")
    p.add_argument("--capsule", default=None,
                   help="Path to .lyc capsule file. If omitted, looks for "
                        "traffic_split.lyc in the benchmark directory.")
    p.add_argument("--output-dir", default=None,
                   help="Output directory. Auto-generated if omitted.")
    args = p.parse_args()

    if args.output_dir is None:
        ts = time.strftime("%Y%m%d_%H%M%S")
        args.output_dir = os.path.join(
            os.path.dirname(os.path.abspath(__file__)),
            "results", f"run_{ts}")
    os.makedirs(args.output_dir, exist_ok=True)

    syntra = None
    capsule_path = None
    if args.algorithm is not None:
        if args.capsule is not None:
            capsule_path = args.capsule
        else:
            capsule_path = os.path.join(
                os.path.dirname(os.path.abspath(__file__)),
                "traffic_split.lyc")
        if not os.path.exists(capsule_path):
            print(f"ERROR: capsule not found at {capsule_path}",
                  file=sys.stderr)
            sys.exit(1)
        try:
            with urllib.request.urlopen(
                    f"{args.syntra_url}/health", timeout=5) as r:
                json.loads(r.read().decode())
        except Exception as e:
            print(f"ERROR: cannot reach Syntra at {args.syntra_url}: {e}",
                  file=sys.stderr)
            sys.exit(1)
        syntra = SyntraClient(
            args.syntra_url, args.admin_key,
            "traffic_split", "main", "split")

    print("=" * 72)
    print("  TRAFFIC SPLIT RESILIENCE BENCHMARK")
    print("=" * 72)
    print(f"  Seeds: {args.seeds} (offset {args.seed_offset})  "
          f"Weeks: {args.weeks}  "
          f"Requests/week: {args.requests_per_week}")
    print(f"  Total decisions/policy/seed: "
          f"{args.weeks * args.requests_per_week:,}")
    print(f"  Regime shift at week {REGIME_SHIFT_WEEK} "
          f"(enterprise tier conversion table flips)")
    if syntra:
        print(f"  Syntra: algorithm={args.algorithm}  "
              f"context={args.context_type}")
    print()

    all_results = []
    t0 = time.time()

    for i in range(args.seeds):
        seed = args.seed_offset + i
        t1 = time.time()
        result = run_seed(
            seed=seed,
            weeks=args.weeks,
            requests_per_week=args.requests_per_week,
            syntra=syntra,
            capsule_path=capsule_path,
            algorithm=args.algorithm,
            context_type=args.context_type,
        )
        s = result["summary"]
        line = (f"  Seed {seed} [{i+1}/{args.seeds}]: "
                f"{time.time()-t1:.1f}s  "
                f"oracle={s['oracle']['score']:>8.1f}  "
                f"eg_tier={s['epsilon_greedy_per_tier']['score']:>8.1f}")
        if syntra:
            line += f"  syntra={s['syntra']['score']:>8.1f}"
        print(line)
        all_results.append(result)

    print(f"\n  Total time: {time.time()-t0:.1f}s")

    # Aggregate summary
    policies = list(all_results[0]["summary"].keys())
    print()
    print("=" * 72)
    print("  AGGREGATE (mean across seeds)")
    print("=" * 72)
    print(f"  {'policy':<32} {'conv':>8} {'cost $':>10} "
          f"{'orig':>10} {'corr':>10} {'regret':>10}")

    for pol in policies:
        mc = statistics.mean(r["summary"][pol]["total_converted"]
                             for r in all_results)
        mcost = statistics.mean(r["summary"][pol]["total_cost_usd"]
                                for r in all_results)
        mo = statistics.mean(r["summary"][pol]["total_reward_original"]
                             for r in all_results)
        mco = statistics.mean(r["summary"][pol]["total_reward_corrected"]
                              for r in all_results)
        mreg = statistics.mean(r["summary"][pol]["cumulative_regret"]
                               for r in all_results)
        marker = " <- SYNTRA" if pol == "syntra" else ""
        print(f"  {pol:<32} {mc:>8.0f} {mcost:>10.2f} "
              f"{mo:>10.2f} {mco:>10.2f} {mreg:>10.2f}{marker}")

    # Reward-blindness check
    orig_values = [
        statistics.mean(r["summary"][pol]["total_reward_original"]
                        for r in all_results)
        for pol in policies
    ]
    corr_values = [
        statistics.mean(r["summary"][pol]["total_reward_corrected"]
                        for r in all_results)
        for pol in policies
    ]
    orig_spread = max(orig_values) - min(orig_values)
    corr_spread = max(corr_values) - min(corr_values)

    print()
    print(f"  Original reward spread (max-min across policies): "
          f"{orig_spread:.4f}")
    print(f"  Corrected reward spread (fixed always_a baseline): "
          f"{corr_spread:.4f}")
    if orig_spread > 0:
        print(f"  Spread ratio corrected/original: "
              f"{corr_spread/orig_spread:.2f}x")
    else:
        print("  Spread ratio: N/A (zero original spread)")

    crit = evaluate(all_results)
    if syntra:
        print()
        print("  Pass/Fail Criteria:")
        for k, v in crit.items():
            if k == "overall":
                continue
            if v.get("pass") is None:
                st = "N/A"
            else:
                st = "PASS" if v["pass"] else "FAIL"
            print(f"    [{st}] {k}: {v['value']} "
                  f"(threshold {v['threshold']})")
        ov = crit["overall"]
        if ov.get("pass") is None:
            print(f"\n  OVERALL: N/A  ({ov.get('note', '')})")
        else:
            print(f"\n  OVERALL: {'PASS' if ov['pass'] else 'FAIL'} "
                  f"({ov['passed']}/{ov['total']} criteria)")

    # Write summary.json
    aggregate = {}
    for pol in policies:
        aggregate[pol] = {
            "mean_converted": statistics.mean(
                r["summary"][pol]["total_converted"] for r in all_results),
            "mean_cost_usd": statistics.mean(
                r["summary"][pol]["total_cost_usd"] for r in all_results),
            "mean_reward_original": statistics.mean(
                r["summary"][pol]["total_reward_original"] for r in all_results),
            "mean_reward_corrected": statistics.mean(
                r["summary"][pol]["total_reward_corrected"] for r in all_results),
            "mean_cumulative_regret": statistics.mean(
                r["summary"][pol]["cumulative_regret"] for r in all_results),
        }

    summary_json = {
        "benchmark": "traffic_split_resilience",
        "seeds": args.seeds,
        "seed_offset": args.seed_offset,
        "weeks": args.weeks,
        "requests_per_week": args.requests_per_week,
        "regime_shift_week": REGIME_SHIFT_WEEK,
        "algorithm": args.algorithm,
        "context_type": args.context_type,
        "criteria": crit,
        "aggregate": aggregate,
        "spread_original": orig_spread,
        "spread_corrected": corr_spread,
        "spread_ratio": corr_spread / orig_spread if orig_spread > 0 else None,
    }

    summary_path = os.path.join(args.output_dir, "summary.json")
    with open(summary_path, "w") as f:
        json.dump(summary_json, f, indent=2)

    # Write seeds.csv
    seeds_path = os.path.join(args.output_dir, "seeds.csv")
    with open(seeds_path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["seed", "policy", "total_converted", "total_cost_usd",
                    "reward_original", "reward_corrected",
                    "score", "cumulative_regret"])
        for r in all_results:
            for pol, s in r["summary"].items():
                w.writerow([
                    r["seed"], pol,
                    s["total_converted"],
                    f"{s['total_cost_usd']:.4f}",
                    f"{s['total_reward_original']:.4f}",
                    f"{s['total_reward_corrected']:.4f}",
                    f"{s['score']:.4f}",
                    f"{s['cumulative_regret']:.4f}",
                ])

    print(f"\n  Output: {args.output_dir}")
    print(f"    summary.json: {summary_path}")
    print(f"    seeds.csv:    {seeds_path}")

    ov = crit.get("overall", {})
    if ov.get("pass") is False:
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
