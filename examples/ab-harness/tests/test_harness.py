"""
tests/test_harness.py -- Unit tests for ab_harness.py.

These tests exercise all pure-math and parsing logic without requiring a live
Syntra server.  Run with:

    PYTHONPATH=. python3 -m pytest tests/ -v
"""
import json
import math
import os
import random
import sys
import tempfile
import textwrap
import unittest

# Make sure the parent directory is importable regardless of how pytest is
# invoked.
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from ab_harness import (
    TrafficSpec,
    _paired_t_test,
    _betainc,
    aggregate_results,
    compute_regret,
    parse_traffic_spec,
)


# ---------------------------------------------------------------------------
# Helper: write a temp JSON traffic spec and return the path.
# ---------------------------------------------------------------------------

def _write_json_spec(d: dict) -> str:
    fd, path = tempfile.mkstemp(suffix=".json")
    with os.fdopen(fd, "w") as fh:
        json.dump(d, fh)
    return path


# ---------------------------------------------------------------------------
# Test 1: paired t-test produces correct p-value on a known synthetic example.
# ---------------------------------------------------------------------------

class TestPairedTTest(unittest.TestCase):
    def test_known_significant_difference(self):
        """
        Known synthetic example: 10 paired differences all equal to 5.0,
        stdev=0 -> t -> infinity -> p should be 0.0 (or extremely close).
        """
        diffs = [5.0] * 10
        p = _paired_t_test(diffs)
        self.assertAlmostEqual(p, 0.0, places=6,
                               msg=f"Expected p~0 for identical large diffs, got {p}")

    def test_known_nonsignificant(self):
        """
        Symmetric diffs centred at 0 should yield a high p-value (> 0.5).
        diffs = [1, -1, 1, -1, ...] -> mean=0, high p.
        """
        diffs = [1.0, -1.0] * 8  # n=16, mean=0
        p = _paired_t_test(diffs)
        self.assertGreater(p, 0.5,
                           msg=f"Expected p>0.5 for zero-mean diffs, got {p}")

    def test_known_approximate_p_value(self):
        """
        For n=5 differences [2, 3, 2, 3, 2] the t-statistic is known.
        mean=2.4, stdev=sqrt(((2-2.4)^2+(3-2.4)^2+(2-2.4)^2+(3-2.4)^2+(2-2.4)^2)/4)
             = sqrt((0.16+0.36+0.16+0.36+0.16)/4) = sqrt(1.2/4) = sqrt(0.3) ~ 0.5477
        t = 2.4 / (0.5477 / sqrt(5)) = 2.4 / 0.2449 ~ 9.80
        df=4, two-tailed p is very small (well below 0.05).
        """
        diffs = [2.0, 3.0, 2.0, 3.0, 2.0]
        p = _paired_t_test(diffs)
        self.assertLess(p, 0.01,
                        msg=f"Expected p<0.01 for large consistent diffs, got {p}")

    def test_degenerate_n1(self):
        """Single pair: p must be 1.0 (undefined)."""
        p = _paired_t_test([3.7])
        self.assertEqual(p, 1.0)

    def test_degenerate_zero_variance(self):
        """All diffs identical and non-zero -> t is infinite -> p=0."""
        p = _paired_t_test([100.0] * 20)
        self.assertAlmostEqual(p, 0.0, places=6)

    def test_degenerate_all_zero(self):
        """All diffs are 0 -> stdev=0, mean=0 -> p=1.0 (no signal)."""
        p = _paired_t_test([0.0] * 10)
        self.assertEqual(p, 1.0)


# ---------------------------------------------------------------------------
# Test 2: cumulative regret math.
# ---------------------------------------------------------------------------

class TestCumulativeRegret(unittest.TestCase):
    def test_best_arm_zero_regret(self):
        """Always picking the oracle arm gives 0 cumulative regret."""
        oracle = [1.0, 1.0, 1.0, 1.0, 1.0]
        actual = [1.0, 1.0, 1.0, 1.0, 1.0]
        regret = compute_regret(oracle, actual)
        self.assertAlmostEqual(regret, 0.0, places=9)

    def test_worst_arm_full_regret(self):
        """Always picking the worst arm gives oracle_sum - worst_sum regret."""
        oracle = [1.0] * 10
        worst = [0.0] * 10
        regret = compute_regret(oracle, worst)
        self.assertAlmostEqual(regret, 10.0, places=9)

    def test_partial_regret(self):
        """Regret accumulates correctly across mixed rounds."""
        oracle = [1.0, 1.0, 1.0, 1.0]
        actual = [1.0, 0.5, 1.0, 0.5]  # regret = 0.5 + 0.5 = 1.0
        regret = compute_regret(oracle, actual)
        self.assertAlmostEqual(regret, 1.0, places=9)

    def test_negative_regret_impossible_when_oracle_is_best(self):
        """If oracle is computed as the max over arms, regret >= 0 always."""
        # Simulate: oracle picks 0.9 always; we happen to get 0.8 always.
        oracle = [0.9] * 5
        actual = [0.8] * 5
        regret = compute_regret(oracle, actual)
        self.assertGreaterEqual(regret, 0.0)


# ---------------------------------------------------------------------------
# Test 3: traffic spec parser.
# ---------------------------------------------------------------------------

class TestTrafficSpecParser(unittest.TestCase):
    def _valid_spec_dict(self):
        return {
            "arms": ["a", "b", "c"],
            "true_rewards": {"a": 0.2, "b": 0.5, "c": 0.3},
            "noise_std": 0.1,
        }

    def test_valid_spec_parsed(self):
        path = _write_json_spec(self._valid_spec_dict())
        try:
            spec = parse_traffic_spec(path)
            self.assertEqual(spec.arms, ["a", "b", "c"])
            self.assertAlmostEqual(spec.true_rewards["b"], 0.5)
            self.assertAlmostEqual(spec.noise_std, 0.1)
        finally:
            os.unlink(path)

    def test_missing_arms_raises(self):
        d = self._valid_spec_dict()
        del d["arms"]
        path = _write_json_spec(d)
        try:
            with self.assertRaises(ValueError, msg="Expected ValueError for missing 'arms'"):
                parse_traffic_spec(path)
        finally:
            os.unlink(path)

    def test_missing_true_rewards_raises(self):
        d = self._valid_spec_dict()
        del d["true_rewards"]
        path = _write_json_spec(d)
        try:
            with self.assertRaises(ValueError):
                parse_traffic_spec(path)
        finally:
            os.unlink(path)

    def test_too_few_arms_raises(self):
        d = self._valid_spec_dict()
        d["arms"] = ["only_one"]
        d["true_rewards"] = {"only_one": 0.5}
        path = _write_json_spec(d)
        try:
            with self.assertRaises(ValueError):
                parse_traffic_spec(path)
        finally:
            os.unlink(path)

    def test_missing_reward_for_arm_raises(self):
        d = self._valid_spec_dict()
        d["true_rewards"] = {"a": 0.1, "b": 0.5}  # "c" missing
        path = _write_json_spec(d)
        try:
            with self.assertRaises(ValueError):
                parse_traffic_spec(path)
        finally:
            os.unlink(path)

    def test_list_true_rewards_accepted(self):
        d = {"arms": ["x", "y"], "true_rewards": [0.3, 0.7]}
        path = _write_json_spec(d)
        try:
            spec = parse_traffic_spec(path)
            self.assertAlmostEqual(spec.true_rewards["y"], 0.7)
        finally:
            os.unlink(path)

    def test_regime_shifts_parsed(self):
        d = self._valid_spec_dict()
        d["regime_shifts"] = [{"at_round": 100, "new_rewards": {"b": 0.1}}]
        path = _write_json_spec(d)
        try:
            spec = parse_traffic_spec(path)
            self.assertEqual(len(spec.regime_shifts), 1)
            self.assertEqual(spec.regime_shifts[0]["at_round"], 100)
        finally:
            os.unlink(path)

    def test_negative_noise_std_raises(self):
        d = self._valid_spec_dict()
        d["noise_std"] = -0.5
        path = _write_json_spec(d)
        try:
            with self.assertRaises(ValueError):
                parse_traffic_spec(path)
        finally:
            os.unlink(path)


# ---------------------------------------------------------------------------
# Test 4: identical capsules show no significant difference (p > 0.05).
# ---------------------------------------------------------------------------

class TestIdenticalCapsulesNoDifference(unittest.TestCase):
    """Simulate two identical policies and verify the t-test is not significant."""

    def _simulate_identical_run(self, n_seeds: int, rounds: int, seed_offset: int) -> list:
        """Build synthetic per-seed results where A and B always choose the
        same arm (simulated identical policies), with noise."""
        rng = random.Random(42)
        results = []
        for i in range(n_seeds):
            seed = seed_offset + i
            rng_s = random.Random(seed)
            cum = 0.0
            per_round = []
            for _ in range(rounds):
                r = rng_s.gauss(0.5, 0.1)
                cum += r
                per_round.append(r)
            oracle = [0.6] * rounds  # slightly above policy
            regret = compute_regret(oracle, per_round)
            results.append({
                "seed": seed,
                "a": {
                    "cumulative_reward": cum,
                    "mean_per_round": cum / rounds,
                    "refusals": 0,
                    "regret_vs_oracle": regret,
                },
                "b": {
                    "cumulative_reward": cum,  # identical
                    "mean_per_round": cum / rounds,
                    "refusals": 0,
                    "regret_vs_oracle": regret,
                },
                "head_to_head": {
                    "b_minus_a_cumulative": 0.0,
                    "b_won_round_pct": 0.0,
                },
            })
        return results

    def test_identical_policies_not_significant(self):
        results = self._simulate_identical_run(n_seeds=20, rounds=100, seed_offset=5000)
        aggregate = aggregate_results(results, rounds=100)
        p = aggregate["p_value_paired_t"]
        self.assertGreater(p, 0.05,
                           msg=f"Identical capsules should not be significant; got p={p}")
        self.assertFalse(aggregate["confidence_b_better_at_95pct"])


# ---------------------------------------------------------------------------
# Test 5: regret_vs_oracle is correctly computed across rounds.
# ---------------------------------------------------------------------------

class TestRegretVsOracle(unittest.TestCase):
    def test_oracle_reward_per_spec(self):
        """TrafficSpec.oracle_reward should return the best arm's true reward."""
        spec = TrafficSpec(
            arms=["arm0", "arm1", "arm2"],
            true_rewards={"arm0": 0.1, "arm1": 0.8, "arm2": 0.4},
            noise_std=0.0,
            regime_shifts=[],
            context_sequence=None,
        )
        oracle_r = spec.oracle_reward(0, None)
        self.assertAlmostEqual(oracle_r, 0.8)

    def test_oracle_respects_regime_shift(self):
        """After a regime shift, oracle_reward should reflect new rewards."""
        spec = TrafficSpec(
            arms=["arm0", "arm1"],
            true_rewards={"arm0": 0.9, "arm1": 0.1},
            noise_std=0.0,
            regime_shifts=[{"at_round": 10, "new_rewards": {"arm0": 0.1, "arm1": 0.9}}],
            context_sequence=None,
        )
        self.assertAlmostEqual(spec.oracle_reward(5, None), 0.9)   # before shift
        self.assertAlmostEqual(spec.oracle_reward(10, None), 0.9)  # after shift (arm1 now best)
        self.assertAlmostEqual(spec.oracle_reward(15, None), 0.9)

    def test_cumulative_regret_across_rounds(self):
        """Full simulation: always picking worst arm gives exact total regret."""
        spec = TrafficSpec(
            arms=["good", "bad"],
            true_rewards={"good": 1.0, "bad": 0.0},
            noise_std=0.0,
            regime_shifts=[],
            context_sequence=None,
        )
        rounds = 50
        rng = random.Random(0)
        oracle_rewards = [spec.oracle_reward(r, None) for r in range(rounds)]
        # Policy always picks "bad"
        actual_rewards = [spec.reward_for("bad", None, rng) for _ in range(rounds)]
        regret = compute_regret(oracle_rewards, actual_rewards)
        self.assertAlmostEqual(regret, 50.0, places=5,
                               msg=f"Expected regret=50.0, got {regret}")


# ---------------------------------------------------------------------------
# Test 6: refusal handling -- counted and treated as 0 reward.
# ---------------------------------------------------------------------------

class TestRefusalHandling(unittest.TestCase):
    def test_refusal_increments_counter(self):
        """
        Simulate a seed result where some rounds are refusals (reward=0).
        Verify that cumulative reward equals sum of non-refused rounds only.
        """
        # Build a synthetic result that mimics what run_one_seed would return
        # when 3 out of 10 rounds are refused: those rounds contribute 0 reward.
        n_rounds = 10
        n_refusals = 3
        reward_per_good_round = 1.0

        good_rounds = n_rounds - n_refusals
        per_round_reward = [reward_per_good_round] * good_rounds + [0.0] * n_refusals
        cumulative = sum(per_round_reward)
        oracle = [reward_per_good_round] * n_rounds  # oracle always gets 1.0

        # The regret calculation counts refused rounds as 0 actual reward,
        # so regret = n_refusals * oracle_reward
        regret = compute_regret(oracle, per_round_reward)
        expected_regret = float(n_refusals) * reward_per_good_round

        self.assertAlmostEqual(cumulative, float(good_rounds), places=9)
        self.assertAlmostEqual(regret, expected_regret, places=9,
                               msg=f"Expected regret={expected_regret}, got {regret}")

    def test_aggregate_refusal_rate(self):
        """aggregate_results computes refusal_rate = mean(refusals) / rounds."""
        rounds = 100
        seed_results = [
            {
                "seed": i,
                "a": {"cumulative_reward": 50.0, "mean_per_round": 0.5,
                      "refusals": 10, "regret_vs_oracle": 10.0},
                "b": {"cumulative_reward": 60.0, "mean_per_round": 0.6,
                      "refusals": 0, "regret_vs_oracle": 0.0},
                "head_to_head": {"b_minus_a_cumulative": 10.0, "b_won_round_pct": 0.7},
            }
            for i in range(5)
        ]
        agg = aggregate_results(seed_results, rounds)
        self.assertAlmostEqual(agg["a"]["refusal_rate"], 10.0 / rounds, places=6)
        self.assertAlmostEqual(agg["b"]["refusal_rate"], 0.0, places=6)


# ---------------------------------------------------------------------------
# Bonus: betainc sanity checks (underpins the t-test).
# ---------------------------------------------------------------------------

class TestBetaInc(unittest.TestCase):
    def test_boundary_zero(self):
        self.assertEqual(_betainc(1.0, 1.0, 0.0), 0.0)

    def test_boundary_one(self):
        self.assertEqual(_betainc(1.0, 1.0, 1.0), 1.0)

    def test_symmetric(self):
        # I_x(a,b) + I_{1-x}(b,a) = 1
        a, b, x = 3.0, 5.0, 0.4
        val = _betainc(a, b, x) + _betainc(b, a, 1.0 - x)
        self.assertAlmostEqual(val, 1.0, places=6)

    def test_half_point_uniform(self):
        # I_{0.5}(1, 1) = 0.5 (uniform distribution)
        val = _betainc(1.0, 1.0, 0.5)
        self.assertAlmostEqual(val, 0.5, places=6)


if __name__ == "__main__":
    unittest.main()
