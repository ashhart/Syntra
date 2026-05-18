#!/usr/bin/env python3
"""
vaccine_allocation_resilience_benchmark
=======================================

Second-domain test of the reward-misspecification pattern found in the
outbreak benchmark. Same simulator skeleton (4 regions, 52 weeks, R₀
trajectory), but the action space is "allocate a weekly national vaccine
supply across regions" instead of "pick an intervention level."

Five hand-coded allocation policies + two scoring functions (original
policy-dependent counterfactual; corrected fixed-baseline counterfactual).
Pre-registration before run; honest reporting after.

Usage:
    python3 benchmark.py [--seeds N] [--seed-offset N] [--weeks N] [--regions N]
"""

import argparse
import copy
import csv
import json
import math
import os
import random
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field

N_REGIONS_DEFAULT = 4
N_WEEKS_DEFAULT = 52

VACCINE_EFFICACY = 0.85
VACCINATION_LAG_WEEKS = 2
COST_PER_DOSE_USD = 25.0


def weekly_supply(week: int) -> int:
    if week < 12:
        return int(50_000 * week / 12)
    return 50_000


@dataclass
class RegionState:
    name: str
    population: int
    susceptible: int
    cases_active: int
    recovered: int
    cumulative_cases: int
    cumulative_deaths: int
    cumulative_doses_received: int
    last_R0: float
    pending_doses: list = field(default_factory=list)  # list of (week_administered, count)
    cfr: float = 0.012


@dataclass
class WeekOutcome:
    region_idx: int
    week: int
    doses_allocated: int
    new_cases_true: int
    deaths: int
    vaccine_cost: float
    reward: float


def variant_R0(week: int, rng: random.Random) -> float:
    base = 1.6 + 0.4 * math.sin(week / 8.0)
    if rng.random() < 0.04:
        base += rng.choice([0.8, 1.2])
    return base + rng.gauss(0, 0.1)


def step_region_logic(region: RegionState, rng: random.Random, week: int,
                       doses: int) -> WeekOutcome:
    """Pure region-step under a vaccine allocation. Mutates region."""
    R0 = variant_R0(week, rng)

    # Activate pending doses that have completed the 2-week lag.
    activated = 0
    new_pending = []
    for (w_admin, count) in region.pending_doses:
        if week - w_admin >= VACCINATION_LAG_WEEKS:
            effective = int(count * VACCINE_EFFICACY)
            effective = min(effective, region.susceptible)
            region.susceptible -= effective
            region.recovered += effective
            activated += effective
        else:
            new_pending.append((w_admin, count))
    region.pending_doses = new_pending

    # Add this week's allocated doses to the pending queue (subject to
    # susceptible-supply cap so we don't dose more than is plausible).
    administered = min(doses, region.susceptible)
    region.pending_doses.append((week, administered))
    region.cumulative_doses_received += administered

    # Transmission dynamics: Rt depends on susceptible fraction.
    s_frac = region.susceptible / region.population
    Rt = R0 * s_frac
    true_new_cases = int(max(0, region.cases_active * (Rt - 1.0)) + region.cases_active * 0.2)
    true_new_cases = min(true_new_cases, region.susceptible)
    true_new_cases = max(true_new_cases, 0)

    deaths = int(true_new_cases * region.cfr * rng.uniform(0.9, 1.1))
    recovered_this_week = int(region.cases_active * 0.7)

    region.cases_active = max(0, region.cases_active + true_new_cases - recovered_this_week - deaths)
    region.cumulative_cases += true_new_cases
    region.cumulative_deaths += deaths
    region.susceptible = max(0, region.susceptible - true_new_cases)
    region.recovered += recovered_this_week
    region.last_R0 = R0

    vaccine_cost = administered * COST_PER_DOSE_USD

    # ORIGINAL reward — policy-dependent counterfactual baseline.
    # The pathology: counterfactual_deaths is computed from this week's
    # cases_active, which itself depends on past allocation choices.
    projected_no_vac_Rt = R0 * (region.susceptible + activated) / region.population
    projected_no_vac_cases = max(0, region.cases_active * (projected_no_vac_Rt - 1.0)) + region.cases_active * 0.2
    counterfactual_deaths = projected_no_vac_cases * region.cfr
    lives_saved = max(0, counterfactual_deaths - deaths)
    normalized_lives = min(1.0, lives_saved / 100.0)
    normalized_cost = min(1.0, vaccine_cost / (weekly_supply(week) * COST_PER_DOSE_USD + 1.0))
    reward = normalized_lives - 0.3 * normalized_cost

    return WeekOutcome(
        region_idx=-1, week=week, doses_allocated=doses,
        new_cases_true=true_new_cases, deaths=deaths,
        vaccine_cost=vaccine_cost, reward=reward,
    )


class VaccineSim:
    def __init__(self, seed: int, weeks: int, n_regions: int):
        self.seed = seed
        self.weeks = weeks
        self.rng = random.Random(seed)
        self.regions = []
        for i in range(n_regions):
            pop = 500_000 + self.rng.randint(0, 2_000_000)
            init_active = self.rng.randint(10, 200)
            s_frac = 0.85 + self.rng.uniform(-0.05, 0.05)
            susc = int(pop * s_frac)
            self.regions.append(RegionState(
                name=f"region_{i}",
                population=pop,
                susceptible=susc,
                cases_active=init_active,
                recovered=pop - susc - init_active,
                cumulative_cases=0,
                cumulative_deaths=0,
                cumulative_doses_received=0,
                last_R0=1.6,
            ))
        self.week = 0

    def step_region(self, region_idx: int, doses: int) -> WeekOutcome:
        r = self.regions[region_idx]
        out = step_region_logic(r, self.rng, self.week, doses)
        out.region_idx = region_idx
        return out


def compute_reference_no_vaccine(seed: int, weeks: int, n_regions: int):
    """Fixed-counterfactual trajectory: deaths per region per week under
    zero allocation throughout. Same R0 sequence by virtue of the same seed."""
    ref = VaccineSim(seed, weeks, n_regions)
    ref_deaths = [[0] * weeks for _ in range(n_regions)]
    ref_pop = [r.population for r in ref.regions]
    for w in range(weeks):
        ref.week = w
        for r_idx in range(n_regions):
            out = ref.step_region(r_idx, 0)
            ref_deaths[r_idx][w] = out.deaths
    return ref_deaths, ref_pop


def corrected_score(outcomes, ref_deaths, ref_pop) -> float:
    total = 0.0
    for o in outcomes:
        r_idx = o.region_idx
        w = o.week
        ref = ref_deaths[r_idx][w] if r_idx < len(ref_deaths) and w < len(ref_deaths[r_idx]) else 0
        lives_saved = max(0, ref - o.deaths)
        normalized_lives = min(1.0, lives_saved / 100.0)
        normalized_cost = min(1.0, o.vaccine_cost / (weekly_supply(w) * COST_PER_DOSE_USD + 1.0))
        total += normalized_lives - 0.3 * normalized_cost
    return total


def clipping_diagnostic(outcomes, ref_deaths):
    """Return fraction of weeks where each reward's normalized_lives term
    hits its upper clipping bound (=1.0), and the fraction of weeks where
    it's at zero (floor). Tells us whether the reward is operating in its
    meaningful signal range vs saturated at a boundary."""
    n = len(outcomes)
    if n == 0: return {}
    orig_at_ceiling = 0
    orig_at_floor = 0
    corr_at_ceiling = 0
    corr_at_floor = 0
    for o in outcomes:
        # Reconstruct normalized_lives under both rewards.
        # Original uses the policy-dependent counterfactual baked into o.reward,
        # so we recompute the components here directly. The simulator stored
        # the original reward already; we can recover normalized_lives_orig as
        # whatever the original-counterfactual scheme produced. For the
        # diagnostic, we just look at the corrected one (where we know the
        # baseline) and report saturation directly.
        ref = ref_deaths[o.region_idx][o.week] if o.region_idx < len(ref_deaths) and o.week < len(ref_deaths[o.region_idx]) else 0
        lives_saved_corr = max(0, ref - o.deaths)
        nl_corr = min(1.0, lives_saved_corr / 100.0)
        if nl_corr >= 0.999: corr_at_ceiling += 1
        if nl_corr <= 0.001: corr_at_floor += 1
    return {
        "n_weeks": n,
        "corrected_lives_at_ceiling": corr_at_ceiling / n,
        "corrected_lives_at_floor": corr_at_floor / n,
    }


# ----- Policies -----

class AllocPolicy:
    def __init__(self, name): self.name = name
    def allocate(self, sim: VaccineSim) -> list:
        """Return a list of doses per region summing to weekly_supply(sim.week)."""
        raise NotImplementedError


class EqualSplit(AllocPolicy):
    def __init__(self): super().__init__("equal_split")
    def allocate(self, sim):
        n = len(sim.regions)
        supply = weekly_supply(sim.week)
        base = supply // n
        rem = supply - base * n
        out = [base] * n
        for i in range(rem):
            out[i] += 1
        return out


class ProportionalToCases(AllocPolicy):
    def __init__(self): super().__init__("proportional_to_cases")
    def allocate(self, sim):
        supply = weekly_supply(sim.week)
        n = len(sim.regions)
        weights = [r.cases_active for r in sim.regions]
        s = sum(weights)
        if s == 0:
            base = supply // n
            rem = supply - base * n
            out = [base] * n
            for i in range(rem):
                out[i] += 1
            return out
        raw = [supply * w / s for w in weights]
        out = [int(x) for x in raw]
        remainder = supply - sum(out)
        fractions = sorted(range(n), key=lambda i: raw[i] - int(raw[i]), reverse=True)
        for i in range(remainder):
            out[fractions[i % n]] += 1
        return out


class ProportionalToSusceptible(AllocPolicy):
    def __init__(self): super().__init__("proportional_to_susceptible")
    def allocate(self, sim):
        supply = weekly_supply(sim.week)
        n = len(sim.regions)
        weights = [r.susceptible for r in sim.regions]
        s = sum(weights)
        if s == 0:
            base = supply // n
            return [base] * n + [0] * (supply - base * n)
        raw = [supply * w / s for w in weights]
        out = [int(x) for x in raw]
        remainder = supply - sum(out)
        fractions = sorted(range(n), key=lambda i: raw[i] - int(raw[i]), reverse=True)
        for i in range(remainder):
            out[fractions[i % n]] += 1
        return out


class ProactiveHighRisk(AllocPolicy):
    def __init__(self): super().__init__("proactive_high_risk")
    def allocate(self, sim):
        supply = weekly_supply(sim.week)
        n = len(sim.regions)
        # Front-load regions with R0 > 1.5; if any, split entirely among them.
        # If none, fall back to equal split.
        high_risk = [i for i, r in enumerate(sim.regions) if r.last_R0 > 1.5]
        if not high_risk:
            base = supply // n
            rem = supply - base * n
            out = [base] * n
            for i in range(rem):
                out[i] += 1
            return out
        base = supply // len(high_risk)
        rem = supply - base * len(high_risk)
        out = [0] * n
        for j, i in enumerate(high_risk):
            out[i] = base + (1 if j < rem else 0)
        return out


class MyopicOracle(AllocPolicy):
    def __init__(self): super().__init__("myopic_oracle")
    def allocate(self, sim):
        # Greedy: allocate doses one chunk at a time to the region with
        # highest marginal expected lives_saved next week. Coarsens for speed.
        supply = weekly_supply(sim.week)
        n = len(sim.regions)
        if supply == 0:
            return [0] * n
        chunk = max(1, supply // 20)  # 20-step greedy
        out = [0] * n
        remaining = supply
        # Marginal value: rough projection of lives saved by adding `chunk`
        # doses to region i given current state.
        def marginal_value(i, current_alloc):
            r = sim.regions[i]
            new_alloc = current_alloc + chunk
            # Approximate: each dose with efficacy moves a susceptible to
            # recovered after lag; the value is proportional to that region's
            # current R0 and cases_active.
            effective = min(new_alloc, r.susceptible) * VACCINE_EFFICACY
            rt = r.last_R0 * max(0, (r.susceptible - effective)) / max(1, r.population)
            projected_cases_with = max(0, r.cases_active * (rt - 1.0)) + r.cases_active * 0.2
            projected_deaths_with = projected_cases_with * r.cfr
            # Baseline projection with current_alloc
            effective_base = min(current_alloc, r.susceptible) * VACCINE_EFFICACY
            rt_base = r.last_R0 * max(0, (r.susceptible - effective_base)) / max(1, r.population)
            projected_cases_base = max(0, r.cases_active * (rt_base - 1.0)) + r.cases_active * 0.2
            projected_deaths_base = projected_cases_base * r.cfr
            return projected_deaths_base - projected_deaths_with
        while remaining > 0:
            best_i = 0
            best_v = float("-inf")
            for i in range(n):
                v = marginal_value(i, out[i])
                if v > best_v:
                    best_v = v
                    best_i = i
            give = min(chunk, remaining)
            out[best_i] += give
            remaining -= give
        return out


# ── Syntra integration ─────────────────────────────────────────────────────
#
# The simulator's native action space is continuous: allocate weekly_supply
# doses across N regions. To pose this as a discrete bandit problem we ask
# Syntra to pick a *priority region* each week. The harness then applies a
# fixed allocation rule:
#
#     priority gets 50% of supply, rest split equally
#
# Reward fed back is the mean of the per-region original reward (matches the
# capsule's [-1.0, 1.0] continuous range; one feedback per week per Syntra
# decision).
#
# Two context modes:
#   discrete: one bucket per week (uniform context_key); meta-bandit has
#             5 candidates.
#   features: avg_active_per_100k + avg_susceptible_frac + week_phase;
#             enrolls LinUCB as the 6th candidate.

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

    def author_and_install(self, n_regions):
        """Author a YAML capsule with n_regions options on the fly, compile via
        `syntra author`, and POST the resulting program.lyc to /install.
        """
        with tempfile.TemporaryDirectory() as tmp:
            spec_path = os.path.join(tmp, "spec.yaml")
            out_dir = os.path.join(tmp, "out")
            spec = (
                "name: vaccine-priority\n"
                "options:\n"
                + "".join(f"  - region_{i}\n" for i in range(n_regions))
                + "reward:\n"
                  "  type: continuous\n"
                  "  range: [-1.0, 1.0]\n"
            )
            with open(spec_path, "w") as f:
                f.write(spec)
            subprocess.run(
                ["syntra", "author", spec_path, "--out-dir", out_dir],
                check=True, stdout=subprocess.DEVNULL,
            )
            try:
                self._request("POST", f"/tenants/{self.tenant}/jobs",
                              {"id": self.job, "name": "Vaccine Benchmark"})
            except Exception:
                pass
            with open(os.path.join(out_dir, "program.lyc"), "rb") as f:
                self._request("POST", f"{self.base_path}/install", raw=f.read())

    def configure_learning(self, algorithm: str = "meta_bandit",
                           context_type: str = "discrete"):
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
        body = {
            "learningRate": 0.05,
            "decay": {"enabled": True, "halfLifeFeedbacks": 60},
            "window": {"enabled": True, "size": 30},
            "changeDetection": {"enabled": True, "method": "pageHinkley",
                                "threshold": 3.0, "minDrift": 0.05,
                                "explorationBoost": 0.30, "boostDuration": 8},
            "safety": {
                "minExploration": 0.08, "rewardClip": 1.0,
                "snapshotOnFeedback": False, "journalOnFeedback": False,
                "selectionEpsilon": 0.10,
            },
        }
        if algorithm in algo_to_alg_field:
            body["algorithm"] = algo_to_alg_field[algorithm]
            body["safety"]["selectionMode"] = algo_to_selection_mode[algorithm]
        # else: meta_bandit — omit `algorithm` and `selectionMode`.
        if context_type == "features":
            body["contextSpec"] = {
                "type": "features",
                "features": [
                    {"name": "avg_active_per_100k",
                     "type": {"kind": "continuous", "range": [0.0, 2000.0]}},
                    {"name": "avg_susceptible_frac",
                     "type": {"kind": "continuous", "range": [0.0, 1.0]}},
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

    def feedback(self, decision_id, reward):
        body = {"decisionId": decision_id, "reward": reward}
        return self._request("POST", f"{self.base_path}/feedback", body)


class SyntraAllocPolicy(AllocPolicy):
    """Wraps a Syntra capsule as an allocation policy.

    Calls /decide each week to pick the priority region; allocates 50% of
    weekly supply to the priority and splits the remaining 50% evenly.
    Feeds back the mean per-region reward after the week resolves.
    """
    def __init__(self, syntra: SyntraClient, context_type: str):
        super().__init__("syntra")
        self.syntra = syntra
        self.context_type = context_type
        self.last_decision_id = None

    def _features(self, sim) -> dict:
        rates = [(r.cases_active / r.population) * 100_000 for r in sim.regions]
        susc = [r.susceptible / r.population for r in sim.regions]
        return {
            "avg_active_per_100k": min(sum(rates) / len(rates), 2000.0),
            "avg_susceptible_frac": sum(susc) / len(susc),
            "week_phase": (sim.week % 24) + 0.0,
        }

    def allocate(self, sim):
        supply = weekly_supply(sim.week)
        n = len(sim.regions)
        if self.context_type == "features":
            decision = self.syntra.decide(features=self._features(sim))
        else:
            # One bucket per week — context-free; lets the bandit converge
            # on a single global preference.
            decision = self.syntra.decide(context_key="global")
        decisions = decision.get("decisions") or []
        if decisions:
            priority = int(decisions[0].get("chosen_option", 0))
            self.last_decision_id = decision.get("decisionId")
        else:
            priority = 0
            self.last_decision_id = None
        # Allocation rule: 50% to priority + 50% even split.
        priority_share = supply // 2
        rest = supply - priority_share
        base = rest // n
        rem = rest - base * n
        out = [base] * n
        for i in range(rem):
            out[i] += 1
        out[priority] += priority_share
        return out

    def feedback_for_week(self, week_outcomes: list):
        if self.last_decision_id is None:
            return
        if not week_outcomes:
            return
        mean_reward = sum(o.reward for o in week_outcomes) / len(week_outcomes)
        mean_reward = max(-1.0, min(1.0, mean_reward))
        try:
            self.syntra.feedback(self.last_decision_id, mean_reward)
        except Exception as e:
            print(f"  feedback err: {e}", file=sys.stderr)


def run_seed(seed, weeks, n_regions, syntra_policy=None):
    policies = {
        "equal_split": EqualSplit(),
        "proportional_to_cases": ProportionalToCases(),
        "proportional_to_susceptible": ProportionalToSusceptible(),
        "proactive_high_risk": ProactiveHighRisk(),
        "myopic_oracle": MyopicOracle(),
    }
    if syntra_policy is not None:
        policies["syntra"] = syntra_policy
    sims = {name: VaccineSim(seed, weeks, n_regions) for name in policies}
    ref_deaths, ref_pop = compute_reference_no_vaccine(seed, weeks, n_regions)
    outcomes = {name: [] for name in policies}

    for week in range(weeks):
        for sim in sims.values():
            sim.week = week
        for name, pol in policies.items():
            sim_p = sims[name]
            alloc = pol.allocate(sim_p)
            week_outcomes = []
            for r_idx in range(n_regions):
                out = sim_p.step_region(r_idx, alloc[r_idx])
                outcomes[name].append(out)
                week_outcomes.append(out)
            # Feed reward back to Syntra after the week resolves.
            if name == "syntra" and isinstance(pol, SyntraAllocPolicy):
                pol.feedback_for_week(week_outcomes)

    summary = {}
    for name, outs in outcomes.items():
        total_deaths = sum(o.deaths for o in outs)
        total_cost = sum(o.vaccine_cost for o in outs)
        total_reward_original = sum(o.reward for o in outs)
        total_reward_corrected = corrected_score(outs, ref_deaths, ref_pop)
        diag = clipping_diagnostic(outs, ref_deaths)
        summary[name] = {
            "total_deaths": total_deaths,
            "total_vaccine_cost_M": total_cost / 1e6,
            "total_reward_original": total_reward_original,
            "total_reward_corrected": total_reward_corrected,
            "corrected_lives_at_ceiling": diag.get("corrected_lives_at_ceiling", 0.0),
            "corrected_lives_at_floor": diag.get("corrected_lives_at_floor", 0.0),
        }
    return {"seed": seed, "summary": summary, "outcomes": outcomes}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--seeds", type=int, default=10)
    p.add_argument("--seed-offset", type=int, default=3000,
                   help="Starting seed value. Default 3000 (vaccine development seeds).")
    p.add_argument("--weeks", type=int, default=N_WEEKS_DEFAULT)
    p.add_argument("--regions", type=int, default=N_REGIONS_DEFAULT)
    p.add_argument("--output-dir", default=None)
    p.add_argument("--algorithm", default=None,
                   choices=[None, "weighted", "epsilon_greedy", "ucb", "meta_bandit"],
                   help="If set, include Syntra as a 6th allocation policy. "
                        "meta_bandit drops the algorithm pin and lets the Phase A-F "
                        "warmup + rate-adaptive meta-bandit pick the candidate.")
    p.add_argument("--context-type", default="discrete",
                   choices=["discrete", "features"],
                   help="Syntra context: discrete (single bucket) or features "
                        "(avg_active + avg_susceptible + week_phase; LinUCB-eligible)")
    p.add_argument("--syntra-url", default="http://localhost:8787")
    p.add_argument("--admin-key", default="dev-key")
    args = p.parse_args()

    if args.output_dir is None:
        ts = time.strftime("%Y%m%d_%H%M%S")
        args.output_dir = os.path.join(
            os.path.dirname(os.path.abspath(__file__)), "results", f"run_{ts}")
    os.makedirs(args.output_dir, exist_ok=True)

    syntra_client = None
    if args.algorithm:
        try:
            with urllib.request.urlopen(f"{args.syntra_url}/health", timeout=5) as r:
                json.loads(r.read().decode())
        except Exception as e:
            print(f"ERROR: cannot reach Syntra at {args.syntra_url}: {e}",
                  file=sys.stderr)
            sys.exit(1)
        syntra_client = SyntraClient(
            args.syntra_url, args.admin_key, "vaccine", "main", "alloc")
        syntra_client.reset()
        syntra_client.author_and_install(args.regions)
        syntra_client.configure_learning(algorithm=args.algorithm,
                                         context_type=args.context_type)

    print("=" * 72)
    print("  VACCINE ALLOCATION RESILIENCE BENCHMARK")
    print("=" * 72)
    print(f"  Seeds: {args.seeds} (offset {args.seed_offset})  "
          f"Weeks: {args.weeks}  Regions: {args.regions}")
    if syntra_client:
        print(f"  Syntra: algorithm={args.algorithm}  context={args.context_type}")
    print()

    all_results = []
    for i in range(args.seeds):
        seed = args.seed_offset + i
        syntra_policy = None
        if syntra_client is not None:
            # Fresh capsule state per seed (matches outbreak benchmark pattern).
            syntra_client.reset()
            syntra_client.author_and_install(args.regions)
            syntra_client.configure_learning(algorithm=args.algorithm,
                                             context_type=args.context_type)
            syntra_policy = SyntraAllocPolicy(syntra_client, args.context_type)
        r = run_seed(seed, args.weeks, args.regions, syntra_policy=syntra_policy)
        s = r["summary"]
        mo = s["myopic_oracle"]
        eq = s["equal_split"]
        print(f"  seed {seed}: myopic deaths={mo['total_deaths']:>5}, "
              f"orig={mo['total_reward_original']:>7.3f}, "
              f"corr={mo['total_reward_corrected']:>7.3f}  "
              f"| equal deaths={eq['total_deaths']:>5}, "
              f"orig={eq['total_reward_original']:>7.3f}, "
              f"corr={eq['total_reward_corrected']:>7.3f}")
        all_results.append(r)

    policies = list(all_results[0]["summary"].keys())
    print()
    print("=" * 80)
    print("  AGGREGATE (mean across seeds)")
    print("=" * 80)
    header = f"  {'policy':<28} {'deaths':>8} {'cost ($M)':>11} {'orig':>9} {'corr':>9}"
    print(header)
    print("  " + "-" * (len(header) - 2))
    for pol in policies:
        md = statistics.mean(r["summary"][pol]["total_deaths"] for r in all_results)
        mc = statistics.mean(r["summary"][pol]["total_vaccine_cost_M"] for r in all_results)
        mo_r = statistics.mean(r["summary"][pol]["total_reward_original"] for r in all_results)
        mco_r = statistics.mean(r["summary"][pol]["total_reward_corrected"] for r in all_results)
        print(f"  {pol:<28} {md:>8.0f} {mc:>11.2f} {mo_r:>9.4f} {mco_r:>9.4f}")

    orig_values = [statistics.mean(r["summary"][pol]["total_reward_original"] for r in all_results) for pol in policies]
    corr_values = [statistics.mean(r["summary"][pol]["total_reward_corrected"] for r in all_results) for pol in policies]
    print()
    print(f"  Original reward spread (max-min across policies):  {max(orig_values) - min(orig_values):.4f}")
    print(f"  Corrected reward spread:                            {max(corr_values) - min(corr_values):.4f}")
    print()
    print("  Clipping diagnostic (per-week fractions, averaged over seeds):")
    print(f"  {'policy':<28} {'corr_lives_ceiling':>20} {'corr_lives_floor':>18}")
    for pol in policies:
        c = statistics.mean(r["summary"][pol]["corrected_lives_at_ceiling"] for r in all_results)
        f = statistics.mean(r["summary"][pol]["corrected_lives_at_floor"] for r in all_results)
        print(f"  {pol:<28} {c:>20.3f} {f:>18.3f}")

    summary_json = {
        "benchmark": "vaccine_allocation_resilience",
        "seeds": args.seeds,
        "seed_offset": args.seed_offset,
        "weeks": args.weeks,
        "regions": args.regions,
        "aggregate": {
            pol: {
                "mean_deaths": statistics.mean(r["summary"][pol]["total_deaths"] for r in all_results),
                "mean_cost_M": statistics.mean(r["summary"][pol]["total_vaccine_cost_M"] for r in all_results),
                "mean_reward_original": statistics.mean(r["summary"][pol]["total_reward_original"] for r in all_results),
                "mean_reward_corrected": statistics.mean(r["summary"][pol]["total_reward_corrected"] for r in all_results),
            } for pol in policies
        },
        "spread_original": max(orig_values) - min(orig_values),
        "spread_corrected": max(corr_values) - min(corr_values),
    }
    with open(os.path.join(args.output_dir, "summary.json"), "w") as f:
        json.dump(summary_json, f, indent=2)
    with open(os.path.join(args.output_dir, "seeds.csv"), "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["seed", "policy", "deaths", "cost_M", "reward_original", "reward_corrected"])
        for r in all_results:
            for pol, s in r["summary"].items():
                w.writerow([r["seed"], pol, s["total_deaths"],
                            f"{s['total_vaccine_cost_M']:.3f}",
                            f"{s['total_reward_original']:.4f}",
                            f"{s['total_reward_corrected']:.4f}"])
    print(f"\n  Output: {args.output_dir}")


if __name__ == "__main__":
    main()
