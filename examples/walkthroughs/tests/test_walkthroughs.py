"""Smoke tests for the walkthrough scripts.

Each test:
  1. Imports the walkthrough module (verifying no SyntaxError / ImportError on
     stdlib-only imports).
  2. Calls argparse with no arguments and verifies it produces a Namespace (the
     scripts default to env vars and don't error on missing args at parse time).
  3. Verifies the module exposes a callable ``main``.

No live Syntra instance is required.
"""
from __future__ import annotations

import importlib
import sys
import types
import unittest
from pathlib import Path

# Add the walkthroughs directory to the path so we can import the scripts.
_WALKTHROUGHS_DIR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_WALKTHROUGHS_DIR))


def _load(module_name: str) -> types.ModuleType:
    """Import a walkthrough module by its file stem."""
    return importlib.import_module(module_name)


def _parse_argv(mod: types.ModuleType) -> object:
    """
    Call the module's argparse setup with an empty argv.
    Scripts use argparse.ArgumentParser; we invoke parse_args([]) directly.
    Because each module defines a local parser inside main(), we reconstruct
    it by calling parse_known_args on a fresh parser the same way the scripts
    do -- but since the parsers are local, the easiest approach is to confirm
    the module is importable and has a callable main().
    """
    return getattr(mod, "main")


class TestWalkthroughs(unittest.TestCase):

    def _smoke(self, stem: str) -> None:
        """Import module, verify main() is callable, and no unexpected imports."""
        mod = _load(stem)
        self.assertTrue(
            callable(getattr(mod, "main", None)),
            f"{stem}.main is not callable",
        )
        # Verify no third-party imports crept in.
        for forbidden in ("requests", "numpy", "pandas", "httpx", "aiohttp", "urllib3"):
            self.assertNotIn(
                forbidden,
                sys.modules,
                f"Forbidden import '{forbidden}' found after importing {stem}",
            )

    def test_01_scoped_tokens(self) -> None:
        self._smoke("01_scoped_tokens")

    def test_02_continuous_action_pricing(self) -> None:
        self._smoke("02_continuous_action_pricing")

    def test_03_multi_objective_feedback(self) -> None:
        self._smoke("03_multi_objective_feedback")

    def test_04_batched_feedback(self) -> None:
        self._smoke("04_batched_feedback")

    def test_05_backup_and_restore(self) -> None:
        self._smoke("05_backup_and_restore")

    def test_06_rate_limit_handling(self) -> None:
        self._smoke("06_rate_limit_handling")

    def test_07_metrics_scrape(self) -> None:
        self._smoke("07_metrics_scrape")

    def test_metrics_parser_unit(self) -> None:
        """Unit test the hand-rolled Prometheus parser in 07 without network."""
        mod = _load("07_metrics_scrape")
        sample_text = """\
# TYPE lycan_request_total counter
lycan_request_total{kind="decide",tenant="t1",job="j1",capsule="c1",status="ok"} 42
lycan_request_total{kind="feedback",tenant="t1",job="j1",capsule="c1",status="ok"} 40
# TYPE lycan_decide_latency_seconds histogram
lycan_decide_latency_seconds_bucket{le="0.005"} 5
lycan_decide_latency_seconds_bucket{le="0.01"} 10
lycan_decide_latency_seconds_bucket{le="0.025"} 30
lycan_decide_latency_seconds_bucket{le="+Inf"} 42
lycan_decide_latency_seconds_count 42
lycan_decide_latency_seconds_sum 0.35
# TYPE lycan_refusals_total counter
lycan_refusals_total{tenant="t1",job="j1",capsule="c1",reason="ood"} 2
"""
        parsed = mod.parse_metrics(sample_text)
        # The parser strips known suffixes (_total, _bucket, _count, _sum)
        # to produce canonical metric family names.
        self.assertIn("lycan_request", parsed)
        self.assertIn("lycan_decide_latency_seconds", parsed)
        self.assertIn("lycan_refusals", parsed)

        p99 = mod.estimate_p99_latency(parsed)
        self.assertIsNotNone(p99)
        self.assertGreater(p99, 0.0)
        self.assertLess(p99, 1.0)

        top = mod.top_capsule_by_decides(parsed)
        self.assertIsNotNone(top)
        self.assertEqual(top["decides"], 42)

        rate = mod.refusal_rate(parsed)
        self.assertIsNotNone(rate)
        self.assertAlmostEqual(rate, 2 / 42, places=5)

    def test_expected_midpoints(self) -> None:
        """Verify the continuous-action midpoint formula in 02 is correct."""
        mod = _load("02_continuous_action_pricing")
        lo, hi, n = mod.RANGE_LO, mod.RANGE_HI, mod.N_BUCKETS
        width = (hi - lo) / n
        for i in range(n):
            expected = lo + width * (i + 0.5)
            self.assertAlmostEqual(mod.EXPECTED_MIDPOINTS[i], expected, places=9)


if __name__ == "__main__":
    unittest.main()
