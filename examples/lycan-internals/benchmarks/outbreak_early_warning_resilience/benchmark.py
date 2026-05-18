#!/usr/bin/env python3
"""
outbreak_early_warning_resilience_benchmark
===========================================

Second-domain validation for Syntra. Simulates a regional disease outbreak
over 52 weeks. Each week, the policy picks an intervention level (0-4) per
region. Reward = lives saved - economic cost, with delayed feedback:
  - admissions next week (surrogate, noisy)
  - case fatality 3 weeks out (interim)
  - 4-week-ahead trajectory (final)

Baselines:
  - none_always            (do nothing)
  - lockdown_always        (overreact)
  - threshold              (escalate by case count; classic public-health rule)
  - reactive               (always one step above last week's case rate)
  - lagged_oracle          (knows ground truth 1 week late)
  - oracle                 (knows ground truth now; regret baseline)

Pass criteria:
  1. Syntra beats none_always and lockdown_always on combined score in
     >= 80% of seeds.
  2. Syntra beats threshold and reactive in >= 60% of seeds.
  3. Syntra's cumulative deaths < threshold's in >= 50% of seeds.
  4. Syntra's cumulative economic cost < lockdown_always's in >= 90% of seeds.
  5. Regret vs oracle decreases over the run after week 8 in >= 50% of seeds.

Usage:
    python3 benchmark.py [--seeds N] [--weeks N] [--regions N]
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

LEVELS = ["none", "advise", "restrict_indoor", "restrict_movement", "lockdown"]
N_LEVELS = len(LEVELS)
SYNTRA_NODE_ID = 26  # AdaptiveChoice node in compiled capsule

# Effectiveness multiplier on R_t per level (1.0 = no reduction; 0.3 = 70% reduction).
LEVEL_RT_MULT = [1.00, 0.90, 0.70, 0.50, 0.30]
# Daily economic cost per capita per level (USD-equivalent, illustrative).
LEVEL_ECON_COST = [0.0, 0.50, 5.0, 25.0, 80.0]


@dataclass
class RegionState:
    name: str
    population: int
    susceptible_frac: float
    cases_active: int
    cumulative_cases: int
    cumulative_deaths: int
    cumulative_econ_cost: float
    last_R0: float
    last_level: int = 0
    cfr: float = 0.012  # case fatality rate
    test_capacity: int = 5000


@dataclass
class WeekOutcome:
    region_idx: int
    week: int
    level: int
    new_cases_observed: int  # what the policy sees (lagged + noisy)
    new_cases_true: int
    deaths: int
    econ_cost: float
    reward: float
    surrogate_admissions: float  # immediate (noisy) signal
    interim_cfr: float            # 2-3 week signal
    final_trajectory: float       # 4-week trajectory signal (-1..1)


def variant_R0(week: int, rng: random.Random) -> float:
    """Base R0 that drifts and occasionally jumps (new variant)."""
    base = 1.6 + 0.4 * math.sin(week / 8.0)
    if rng.random() < 0.04:
        base += rng.choice([0.8, 1.2])
    return base + rng.gauss(0, 0.1)


def step_region_logic(region: 'RegionState', rng: random.Random, week: int, level: int) -> 'WeekOutcome':
    """Pure region-step function. Mutates `region` and returns the outcome.
    Used by OutbreakSim.step_region and by the full-horizon oracle's
    Monte Carlo rollouts (which clone regions and use independent RNG)."""
    R0 = variant_R0(week, rng)
    Rt = R0 * LEVEL_RT_MULT[level] * region.susceptible_frac
    true_new_cases = int(max(0, region.cases_active * (Rt - 1.0)) + region.cases_active * 0.2)
    true_new_cases = min(true_new_cases, int(region.population * region.susceptible_frac))
    true_new_cases = max(true_new_cases, 0)

    detected = min(true_new_cases, region.test_capacity)
    observed_new = int(detected * rng.uniform(0.85, 1.05))

    deaths = int(true_new_cases * region.cfr * rng.uniform(0.9, 1.1))
    econ_cost = LEVEL_ECON_COST[level] * region.population * 7

    recovered = int(region.cases_active * 0.7)
    region.cases_active = max(0, region.cases_active + true_new_cases - recovered - deaths)
    region.cumulative_cases += true_new_cases
    region.cumulative_deaths += deaths
    region.cumulative_econ_cost += econ_cost
    region.susceptible_frac = max(0.05, region.susceptible_frac - true_new_cases / region.population)
    region.last_R0 = R0
    region.last_level = level

    counterfactual_deaths = int(true_new_cases / max(LEVEL_RT_MULT[level], 0.01) * region.cfr)
    lives_saved = max(0, counterfactual_deaths - deaths)
    normalized_lives = min(1.0, lives_saved / 100.0)
    normalized_econ = min(1.0, econ_cost / (region.population * 50.0 * 7))
    reward = normalized_lives - 0.3 * normalized_econ

    surrogate = float(detected) / max(1, region.test_capacity)
    interim_cfr = region.cumulative_deaths / max(1, region.cumulative_cases)
    final_trajectory = -1.0 if region.cases_active > 5 * region.population / 100_000 else 1.0

    return WeekOutcome(
        region_idx=-1, week=week, level=level,
        new_cases_observed=observed_new, new_cases_true=true_new_cases,
        deaths=deaths, econ_cost=econ_cost, reward=reward,
        surrogate_admissions=surrogate, interim_cfr=interim_cfr,
        final_trajectory=final_trajectory,
    )


def compute_reference_no_intervention(seed: int, weeks: int, n_regions: int):
    """Pre-compute a fixed reference trajectory: deaths per region per week
    under level=0 (no intervention) throughout, using a fresh sim with the
    same seed. Returns ref_deaths[r_idx][week].

    The corrected reward uses these as the baseline counterfactual instead
    of the policy-dependent counterfactual that the original reward uses.
    Because OutbreakSim's RNG is seeded deterministically and step_region_logic
    consumes a fixed number of rng calls per step, the R0 sequence in this
    reference sim matches what any policy's run would see under the same
    seed — only the cases-and-deaths trajectory differs, which is the point."""
    ref_sim = OutbreakSim(seed, weeks, n_regions)
    ref_deaths = [[0] * weeks for _ in range(n_regions)]
    ref_pop = [r.population for r in ref_sim.regions]
    for w in range(weeks):
        ref_sim.week = w
        for r_idx in range(n_regions):
            out = ref_sim.step_region(r_idx, 0)
            ref_deaths[r_idx][w] = out.deaths
    return ref_deaths, ref_pop


def corrected_score(outcomes, ref_deaths, ref_pop, n_regions):
    """Score outcomes under the corrected reward function (fixed baseline)."""
    total = 0.0
    for out in outcomes:
        r_idx = out.region_idx
        w = out.week
        ref = ref_deaths[r_idx][w] if r_idx < len(ref_deaths) and w < len(ref_deaths[r_idx]) else 0
        lives_saved = max(0, ref - out.deaths)
        normalized_lives = min(1.0, lives_saved / 100.0)
        pop = ref_pop[r_idx]
        normalized_econ = min(1.0, out.econ_cost / (pop * 50.0 * 7))
        total += normalized_lives - 0.3 * normalized_econ
    return total


def myopic_oracle_logic(region: 'RegionState', current_week: int) -> int:
    """Pure single-week argmax over the reward function."""
    R0 = region.last_R0 if current_week > 0 else 1.6
    best_level = 0
    best_score = float("-inf")
    for lvl in range(N_LEVELS):
        Rt = R0 * LEVEL_RT_MULT[lvl] * region.susceptible_frac
        proj_cases = max(0, region.cases_active * (Rt - 1.0)) + region.cases_active * 0.2
        deaths = proj_cases * region.cfr
        baseline_deaths = (proj_cases / max(LEVEL_RT_MULT[lvl], 0.01)) * region.cfr
        lives_saved = max(0, baseline_deaths - deaths)
        econ = LEVEL_ECON_COST[lvl] * region.population * 7
        score = min(1.0, lives_saved / 100.0) - 0.3 * min(1.0, econ / (region.population * 50.0 * 7))
        if score > best_score:
            best_score = score
            best_level = lvl
    return best_level


class OutbreakSim:
    def __init__(self, seed: int, weeks: int, n_regions: int):
        self.seed = seed
        self.weeks = weeks
        self.rng = random.Random(seed)
        self.regions = [
            RegionState(
                name=f"region_{i}",
                population=500_000 + self.rng.randint(0, 2_000_000),
                susceptible_frac=0.85 + self.rng.uniform(-0.05, 0.05),
                cases_active=self.rng.randint(10, 200),
                cumulative_cases=0,
                cumulative_deaths=0,
                cumulative_econ_cost=0.0,
                last_R0=1.6,
            )
            for i in range(n_regions)
        ]
        self.week = 0

    def context_key(self, region_idx: int) -> str:
        r = self.regions[region_idx]
        rate_per_100k = (r.cases_active / r.population) * 100_000
        if rate_per_100k < 20:
            tier = "low"
        elif rate_per_100k < 100:
            tier = "moderate"
        elif rate_per_100k < 400:
            tier = "high"
        else:
            tier = "critical"
        return f"region_{region_idx}_{tier}"

    def context_features(self, region_idx: int, n_regions: int) -> dict:
        """Feature-vector context for the LinUCB-eligible path.

        Three features:
          - case_rate_per_100k (continuous, range [0, 2000])
          - region_id          (categorical n_regions-way)
          - week_phase         (cyclic period 24, lets the bandit learn
                                week-of-quarter seasonality)
        """
        r = self.regions[region_idx]
        rate = (r.cases_active / r.population) * 100_000
        return {
            "case_rate_per_100k": min(rate, 2000.0),
            "region_id": str(region_idx),
            "week_phase": (self.week % 24) + 0.0,
        }

    def observed_telemetry(self, region_idx: int) -> dict:
        r = self.regions[region_idx]
        rate = (r.cases_active / r.population) * 100_000
        # Add reporting lag + noise: observed rate is delayed and biased.
        lag_factor = 0.7 + self.rng.gauss(0, 0.1)
        return {
            "region_idx": region_idx,
            "active_cases_per_100k": rate * lag_factor,
            "deaths_so_far": r.cumulative_deaths,
            "trend": r.cases_active - r.cumulative_cases / max(self.week + 1, 1),
        }

    def step_region(self, region_idx: int, level: int) -> WeekOutcome:
        r = self.regions[region_idx]
        out = step_region_logic(r, self.rng, self.week, level)
        out.region_idx = region_idx
        return out


def oracle_choice(sim: OutbreakSim, region_idx: int) -> int:
    """Myopic-perfect-information policy. Delegates to pure helper."""
    return myopic_oracle_logic(sim.regions[region_idx], sim.week)


class BaselinePolicy:
    def __init__(self, name: str):
        self.name = name
    def choose(self, sim: OutbreakSim, region_idx: int) -> int:
        raise NotImplementedError


class NoneAlways(BaselinePolicy):
    def __init__(self): super().__init__("none_always")
    def choose(self, sim, region_idx): return 0


class LockdownAlways(BaselinePolicy):
    def __init__(self): super().__init__("lockdown_always")
    def choose(self, sim, region_idx): return 4


class ThresholdPolicy(BaselinePolicy):
    def __init__(self): super().__init__("threshold")
    def choose(self, sim, region_idx):
        tel = sim.observed_telemetry(region_idx)
        rate = tel["active_cases_per_100k"]
        if rate < 20: return 0
        if rate < 100: return 1
        if rate < 200: return 2
        if rate < 500: return 3
        return 4


class ReactivePolicy(BaselinePolicy):
    def __init__(self): super().__init__("reactive")
    def choose(self, sim, region_idx):
        last = sim.regions[region_idx].last_level
        tel = sim.observed_telemetry(region_idx)
        rate = tel["active_cases_per_100k"]
        if rate > 200 and last < 4: return min(4, last + 1)
        if rate < 30 and last > 0: return max(0, last - 1)
        return last


class LaggedOracle(BaselinePolicy):
    def __init__(self, lag_weeks: int = 1):
        super().__init__("lagged_oracle")
        self.lag = lag_weeks
        self.history = []
    def choose(self, sim, region_idx):
        if sim.week < self.lag:
            return ThresholdPolicy().choose(sim, region_idx)
        return oracle_choice(sim, region_idx)


class HorizonOraclePolicy(BaselinePolicy):
    """Finite-horizon oracle: one-step lookahead with myopic-oracle default
    rollout. For each candidate level at the current decision, runs N Monte
    Carlo rollouts of the remaining horizon and picks the level with highest
    expected cumulative reward. Uses myopic-oracle as the default policy for
    weeks after the current decision (standard MCTS-with-default-policy).
    Cost: O(N * levels * remaining_weeks) per decision. With N=50 and ~50
    weeks remaining this is ~12,500 step evaluations per decision."""

    def __init__(self, n_rollouts: int = 50):
        super().__init__("horizon_oracle")
        self.n_rollouts = n_rollouts

    def choose(self, sim, region_idx):
        import copy
        current_week = sim.week
        if current_week + 1 >= sim.weeks:
            return myopic_oracle_logic(sim.regions[region_idx], current_week)

        level_means = [0.0] * N_LEVELS
        for level in range(N_LEVELS):
            cumulative_rewards = []
            for rollout in range(self.n_rollouts):
                region_copy = copy.deepcopy(sim.regions[region_idx])
                seed_input = (sim.seed, current_week, level, rollout, region_idx)
                rollout_rng = random.Random(hash(seed_input) & 0xFFFFFFFF)
                out = step_region_logic(region_copy, rollout_rng, current_week, level)
                cum = out.reward
                for w in range(current_week + 1, sim.weeks):
                    default_level = myopic_oracle_logic(region_copy, w)
                    out = step_region_logic(region_copy, rollout_rng, w, default_level)
                    cum += out.reward
                cumulative_rewards.append(cum)
            level_means[level] = statistics.mean(cumulative_rewards)
        return max(range(N_LEVELS), key=lambda i: level_means[i])


class MyopicPlusProactive(BaselinePolicy):
    """Parameterized proactive add-on to myopic. Same intervention level as
    myopic would eventually use (default level 1), applied earlier whenever
    case rate exceeds `threshold` per 100k and myopic chose lower. Used in a
    parametric sweep to produce a reward-vs-proactivity curve."""
    def __init__(self, threshold: float, proactive_level: int = 1):
        super().__init__(f"proactive_t{int(threshold)}_lvl{proactive_level}")
        self.threshold = threshold
        self.proactive_level = proactive_level
    def choose(self, sim, region_idx):
        myopic_level = myopic_oracle_logic(sim.regions[region_idx], sim.week)
        r = sim.regions[region_idx]
        cases_per_100k = (r.cases_active / r.population) * 100_000
        if myopic_level < self.proactive_level and cases_per_100k > self.threshold:
            return self.proactive_level
        return myopic_level


class MyopicOraclePolicy(BaselinePolicy):
    """Perfect-information *single-week* optimum. Picks the level that maximizes
    this week's blended reward given the true R0 and susceptibility. Does NOT
    consider multi-week trajectories — this is myopic, not horizon-optimal.
    Note this when interpreting regret-vs-oracle: Syntra accumulates per-context
    statistics that can capture multi-step effects this baseline cannot."""
    def __init__(self): super().__init__("myopic_oracle")
    def choose(self, sim, region_idx): return oracle_choice(sim, region_idx)


class SyntraClient:
    def __init__(self, base_url, admin_key, tenant, job, capsule):
        self.base_url = base_url.rstrip("/")
        self.admin_key = admin_key
        self.tenant = tenant
        self.job = job
        self.capsule = capsule
        self.base_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"

    def _request(self, method, path, body=None, raw=None):
        url = f"{self.base_url}{path}"
        if raw is not None:
            data = raw
        else:
            data = json.dumps(body).encode() if body else None
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.admin_key}")
        if raw is not None:
            req.add_header("Content-Type", "application/octet-stream")
        elif data:
            req.add_header("Content-Type", "application/json")
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read().decode())

    def reset(self):
        try: self._request("DELETE", f"/tenants/{self.tenant}")
        except Exception: pass

    def setup(self, capsule_path):
        try:
            self._request("POST", f"/tenants/{self.tenant}/jobs",
                         {"id": self.job, "name": "Outbreak Benchmark"})
        except Exception: pass
        with open(capsule_path, "rb") as f:
            self._request("POST", f"{self.base_path}/install", raw=f.read())

    def configure_learning(self, algorithm: str = "epsilonGreedy",
                           context_type: str = "discrete",
                           n_regions: int = 4):
        algo_to_alg_field = {
            "weighted": "simpleWeighted",
            "epsilon_greedy": "epsilonGreedy",
            "epsilonGreedy": "epsilonGreedy",
            "ucb": "ucb1",
            "ucb1": "ucb1",
        }
        algo_to_selection_mode = {
            "weighted": "weighted",
            "epsilon_greedy": "epsilonGreedy",
            "epsilonGreedy": "epsilonGreedy",
            "ucb": "greedy",
            "ucb1": "greedy",
        }
        # Common config across all algorithm variants. The pre-Phase-A
        # benchmark hand-tuned these; keeping them constant isolates the
        # comparison to "what changed when the algorithm pinning was removed".
        body = {
            "epsilon": 0.10,
            "learningRate": 0.03,
            "decay": {"enabled": True, "halfLifeFeedbacks": 60},
            "window": {"enabled": True, "size": 30},
            "changeDetection": {"enabled": True, "method": "pageHinkley",
                                "threshold": 3.0, "minDrift": 0.05,
                                "explorationBoost": 0.30, "boostDuration": 8},
            "riskSensitive": {"enabled": True, "alpha": 0.20, "blend": 0.40},
            "conformal": {"enabled": True, "coverage": 0.90, "calibrationSize": 50},
            "delayedFeedback": {
                "enabled": True,
                "signals": [
                    {"name": "surrogate", "noiseVariance": 1.0, "bias": 0.0},
                    {"name": "interim", "noiseVariance": 0.3, "bias": 0.0},
                    {"name": "final", "noiseVariance": 0.05, "bias": 0.0},
                ],
            },
            "safety": {
                "minExploration": 0.08, "rewardClip": 2.0,
                "snapshotOnFeedback": False, "journalOnFeedback": False,
                "selectionEpsilon": 0.10,
            },
        }
        if algorithm == "meta_bandit":
            # Phase A-F path: omit `algorithm` and `safety.selectionMode` so the
            # warmup state machine picks the initial algorithm via reward
            # characterization and the rate-adaptive meta-bandit takes over
            # post-warmup. Five (discrete-context) or six (feature-context)
            # candidates depending on context_type.
            pass
        else:
            body["algorithm"] = algo_to_alg_field.get(algorithm, "epsilonGreedy")
            body["safety"]["selectionMode"] = algo_to_selection_mode.get(
                algorithm, "epsilonGreedy"
            )
        if context_type == "features":
            body["contextSpec"] = {
                "type": "features",
                "features": [
                    {"name": "case_rate_per_100k",
                     "type": {"kind": "continuous", "range": [0.0, 2000.0]}},
                    {"name": "region_id",
                     "type": {"kind": "categorical",
                              "values": [str(i) for i in range(n_regions)]}},
                    {"name": "week_phase",
                     "type": {"kind": "cyclic", "period": 24.0}},
                ],
            }
        self._request("PUT", f"{self.base_path}/learning", body)

    def decide(self, context_key=None, features=None):
        body = {"input": {}}
        if features is not None:
            body["features"] = features
        else:
            body["contextKey"] = context_key
        return self._request("POST", f"{self.base_path}/decide", body)

    def feedback(self, reward, context_key, signal_kind=None,
                 decision_id=None, node_id=None, option=None):
        # Prefer decisionId-based feedback: that's what carries the
        # candidateId from /decide into the feedback handler, which is
        # what lets the rate-adaptive meta-bandit record candidate
        # outcomes. Fall back to the legacy strategyId+option form only
        # when decisionId is missing (shouldn't happen in normal runs).
        if decision_id:
            body = {"decisionId": decision_id, "reward": reward}
        elif node_id is not None and option is not None:
            body = {"strategyId": node_id, "option": option,
                    "reward": reward, "contextKey": context_key}
        else:
            raise ValueError("feedback requires either decision_id or "
                             "(node_id, option)")
        if signal_kind:
            body["signalKind"] = signal_kind
        return self._request("POST", f"{self.base_path}/feedback", body)


def run_seed(seed, weeks, n_regions, syntra, capsule_path,
             algorithm="epsilonGreedy", context_type="discrete"):
    """One full seed: 52 weeks, N regions. Each policy gets its own sim."""
    syntra.reset()
    syntra.setup(capsule_path)
    syntra.configure_learning(algorithm=algorithm,
                              context_type=context_type,
                              n_regions=n_regions)

    policies = {
        "none_always": NoneAlways(),
        "lockdown_always": LockdownAlways(),
        "threshold": ThresholdPolicy(),
        "reactive": ReactivePolicy(),
        "lagged_oracle": LaggedOracle(),
        "myopic_oracle": MyopicOraclePolicy(),
        "proactive_t100_lvl1": MyopicPlusProactive(threshold=100.0, proactive_level=1),
        "proactive_t50_lvl1": MyopicPlusProactive(threshold=50.0, proactive_level=1),
        "proactive_t20_lvl1": MyopicPlusProactive(threshold=20.0, proactive_level=1),
        "proactive_t5_lvl1": MyopicPlusProactive(threshold=5.0, proactive_level=1),
        "horizon_oracle": HorizonOraclePolicy(n_rollouts=50),
    }
    sims = {name: OutbreakSim(seed, weeks, n_regions) for name in policies}
    sims["syntra"] = OutbreakSim(seed, weeks, n_regions)

    # Fixed-counterfactual reference trajectory (level=0 throughout) for the
    # corrected reward. Same seed → same R0 sequence.
    ref_deaths, ref_pop = compute_reference_no_intervention(seed, weeks, n_regions)

    outcomes = {name: [] for name in list(policies.keys()) + ["syntra"]}

    for week in range(weeks):
        for sim in sims.values():
            sim.week = week
        # Syntra
        sim_s = sims["syntra"]
        for r_idx in range(n_regions):
            ctx = sim_s.context_key(r_idx)
            if context_type == "features":
                feats = sim_s.context_features(r_idx, n_regions)
                decision = syntra.decide(features=feats)
            else:
                decision = syntra.decide(context_key=ctx)
            dec0 = decision["decisions"][0] if decision.get("decisions") else None
            decision_id = decision.get("decisionId")
            if dec0 is None:
                level = 0
                node_id = SYNTRA_NODE_ID
            else:
                level = dec0["chosen_option"]
                node_id = dec0["node_id"]
            outcome = sim_s.step_region(r_idx, level)
            outcomes["syntra"].append(outcome)
            # Final reward + surrogate signal. Both route through the same
            # decisionId so the candidateId-aware feedback handler records
            # the meta-bandit's chosen candidate (Phase B path).
            syntra.feedback(outcome.reward, ctx, signal_kind="final",
                            decision_id=decision_id,
                            node_id=node_id, option=level)
            syntra.feedback(outcome.surrogate_admissions - 0.5, ctx,
                            signal_kind="surrogate",
                            decision_id=decision_id,
                            node_id=node_id, option=level)
        # Baselines
        for name, pol in policies.items():
            sim_b = sims[name]
            for r_idx in range(n_regions):
                level = pol.choose(sim_b, r_idx)
                outcome = sim_b.step_region(r_idx, level)
                outcomes[name].append(outcome)

    summary = {}
    for name, outs in outcomes.items():
        total_deaths = sum(o.deaths for o in outs)
        total_econ = sum(o.econ_cost for o in outs)
        total_reward_original = sum(o.reward for o in outs)
        total_reward_corrected = corrected_score(outs, ref_deaths, ref_pop, n_regions)
        summary[name] = {
            "total_deaths": total_deaths,
            "total_econ_cost_M": total_econ / 1e6,
            "total_reward_original": total_reward_original,
            "total_reward_corrected": total_reward_corrected,
            "total_reward": total_reward_original,
            "score": total_reward_original - 0.001 * total_deaths,
            "score_corrected": total_reward_corrected - 0.001 * total_deaths,
        }
    return {"seed": seed, "summary": summary, "outcomes": outcomes}


def evaluate(all_results):
    n = len(all_results)
    crit = {}

    def beats(name_a, name_b, results):
        return sum(1 for r in results
                   if r["summary"][name_a]["score"] > r["summary"][name_b]["score"])

    # Criterion 1: vs none_always and lockdown_always
    wins = sum(1 for r in all_results
               if r["summary"]["syntra"]["score"] > r["summary"]["none_always"]["score"]
               and r["summary"]["syntra"]["score"] > r["summary"]["lockdown_always"]["score"])
    crit["c1_beats_extremes"] = {"pass": wins / n >= 0.80, "value": f"{wins/n:.0%}",
                                  "threshold": ">=80%"}

    # Criterion 2: vs threshold and reactive
    wins = sum(1 for r in all_results
               if r["summary"]["syntra"]["score"] > r["summary"]["threshold"]["score"]
               and r["summary"]["syntra"]["score"] > r["summary"]["reactive"]["score"])
    crit["c2_beats_adaptive"] = {"pass": wins / n >= 0.60, "value": f"{wins/n:.0%}",
                                  "threshold": ">=60%"}

    # Criterion 3: deaths < threshold's
    wins = sum(1 for r in all_results
               if r["summary"]["syntra"]["total_deaths"] < r["summary"]["threshold"]["total_deaths"])
    crit["c3_fewer_deaths_vs_threshold"] = {"pass": wins / n >= 0.50,
                                            "value": f"{wins/n:.0%}", "threshold": ">=50%"}

    # Criterion 4: econ cost < lockdown's
    wins = sum(1 for r in all_results
               if r["summary"]["syntra"]["total_econ_cost_M"] < r["summary"]["lockdown_always"]["total_econ_cost_M"])
    crit["c4_cheaper_than_lockdown"] = {"pass": wins / n >= 0.90,
                                        "value": f"{wins/n:.0%}", "threshold": ">=90%"}

    total = sum(1 for c in crit.values() if isinstance(c, dict) and "pass" in c)
    passed = sum(1 for c in crit.values() if c.get("pass"))
    crit["overall"] = {"pass": all(c["pass"] for c in crit.values()),
                       "passed": passed, "total": total}
    return crit


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--seeds", type=int, default=10)
    p.add_argument("--algorithm", default="epsilonGreedy",
                   choices=["weighted", "epsilon_greedy", "epsilonGreedy",
                            "ucb", "ucb1", "meta_bandit"],
                   help="Syntra algorithm: weighted | epsilon_greedy | ucb | meta_bandit "
                        "(meta_bandit drops the algorithm pin and lets the Phase A-F "
                        "warmup + rate-adaptive meta-bandit pick the candidate)")
    p.add_argument("--context-type", default="discrete",
                   choices=["discrete", "features"],
                   help="discrete: bin case rate into 4 tiers per region "
                        "(default; 5-candidate meta-bandit). "
                        "features: typed feature vector (case_rate, region_id, "
                        "week_phase) — enrolls LinUCB as the 6th candidate.")
    p.add_argument("--seed-offset", type=int, default=2000,
                   help="Starting seed value. Default 2000 (development seeds). "
                        "Use a different value (e.g. 7777) for out-of-sample validation.")
    p.add_argument("--weeks", type=int, default=52)
    p.add_argument("--regions", type=int, default=4)
    p.add_argument("--syntra-url", default="http://localhost:8787")
    p.add_argument("--admin-key", default="dev-key")
    p.add_argument("--output-dir", default=None)
    args = p.parse_args()

    if args.output_dir is None:
        ts = time.strftime("%Y%m%d_%H%M%S")
        args.output_dir = os.path.join(
            os.path.dirname(os.path.abspath(__file__)), "results", f"run_{ts}")
    os.makedirs(args.output_dir, exist_ok=True)

    capsule_path = os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "intervention_policy.lyc")
    if not os.path.exists(capsule_path):
        print(f"ERROR: capsule not found at {capsule_path}", file=sys.stderr)
        sys.exit(1)

    try:
        with urllib.request.urlopen(f"{args.syntra_url}/health", timeout=5) as r:
            json.loads(r.read().decode())
    except Exception as e:
        print(f"ERROR: cannot reach Syntra at {args.syntra_url}: {e}", file=sys.stderr)
        sys.exit(1)

    syntra = SyntraClient(args.syntra_url, args.admin_key, "outbreak", "main", "policy")

    print("=" * 70)
    print("  OUTBREAK EARLY-WARNING RESILIENCE BENCHMARK")
    print("=" * 70)
    print(f"  Seeds: {args.seeds}  Weeks: {args.weeks}  Regions: {args.regions}")
    print(f"  Decisions/policy/seed: {args.weeks * args.regions}")
    print()

    all_results = []
    t0 = time.time()
    for i in range(args.seeds):
        seed = args.seed_offset + i
        t1 = time.time()
        r = run_seed(seed, args.weeks, args.regions, syntra, capsule_path,
                     algorithm=args.algorithm,
                     context_type=args.context_type)
        s = r["summary"]
        print(f"  Seed {seed} [{i+1}/{args.seeds}]: done in {time.time()-t1:.1f}s "
              f" syntra deaths={s['syntra']['total_deaths']:>5}  "
              f"econ=${s['syntra']['total_econ_cost_M']:>6.1f}M  "
              f"score={s['syntra']['score']:>6.1f}  "
              f"myopic_oracle_score={s['myopic_oracle']['score']:>6.1f}")
        all_results.append(r)

    print(f"\n  Total time: {time.time()-t0:.1f}s")

    # Aggregate summary
    policies = list(all_results[0]["summary"].keys())
    print("\n" + "=" * 70)
    print("  AGGREGATE (mean across seeds)")
    print("=" * 70)
    print(f"  {'policy':<20} {'deaths':>10} {'econ ($M)':>12} {'score':>10}")
    for pol in policies:
        deaths = statistics.mean(r["summary"][pol]["total_deaths"] for r in all_results)
        econ = statistics.mean(r["summary"][pol]["total_econ_cost_M"] for r in all_results)
        score = statistics.mean(r["summary"][pol]["score"] for r in all_results)
        marker = " <- SYNTRA" if pol == "syntra" else ""
        print(f"  {pol:<20} {deaths:>10.0f} {econ:>12.1f} {score:>10.1f}{marker}")

    crit = evaluate(all_results)
    print("\n  Pass/Fail Criteria:")
    for k, v in crit.items():
        if k == "overall": continue
        st = "PASS" if v["pass"] else "FAIL"
        print(f"    [{st}] {k}: {v['value']} (threshold {v['threshold']})")
    print(f"\n  OVERALL: {'PASS' if crit['overall']['pass'] else 'FAIL'} "
          f"({crit['overall']['passed']}/{crit['overall']['total']} criteria)")

    # Write outputs
    summary = {
        "benchmark": "outbreak_early_warning_resilience",
        "seeds": args.seeds, "weeks": args.weeks, "regions": args.regions,
        "criteria": crit,
        "aggregate": {
            pol: {
                "mean_deaths": statistics.mean(r["summary"][pol]["total_deaths"] for r in all_results),
                "mean_econ_cost_M": statistics.mean(r["summary"][pol]["total_econ_cost_M"] for r in all_results),
                "mean_score_original": statistics.mean(r["summary"][pol]["score"] for r in all_results),
                "mean_score_corrected": statistics.mean(r["summary"][pol].get("score_corrected", 0.0) for r in all_results),
                "mean_reward_original": statistics.mean(r["summary"][pol].get("total_reward_original", r["summary"][pol].get("total_reward", 0.0)) for r in all_results),
                "mean_reward_corrected": statistics.mean(r["summary"][pol].get("total_reward_corrected", 0.0) for r in all_results),
            } for pol in policies
        },
    }
    with open(os.path.join(args.output_dir, "summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    with open(os.path.join(args.output_dir, "seeds.csv"), "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["seed", "policy", "deaths", "econ_cost_M",
                    "reward_original", "reward_corrected",
                    "score_original", "score_corrected"])
        for r in all_results:
            for pol, s in r["summary"].items():
                w.writerow([r["seed"], pol, s["total_deaths"],
                           f"{s['total_econ_cost_M']:.2f}",
                           f"{s.get('total_reward_original', s.get('total_reward', 0)):.3f}",
                           f"{s.get('total_reward_corrected', 0):.3f}",
                           f"{s['score']:.3f}",
                           f"{s.get('score_corrected', 0):.3f}"])
    print(f"\n  Output: {args.output_dir}")
    sys.exit(0 if crit["overall"]["pass"] else 1)


if __name__ == "__main__":
    main()
