#!/usr/bin/env python3
"""
Three sanity checks before trusting the Syntra/VW MAB result.

(1) VW alone on a known problem. Arm 0 wins p=0.9, Arm 1 p=0.1. Expect VW
    to pick arm 0 ~90% of the time after warmup (10% goes to epsilon
    exploration). Also confirms cost-vs-reward convention.

(2) Syntra alone on the same problem with Thompson. Print the Beta
    posteriors after the run. Expect arm 0 ≈ Beta(picks*0.9, picks*0.1),
    arm 1 ≈ Beta(small, small).

(3) Uniform-random baseline. Pick uniformly at random every round, with
    zero state. Expected regret ≈ 0.5 × n_rounds × mean_gap. If the harness
    produces this number, the regret computation is sound.

Usage: /tmp/vw_env/bin/python diagnostics.py
"""

import json
import os
import random
import sys
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import benchmark as bm
import vowpalwabbit


def vw_alone_known_problem(rounds=2000):
    print("=" * 72)
    print("CHECK 1: VW alone on arm-means [0.1, 0.9] (arm 1 better, NOT arm 0)")
    print("Deliberately reversing the typical 'arm 0 wins' assumption to catch")
    print("any cost-vs-reward sign confusion. Expect VW to converge on arm 1.")
    print("=" * 72)
    arm_means = [0.1, 0.9]
    rng = random.Random(12345)
    vw = bm.VWMAB(n_arms=2, epsilon=0.10)
    picks = [0, 0]
    rewards_seen = [0.0, 0.0]
    last_500_picks = [0, 0]
    cumulative_regret = 0.0
    best_mean = max(arm_means)
    for t in range(rounds):
        arm = vw.choose(rng)
        reward = 1.0 if rng.random() < arm_means[arm] else 0.0
        vw.feedback(arm, reward)
        picks[arm] += 1
        rewards_seen[arm] += reward
        cumulative_regret += best_mean - arm_means[arm]
        if t >= rounds - 500:
            last_500_picks[arm] += 1

    print(f"  picks over {rounds} rounds: arm 0 = {picks[0]}, arm 1 = {picks[1]}")
    print(f"  empirical win rates: arm 0 = {rewards_seen[0]/max(1,picks[0]):.3f}, arm 1 = {rewards_seen[1]/max(1,picks[1]):.3f}")
    print(f"  picks in last 500 rounds: arm 0 = {last_500_picks[0]}, arm 1 = {last_500_picks[1]}")
    print(f"  cumulative regret: {cumulative_regret:.1f}")
    print()
    if last_500_picks[1] > last_500_picks[0]:
        share = last_500_picks[1] / 500
        print(f"  ✓ VW converged on arm 1 (better arm), {share:.1%} of last 500 rounds")
        print(f"    Cost/reward convention is correct — VW maximizes reward.")
        if share < 0.80:
            print(f"  ⚠ Convergence weaker than expected (>=80% for ε=0.10 on Δ=0.8).")
    else:
        print(f"  ✗ VW picked arm 0 (worse) in last 500 rounds.")
        print(f"    Likely cost-vs-reward sign issue. Investigate before trusting the comparison.")
    print()
    return last_500_picks[1] / 500


def syntra_alone_known_problem(rounds=2000):
    print("=" * 72)
    print("CHECK 2: Syntra alone on arm-means [0.1, 0.9] with Thompson")
    print("Expect Syntra to converge on arm 1 and Beta posteriors to reflect")
    print("the per-arm empirical success rates.")
    print("=" * 72)
    arm_means = [0.1, 0.9]
    rng = random.Random(12345)
    capsule_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mab_2arm.lyc")
    syntra = bm.SyntraMAB(
        bm.SYNTRA_BASE_URL, bm.ADMIN_KEY,
        "diag_syntra_alone", "main", "policy", capsule_path,
    )
    picks = [0, 0]
    rewards_seen = [0.0, 0.0]
    last_500_picks = [0, 0]
    cumulative_regret = 0.0
    best_mean = max(arm_means)
    for t in range(rounds):
        arm = syntra.choose()
        reward = 1.0 if rng.random() < arm_means[arm] else 0.0
        syntra.feedback(arm, reward)
        picks[arm] += 1
        rewards_seen[arm] += reward
        cumulative_regret += best_mean - arm_means[arm]
        if t >= rounds - 500:
            last_500_picks[arm] += 1

    print(f"  picks over {rounds} rounds: arm 0 = {picks[0]}, arm 1 = {picks[1]}")
    print(f"  empirical win rates: arm 0 = {rewards_seen[0]/max(1,picks[0]):.3f}, arm 1 = {rewards_seen[1]/max(1,picks[1]):.3f}")
    print(f"  picks in last 500 rounds: arm 0 = {last_500_picks[0]}, arm 1 = {last_500_picks[1]}")
    print(f"  cumulative regret: {cumulative_regret:.1f}")

    # Read the bucket state from Syntra to see the Beta posteriors
    mem_url = f"{bm.SYNTRA_BASE_URL}{syntra.base_path}/memory"
    req = urllib.request.Request(mem_url)
    req.add_header("Authorization", f"Bearer {bm.ADMIN_KEY}")
    with urllib.request.urlopen(req, timeout=10) as r:
        mem = json.loads(r.read())
    strategies = mem.get("strategies", {})
    for nid, sm in strategies.items():
        for ctx_key, bucket in sm.get("contexts", {}).items():
            states = bucket.get("optionStates", [])
            print(f"  bucket optionStates (node {nid}, ctx {ctx_key}):")
            for i, s in enumerate(states):
                if s and s.get("kind") == "betaBernoulli":
                    a, b = s["alpha"], s["beta"]
                    mean = a / (a + b)
                    print(f"    arm {i}: Beta(α={a:.1f}, β={b:.1f})  mean={mean:.3f}  picks={picks[i]}")
                    if abs(a + b - picks[i] - 2) > max(2, picks[i]*0.05):
                        print(f"      ⚠ α+β-2 ({a+b-2:.0f}) ≠ picks ({picks[i]}) — informational state mismatch")
                else:
                    print(f"    arm {i}: NOT BetaBernoulli — got {s}")
    print()
    if last_500_picks[1] > last_500_picks[0]:
        share = last_500_picks[1] / 500
        print(f"  ✓ Syntra converged on arm 1 (better arm), {share:.1%} of last 500 rounds")
    else:
        print(f"  ✗ Syntra picked arm 0 (worse) in last 500 rounds.")
        print(f"    Thompson implementation may have a bug. Inspect Beta posteriors above.")
    print()
    return last_500_picks[1] / 500


def uniform_random_baseline(rounds=2000):
    print("=" * 72)
    print("CHECK 3: Uniform-random baseline on the full PROBLEMS set")
    print("Expected regret per cell: 0.5 × rounds × mean_gap_across_arms")
    print("If the harness math is correct, uniform-random regrets should match.")
    print("=" * 72)
    print(f"  {'arms':<5} {'difficulty':<10} {'expected':>10} {'observed':>10}")
    rng = random.Random(98765)
    for n_arms in [2, 5, 10]:
        for difficulty, arm_means in bm.PROBLEMS[n_arms].items():
            best_mean = max(arm_means)
            # Expected regret of uniform-random
            expected = sum(best_mean - m for m in arm_means) / len(arm_means) * rounds

            # Run uniform-random
            cumulative_regret = 0.0
            for t in range(rounds):
                arm = rng.randint(0, n_arms - 1)
                cumulative_regret += best_mean - arm_means[arm]
            ratio = cumulative_regret / max(expected, 1e-9)
            mark = "✓" if 0.85 <= ratio <= 1.15 else "⚠"
            print(f"  {n_arms:<5} {difficulty:<10} {expected:>10.1f} {cumulative_regret:>10.1f}  ratio={ratio:.2f} {mark}")
    print()


if __name__ == "__main__":
    try:
        with urllib.request.urlopen(f"{bm.SYNTRA_BASE_URL}/health", timeout=5) as r:
            assert json.loads(r.read())["ok"]
    except Exception as e:
        print(f"ERROR: Syntra not reachable: {e}", file=sys.stderr)
        sys.exit(1)

    vw_share = vw_alone_known_problem(rounds=2000)
    syntra_share = syntra_alone_known_problem(rounds=2000)
    uniform_random_baseline(rounds=2000)

    print("=" * 72)
    print("SUMMARY")
    print("=" * 72)
    print(f"  VW share-of-best-arm in last 500: {vw_share:.1%}  (expect ≥80% for Δ=0.8, ε=0.10)")
    print(f"  Syntra share-of-best-arm in last 500: {syntra_share:.1%}  (expect ≥90%)")
    print(f"  Uniform-random regret ratio: see table above (expect all within [0.85, 1.15])")
    print()
    print("  If all three checks pass: bin D result reproduces in good faith.")
    print("  If any check fails: investigate before running the full benchmark.")
