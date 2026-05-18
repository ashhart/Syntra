#!/usr/bin/env python3
"""
Syntra vs Vowpal Wabbit on standard multi-armed bandit problems.

Pre-registered configuration: 3 arm counts × 3 difficulty levels × 10 seeds.
Both algorithms run with epsilon-greedy-equivalent setup, 2000 rounds each.
Metric: cumulative regret at end of run.

Per Task 1 revert, Syntra's runtime path samples bucket weights regardless
of the algorithm field. The comparison is therefore between
  - Syntra: weighted-bucket sampling with learning rate 0.02 (the deployed
    behavior of the appliance)
  - VW: --cb_explore with epsilon=0.10 (field standard contextual bandit
    in pure MAB mode)

This is a fair test of Syntra's bandit core in Frame 1 (per-context-bucket
adaptive selection on problems with discrete context). It is NOT a test of
feature-aware contextual bandit performance; Syntra is not built for that.

Usage:
    /tmp/vw_env/bin/python benchmark.py [--seeds N] [--rounds N]
"""

import argparse
import json
import os
import random
import statistics
import sys
import time
import urllib.error
import urllib.request

import vowpalwabbit

SYNTRA_BASE_URL = os.environ.get("SYNTRA_URL", "http://localhost:8787")
ADMIN_KEY = os.environ.get("ADMIN_KEY", "dev-key")
CAPSULE_DIR = os.path.dirname(os.path.abspath(__file__))


PROBLEMS = {
    # name → list of arm-success-probability vectors
    # We pre-define problems so seeds only affect which Bernoulli draws happen,
    # not the underlying arm means. This isolates "convergence speed" from
    # "problem variance."
    2: {
        "easy":   [0.30, 0.80],
        "medium": [0.40, 0.60],
        "hard":   [0.475, 0.525],
    },
    5: {
        "easy":   [0.20, 0.30, 0.40, 0.50, 0.80],
        "medium": [0.40, 0.45, 0.50, 0.55, 0.60],
        "hard":   [0.475, 0.490, 0.500, 0.510, 0.525],
    },
    10: {
        "easy":   [0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.65, 0.80],
        "medium": [0.40, 0.42, 0.44, 0.46, 0.48, 0.52, 0.54, 0.56, 0.58, 0.60],
        "hard":   [0.475, 0.480, 0.485, 0.490, 0.495, 0.505, 0.510, 0.515, 0.520, 0.525],
    },
}


# ---------------- Syntra wrapper ----------------

class SyntraMAB:
    def __init__(self, base_url, admin_key, tenant, job, capsule, capsule_path):
        self.base_url = base_url.rstrip("/")
        self.admin_key = admin_key
        self.tenant = tenant
        self.job = job
        self.capsule = capsule
        self.base_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
        self.capsule_path = capsule_path
        self.node_id = None
        self.setup()

    def _req(self, method, path, body=None, raw=None):
        url = f"{self.base_url}{path}"
        data = raw if raw is not None else (json.dumps(body).encode() if body else None)
        req = urllib.request.Request(url, data=data, method=method)
        req.add_header("Authorization", f"Bearer {self.admin_key}")
        if raw is not None:
            req.add_header("Content-Type", "application/octet-stream")
        elif data:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                txt = r.read().decode()
                return json.loads(txt) if txt else {}
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"Syntra HTTP {e.code}: {e.read().decode()}") from e

    def setup(self):
        try:
            self._req("DELETE", f"/tenants/{self.tenant}")
        except Exception:
            pass
        try:
            self._req("POST", f"/tenants/{self.tenant}/jobs",
                      {"id": self.job, "name": "syntra-vs-vw"})
        except Exception:
            pass
        with open(self.capsule_path, "rb") as f:
            self._req("POST", f"{self.base_path}/install", raw=f.read())
        self._req("PUT", f"{self.base_path}/learning", {
            "algorithm": "thompson",
            "learningRate": 0.02,
            "decay": {"enabled": False},
            "window": {"enabled": False},
            "changeDetection": {"enabled": False},
            "conformal": {"enabled": False},
            "safety": {
                "minExploration": 0.05,
                "rewardClip": 1.0,
                "snapshotOnFeedback": False,
                "journalOnFeedback": False,
                "selectionMode": "greedy",
                "selectionEpsilon": 0.0,
            },
        })

    def choose(self):
        dec = self._req("POST", f"{self.base_path}/decide",
                         {"contextKey": "default", "input": {}})
        d0 = dec["decisions"][0]
        if self.node_id is None:
            self.node_id = d0["node_id"]
        return d0["chosen_option"]

    def feedback(self, arm, reward):
        self._req("POST", f"{self.base_path}/feedback",
                   {"strategyId": self.node_id, "option": arm,
                    "reward": reward, "contextKey": "default"})


# ---------------- VW wrapper ----------------

class VWMAB:
    def __init__(self, n_arms, epsilon=0.10):
        self.n_arms = n_arms
        self.epsilon = epsilon
        self.vw = vowpalwabbit.Workspace(
            f"--cb_explore {n_arms} --epsilon {epsilon} --quiet"
        )
        self.last_probs = None

    def choose(self, rng):
        ex = self.vw.parse("| Constant:1")
        probs = self.vw.predict(ex)
        self.last_probs = probs
        # Sample from probabilities (1-indexed in VW; convert to 0-indexed)
        r = rng.random()
        cum = 0.0
        for i, p in enumerate(probs):
            cum += p
            if r < cum:
                return i
        return self.n_arms - 1

    def feedback(self, arm, reward):
        # VW expects cost not reward, and 1-indexed actions
        cost = -reward
        action = arm + 1
        prob = self.last_probs[arm]
        learn_str = f"{action}:{cost}:{prob:.6f} | Constant:1"
        ex = self.vw.parse(learn_str)
        self.vw.learn(ex)
        self.vw.finish_example(ex)


# ---------------- Problem ----------------

def sample_bernoulli(arm_means, arm, rng):
    """Return a reward in {0, 1} for pulling `arm` with given mean rewards."""
    return 1.0 if rng.random() < arm_means[arm] else 0.0


def run_instance(algo, arm_means, n_rounds, rng):
    """Run one algorithm on one MAB problem instance. Returns list of
    per-round (chosen_arm, reward, cumulative_regret)."""
    best_mean = max(arm_means)
    cumulative_regret = 0.0
    trace = []
    for t in range(n_rounds):
        if isinstance(algo, VWMAB):
            arm = algo.choose(rng)
        else:
            arm = algo.choose()
        reward = sample_bernoulli(arm_means, arm, rng)
        algo.feedback(arm, reward)
        # Expected-regret per round: optimal_mean - chosen_arm_mean
        regret_step = best_mean - arm_means[arm]
        cumulative_regret += regret_step
        trace.append((arm, reward, cumulative_regret))
    return trace


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--seeds", type=int, default=10)
    p.add_argument("--rounds", type=int, default=2000)
    p.add_argument("--output-dir", default=None)
    args = p.parse_args()

    if args.output_dir is None:
        ts = time.strftime("%Y%m%d_%H%M%S")
        args.output_dir = os.path.join(CAPSULE_DIR, "results", f"run_{ts}")
    os.makedirs(args.output_dir, exist_ok=True)

    print("=" * 72)
    print("  SYNTRA vs VW — Multi-Armed Bandit Comparison")
    print("=" * 72)
    print(f"  Seeds: {args.seeds}  Rounds/instance: {args.rounds}")
    print(f"  Cells: 3 arm counts × 3 difficulty = 9 cells")
    print()

    # Verify Syntra is up
    try:
        with urllib.request.urlopen(f"{SYNTRA_BASE_URL}/health", timeout=5) as r:
            assert json.loads(r.read())["ok"]
    except Exception as e:
        print(f"ERROR: Syntra not reachable at {SYNTRA_BASE_URL}: {e}", file=sys.stderr)
        sys.exit(1)

    results = []
    for n_arms in [2, 5, 10]:
        capsule_path = os.path.join(CAPSULE_DIR, f"mab_{n_arms}arm.lyc")
        for difficulty, arm_means in PROBLEMS[n_arms].items():
            for seed_idx in range(args.seeds):
                seed = 5000 + seed_idx + n_arms * 100 + hash(difficulty) % 1000
                cell = (n_arms, difficulty)

                # Syntra
                t0 = time.time()
                rng_s = random.Random(seed)
                syntra = SyntraMAB(
                    SYNTRA_BASE_URL, ADMIN_KEY,
                    f"mabbench{n_arms}{difficulty[0]}{seed_idx}",
                    "main", "policy", capsule_path,
                )
                trace_s = run_instance(syntra, arm_means, args.rounds, rng_s)
                regret_s = trace_s[-1][2]
                t_s = time.time() - t0

                # VW
                t0 = time.time()
                rng_v = random.Random(seed)
                vw = VWMAB(n_arms=n_arms, epsilon=0.10)
                trace_v = run_instance(vw, arm_means, args.rounds, rng_v)
                regret_v = trace_v[-1][2]
                t_v = time.time() - t0

                results.append({
                    "n_arms": n_arms,
                    "difficulty": difficulty,
                    "seed": seed,
                    "seed_idx": seed_idx,
                    "syntra_regret": regret_s,
                    "vw_regret": regret_v,
                    "ratio": regret_s / regret_v if regret_v > 0 else float("inf"),
                    "syntra_time_s": t_s,
                    "vw_time_s": t_v,
                })
                print(f"  arms={n_arms} {difficulty:<7s} seed={seed_idx:>2d}: "
                      f"syntra={regret_s:>7.2f} ({t_s:>4.1f}s)  "
                      f"vw={regret_v:>7.2f} ({t_v:>4.1f}s)  "
                      f"ratio={results[-1]['ratio']:>5.2f}")

    # Aggregate by cell
    cells = {}
    for r in results:
        key = (r["n_arms"], r["difficulty"])
        cells.setdefault(key, []).append(r)

    print()
    print("=" * 72)
    print("  AGGREGATE BY CELL (mean across seeds)")
    print("=" * 72)
    print(f"  {'arms':<5} {'difficulty':<10} {'syntra_regret':>14} {'vw_regret':>10} {'ratio':>8}")

    cell_summary = {}
    for (n_arms, difficulty), rs in sorted(cells.items()):
        s_mean = statistics.mean(r["syntra_regret"] for r in rs)
        v_mean = statistics.mean(r["vw_regret"] for r in rs)
        ratio_mean = statistics.mean(r["ratio"] for r in rs)
        cell_summary[f"{n_arms}_{difficulty}"] = {
            "syntra_regret": s_mean,
            "vw_regret": v_mean,
            "ratio_mean": ratio_mean,
        }
        print(f"  {n_arms:<5d} {difficulty:<10s} {s_mean:>14.2f} {v_mean:>10.2f} {ratio_mean:>8.2f}")

    n_cells = len(cell_summary)
    cells_within_1_5x = sum(1 for c in cell_summary.values() if c["ratio_mean"] <= 1.5)
    cells_within_2_5x = sum(1 for c in cell_summary.values() if c["ratio_mean"] <= 2.5)
    cells_better_than_vw = sum(1 for c in cell_summary.values() if c["ratio_mean"] < 1.0)
    cells_worse_than_2_5x = sum(1 for c in cell_summary.values() if c["ratio_mean"] > 2.5)

    print()
    print("  Pre-registered bin mapping:")
    print(f"    Cells where syntra ≤ 1.5× vw regret: {cells_within_1_5x}/{n_cells}")
    print(f"    Cells where syntra ≤ 2.5× vw regret: {cells_within_2_5x}/{n_cells}")
    print(f"    Cells where syntra < vw regret:       {cells_better_than_vw}/{n_cells}")
    print(f"    Cells where syntra > 2.5× vw regret:  {cells_worse_than_2_5x}/{n_cells}")

    if cells_within_1_5x >= 7:
        bin_label = "A — competent: within constant factor of VW on ≥7/9 cells"
    elif cells_within_2_5x >= 7:
        bin_label = "B — approximately competent: ≤2.5× VW on ≥7/9, but >1.5× on some"
    elif cells_worse_than_2_5x > 2:
        bin_label = "C — core has issues, investigate"
    elif cells_better_than_vw > 2:
        bin_label = "D — suspicious: Syntra better than VW on multiple cells"
    else:
        bin_label = "Mixed — falls between bins, report numbers, no clean label"
    print(f"\n  PRE-REGISTERED OUTCOME: {bin_label}")

    with open(os.path.join(args.output_dir, "summary.json"), "w") as f:
        json.dump({
            "config": {
                "seeds": args.seeds,
                "rounds": args.rounds,
            },
            "per_instance": results,
            "per_cell": cell_summary,
            "bin": bin_label,
        }, f, indent=2)
    print(f"\n  Output: {args.output_dir}")


if __name__ == "__main__":
    main()
