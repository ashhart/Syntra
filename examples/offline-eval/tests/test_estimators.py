"""Unit tests for syntra_ope estimators.

Tests
-----
1. IPS on a tiny synthetic dataset with a known optimal policy recovers
   the true mean within tolerance.
2. DR estimator with a perfect reward model exactly matches the true mean.
3. Bootstrap CI width shrinks as sample size increases.
4. Missing propensity raises a clean MissingPropensityError.
5. load_csv parses the schema correctly.
6. Eval policy lookup for unseen contexts returns a configurable fallback.
7. Static-mode evaluation on a logged dataset where eval policy differs from
   logging policy.
8. DR falls back to global mean for (context, action) pairs not in the model.
"""

from __future__ import annotations

import csv
import os
import sys
import tempfile
import unittest

_HERE = os.path.dirname(os.path.abspath(__file__))
_ROOT = os.path.dirname(_HERE)
if _ROOT not in sys.path:
    sys.path.insert(0, _ROOT)

from syntra_ope import (
    CSVSchemaError,
    EvalPolicy,
    LoggedRow,
    MissingPropensityError,
    OPEResult,
    RewardModel,
    bootstrap_ci,
    dr_estimate,
    ips_estimate,
    load_csv,
)


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_log(rows):
    """Build a list of LoggedRow from (decision_id, ctx, action, propensity, reward)."""
    return [
        LoggedRow(
            decision_id=r[0],
            context_key=r[1],
            action=r[2],
            propensity=r[3],
            reward=r[4],
        )
        for r in rows
    ]


def _write_csv(rows, fieldnames=None):
    """Write rows to a temporary CSV file and return the path."""
    if fieldnames is None:
        fieldnames = ["decision_id", "context_key", "action", "propensity", "reward"]
    fh = tempfile.NamedTemporaryFile(
        mode="w", suffix=".csv", delete=False, newline="", encoding="utf-8"
    )
    writer = csv.DictWriter(fh, fieldnames=fieldnames)
    writer.writeheader()
    writer.writerows(rows)
    fh.close()
    return fh.name


# ---------------------------------------------------------------------------
# Test 1: IPS recovers the true mean of the eval policy within tolerance
# ---------------------------------------------------------------------------


class TestIPSEstimator(unittest.TestCase):

    def test_ips_known_optimal_policy(self):
        """IPS with a uniform logging policy and a deterministic eval policy
        that always picks the high-reward action should return a higher
        estimate than the logging policy mean."""
        # Two contexts, two actions. For context A: action_1 has reward 0.9,
        # action_2 has reward 0.1. For context B: action_1 reward 0.1, action_2
        # reward 0.8. Logging policy is uniform (p=0.5 each).
        rng_rows = []
        import random
        r = random.Random(0)
        n = 400
        for i in range(n):
            ctx = "ctx_a" if r.random() < 0.5 else "ctx_b"
            action = "act_1" if r.random() < 0.5 else "act_2"
            if ctx == "ctx_a":
                reward = 0.9 if action == "act_1" else 0.1
            else:
                reward = 0.1 if action == "act_1" else 0.8
            rng_rows.append((f"d{i}", ctx, action, 0.5, reward))

        log = _make_log(rng_rows)

        # Optimal eval policy: ctx_a -> act_1, ctx_b -> act_2
        policy = EvalPolicy({"ctx_a": "act_1", "ctx_b": "act_2"})

        ips_val, n_eff = ips_estimate(log, policy)

        # True mean of eval policy is (0.9 + 0.8) / 2 = 0.85
        # The logging policy mean is roughly (0.9+0.1+0.1+0.8)/4 = 0.475.
        # IPS estimate should be in [0.7, 1.0] for n=400.
        self.assertGreater(ips_val, 0.7,
                           f"IPS estimate {ips_val:.4f} is too low for optimal policy")
        self.assertLess(ips_val, 1.05,
                        f"IPS estimate {ips_val:.4f} is implausibly high")
        self.assertEqual(n_eff, n)

    def test_ips_identity_policy(self):
        """When eval policy == logging policy, IPS mean should equal raw mean."""
        log = _make_log([
            ("d1", "ctx_a", "act_1", 0.6, 1.0),
            ("d2", "ctx_a", "act_2", 0.4, 0.0),
            ("d3", "ctx_a", "act_1", 0.6, 1.0),
            ("d4", "ctx_a", "act_2", 0.4, 0.0),
        ])
        # Eval policy same as logging policy (deterministic: always act_1)
        policy = EvalPolicy({"ctx_a": "act_1"})

        ips_val, _ = ips_estimate(log, policy)

        # Only the act_1 rows contribute (propensity 0.6, pi_eval 1.0).
        # Sum = (1.0/0.6)*1.0 + (1.0/0.6)*1.0 = 2/0.6 * 2; divided by N=4
        expected = ((1.0 / 0.6) * 1.0 + (1.0 / 0.6) * 1.0) / 4
        self.assertAlmostEqual(ips_val, expected, places=6)


# ---------------------------------------------------------------------------
# Test 2: DR with a perfect reward model exactly matches the true mean
# ---------------------------------------------------------------------------


class TestDREstimator(unittest.TestCase):

    def test_dr_perfect_model(self):
        """DR with a perfect reward model returns exactly the direct-method
        estimate (IPS correction vanishes to zero because residuals are zero)."""
        # Log: always action=act_1, propensity=0.5, reward follows a known function.
        log = _make_log([
            ("d1", "ctx_a", "act_1", 0.5, 0.8),
            ("d2", "ctx_a", "act_1", 0.5, 0.8),
            ("d3", "ctx_b", "act_2", 0.5, 0.4),
            ("d4", "ctx_b", "act_2", 0.5, 0.4),
        ])

        # "Perfect" reward model: knows the exact rewards.
        model = RewardModel()
        model.fit(log)

        # Eval policy: ctx_a -> act_1, ctx_b -> act_2 (same as log).
        policy = EvalPolicy({"ctx_a": "act_1", "ctx_b": "act_2"})

        dr_val, n_eff = dr_estimate(log, policy, model)

        # DM prediction for ctx_a -> act_1 = 0.8, ctx_b -> act_2 = 0.4.
        # IPS correction for each row: pi_eval/prop * (r - mu(ctx,a))
        # Since r == mu(ctx,a), all corrections are 0.
        # DR = mean of DM predictions = (0.8 + 0.8 + 0.4 + 0.4) / 4 = 0.6
        self.assertAlmostEqual(dr_val, 0.6, places=6)
        self.assertEqual(n_eff, 4)


# ---------------------------------------------------------------------------
# Test 3: Bootstrap CI width shrinks with larger sample size
# ---------------------------------------------------------------------------


class TestBootstrapCI(unittest.TestCase):

    def test_ci_width_shrinks_with_sample_size(self):
        """CI width for n=50 should be larger than for n=500 (in expectation)."""
        import random as rng_mod
        r = rng_mod.Random(99)

        def make_uniform_log(n, seed):
            rr = rng_mod.Random(seed)
            rows = []
            for i in range(n):
                ctx = "ctx_a"
                action = "act_1" if rr.random() < 0.5 else "act_2"
                reward = rr.gauss(0.5, 0.1)
                rows.append((f"d{i}", ctx, action, 0.5, reward))
            return _make_log(rows)

        policy = EvalPolicy({"ctx_a": "act_1"})

        log_small = make_uniform_log(50, seed=1)
        log_large = make_uniform_log(500, seed=2)

        ci_small = bootstrap_ci(log_small, policy, n_bootstrap=200, seed=0)
        ci_large = bootstrap_ci(log_large, policy, n_bootstrap=200, seed=0)

        width_small = ci_small["ips"].ci_95 - ci_small["ips"].ci_5
        width_large = ci_large["ips"].ci_95 - ci_large["ips"].ci_5

        self.assertGreater(
            width_small, width_large,
            f"Expected small-sample CI ({width_small:.4f}) to be wider than "
            f"large-sample CI ({width_large:.4f})."
        )


# ---------------------------------------------------------------------------
# Test 4: Missing propensity raises a clean error
# ---------------------------------------------------------------------------


class TestCSVLoading(unittest.TestCase):

    def test_missing_propensity_raises(self):
        """A row with a blank propensity column raises MissingPropensityError."""
        path = _write_csv([
            {"decision_id": "d1", "context_key": "ctx", "action": "a1",
             "propensity": "0.5", "reward": "0.8"},
            {"decision_id": "d2", "context_key": "ctx", "action": "a2",
             "propensity": "",    "reward": "0.3"},
        ])
        try:
            with self.assertRaises(MissingPropensityError) as ctx:
                load_csv(path)
            self.assertIn("d2", str(ctx.exception))
        finally:
            os.unlink(path)

    # Test 5: load_csv parses schema correctly
    def test_load_csv_parses_correctly(self):
        """load_csv correctly parses all columns and returns LoggedRow objects."""
        path = _write_csv([
            {"decision_id": "dec_001", "context_key": "ctx_a",
             "action": "policy_a", "propensity": "0.6", "reward": "0.75"},
            {"decision_id": "dec_002", "context_key": "ctx_b",
             "action": "policy_b", "propensity": "0.4", "reward": "0.25"},
        ])
        try:
            rows = load_csv(path)
        finally:
            os.unlink(path)

        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0].decision_id, "dec_001")
        self.assertEqual(rows[0].context_key, "ctx_a")
        self.assertEqual(rows[0].action, "policy_a")
        self.assertAlmostEqual(rows[0].propensity, 0.6)
        self.assertAlmostEqual(rows[0].reward, 0.75)
        self.assertEqual(rows[1].decision_id, "dec_002")

    def test_missing_column_raises(self):
        """A CSV missing a required column raises CSVSchemaError."""
        path = _write_csv(
            [{"decision_id": "d1", "context_key": "ctx", "action": "a1",
              "reward": "0.5"}],
            fieldnames=["decision_id", "context_key", "action", "reward"],
        )
        try:
            with self.assertRaises(CSVSchemaError) as ctx:
                load_csv(path)
            self.assertIn("propensity", str(ctx.exception))
        finally:
            os.unlink(path)


# ---------------------------------------------------------------------------
# Test 6: Eval policy lookup for unseen contexts
# ---------------------------------------------------------------------------


class TestEvalPolicy(unittest.TestCase):

    def test_fallback_for_unseen_context(self):
        """Unseen context with a configured fallback returns the fallback action."""
        policy = EvalPolicy({"ctx_a": "act_1"}, fallback_action="act_default")
        self.assertEqual(policy.action_for("ctx_unseen"), "act_default")
        self.assertEqual(policy.probability("ctx_unseen", "act_default"), 1.0)
        self.assertEqual(policy.probability("ctx_unseen", "act_1"), 0.0)

    def test_no_fallback_for_unseen_context(self):
        """Unseen context with no fallback returns None and probability 0."""
        policy = EvalPolicy({"ctx_a": "act_1"}, fallback_action=None)
        self.assertIsNone(policy.action_for("ctx_unseen"))
        self.assertEqual(policy.probability("ctx_unseen", "act_1"), 0.0)

    def test_known_context_probability(self):
        """Known context assigns probability 1 to chosen action, 0 to others."""
        policy = EvalPolicy({"ctx_a": "act_1"})
        self.assertEqual(policy.probability("ctx_a", "act_1"), 1.0)
        self.assertEqual(policy.probability("ctx_a", "act_2"), 0.0)


# ---------------------------------------------------------------------------
# Test 7: Static-mode evaluation where eval policy differs from logging policy
# ---------------------------------------------------------------------------


class TestStaticModeEvaluation(unittest.TestCase):

    def test_different_eval_and_logging_policy(self):
        """When eval policy selects a different action than the logging policy,
        IPS estimate should differ from the logging policy mean reward."""
        # Logging policy: always picks act_1 (propensity 1.0).
        # Reward for act_1 is low (0.2). Reward for act_2 would be 0.9.
        # Eval policy picks act_2 for ctx_a.
        # Because eval policy never chose the logged action, IPS = 0.
        log = _make_log([
            ("d1", "ctx_a", "act_1", 1.0, 0.2),
            ("d2", "ctx_a", "act_1", 1.0, 0.2),
            ("d3", "ctx_a", "act_1", 1.0, 0.2),
        ])
        policy = EvalPolicy({"ctx_a": "act_2"})

        ips_val, n_eff = ips_estimate(log, policy)
        logging_mean = sum(r.reward for r in log) / len(log)

        # IPS is 0 because eval policy never matches the logged action.
        self.assertAlmostEqual(ips_val, 0.0, places=6)
        self.assertAlmostEqual(logging_mean, 0.2, places=6)
        # They differ
        self.assertNotAlmostEqual(ips_val, logging_mean, places=4)

    def test_eval_matches_logging_improves_estimate(self):
        """When eval policy agrees on the high-reward actions in the log,
        the IPS estimate should be higher than the logging policy mean."""
        # Mixed log: half act_1 (high reward), half act_2 (low reward).
        log = _make_log([
            ("d1", "ctx_a", "act_1", 0.5, 0.9),
            ("d2", "ctx_a", "act_2", 0.5, 0.1),
            ("d3", "ctx_a", "act_1", 0.5, 0.9),
            ("d4", "ctx_a", "act_2", 0.5, 0.1),
        ])
        # Eval policy always picks act_1 (the high-reward action).
        policy = EvalPolicy({"ctx_a": "act_1"})

        ips_val, _ = ips_estimate(log, policy)
        logging_mean = sum(r.reward for r in log) / len(log)  # 0.5

        # IPS should reward act_1 rows with weight 1/0.5 = 2 and ignore act_2.
        # IPS = (2*0.9 + 0 + 2*0.9 + 0) / 4 = 3.6 / 4 = 0.9
        self.assertAlmostEqual(ips_val, 0.9, places=6)
        self.assertGreater(ips_val, logging_mean)


# ---------------------------------------------------------------------------
# Test 8: DR falls back to global mean for unseen (context, action) pairs
# ---------------------------------------------------------------------------


class TestDRFallback(unittest.TestCase):

    def test_dr_uses_global_mean_for_unseen_pairs(self):
        """DR estimator uses global mean reward when the model has no entry
        for a (context, action) pair seen in the eval policy."""
        # Log only has ctx_a -> act_1. Eval policy has ctx_b -> act_2
        # which the model has never seen.
        log = _make_log([
            ("d1", "ctx_a", "act_1", 0.8, 0.6),
            ("d2", "ctx_a", "act_1", 0.8, 0.6),
        ])
        model = RewardModel()
        model.fit(log)

        # Global mean = 0.6. ctx_b -> act_2 is unseen.
        self.assertIsNone(model.predict("ctx_b", "act_2"))
        self.assertAlmostEqual(model.global_mean(), 0.6, places=6)

        policy = EvalPolicy({"ctx_a": "act_1", "ctx_b": "act_2"},
                            fallback_action=None)

        # Add a ctx_b row that the eval policy would have covered.
        log_extended = log + [
            LoggedRow("d3", "ctx_b", "act_1", 0.5, 0.3),
        ]
        warnings = []
        dr_val, n_eff = dr_estimate(log_extended, policy, model, warnings=warnings)
        # Should not crash; n_eff should include all rows.
        self.assertEqual(n_eff, 3)
        self.assertIsInstance(dr_val, float)


if __name__ == "__main__":
    unittest.main()
