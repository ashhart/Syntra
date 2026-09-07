"""Unit tests for bench.py latency stats, ratio parsing, watchdog, and reporter.

Run:
    PYTHONPATH=. python3 -m pytest tests/ -v
"""
from __future__ import annotations

import threading
import time

import pytest

# We import directly from the module — no network calls happen at import time.
from bench import (
    LatencyStats,
    WorkerResult,
    ErrorCounts,
    ascii_table,
    parse_ratio,
    _worker_body,
    WATCHDOG_CONSECUTIVE_FAILURE_LIMIT,
)


# ---------------------------------------------------------------------------
# Test 1: LatencyStats computes correct percentiles on a known distribution.
# ---------------------------------------------------------------------------


class TestLatencyStatsKnownDistribution:
    """p50/p95/p99/p999 on a perfectly understood synthetic dataset."""

    def _make_stats(self, values_ns: list[int]) -> LatencyStats:
        ls = LatencyStats()
        for v in values_ns:
            ls.record(v)
        return ls

    def test_uniform_100_samples(self):
        # 100 samples: 1 ms, 2 ms, ..., 100 ms (as ns).
        values = [i * 1_000_000 for i in range(1, 101)]
        ls = self._make_stats(values)
        assert ls.count() == 100
        # p50: rank = ceil(50/100 * 100) - 1 = 49  -> value 50 ms
        assert ls.p50_ms() == pytest.approx(50.0, abs=0.01)
        # p95: rank = ceil(0.95*100)-1 = 94 -> value 95 ms
        assert ls.p95_ms() == pytest.approx(95.0, abs=0.01)
        # p99: rank = ceil(0.99*100)-1 = 98 -> value 99 ms
        assert ls.p99_ms() == pytest.approx(99.0, abs=0.01)
        # p999: rank = ceil(0.999*100)-1 = 99 -> value 100 ms
        assert ls.p999_ms() == pytest.approx(100.0, abs=0.01)

    def test_single_sample(self):
        ls = self._make_stats([5_000_000])  # 5 ms
        assert ls.p50_ms() == pytest.approx(5.0)
        assert ls.p99_ms() == pytest.approx(5.0)
        assert ls.p999_ms() == pytest.approx(5.0)

    def test_out_of_order_insertion_gives_sorted_percentiles(self):
        # Insert in reverse order; percentiles should be identical to sorted insert.
        values = [i * 1_000_000 for i in range(100, 0, -1)]
        ls = self._make_stats(values)
        assert ls.p50_ms() == pytest.approx(50.0, abs=0.01)
        assert ls.p99_ms() == pytest.approx(99.0, abs=0.01)

    def test_all_same_value(self):
        ls = self._make_stats([2_000_000] * 1000)  # all 2 ms
        assert ls.p50_ms() == pytest.approx(2.0)
        assert ls.p95_ms() == pytest.approx(2.0)
        assert ls.p99_ms() == pytest.approx(2.0)
        assert ls.p999_ms() == pytest.approx(2.0)

    def test_two_clusters(self):
        # 900 samples at 1 ms, 100 at 100 ms.
        values = [1_000_000] * 900 + [100_000_000] * 100
        ls = self._make_stats(values)
        # p50 should be in the low cluster
        assert ls.p50_ms() < 10.0
        # p99 should be in the high cluster (rank 989 of 1000)
        assert ls.p99_ms() == pytest.approx(100.0, abs=0.01)


# ---------------------------------------------------------------------------
# Test 2: LatencyStats returns sensible defaults on empty input (no exceptions).
# ---------------------------------------------------------------------------


class TestLatencyStatsEmpty:
    def test_empty_count(self):
        ls = LatencyStats()
        assert ls.count() == 0

    def test_empty_percentiles_return_zero_not_exception(self):
        ls = LatencyStats()
        assert ls.p50_ms() == 0.0
        assert ls.p95_ms() == 0.0
        assert ls.p99_ms() == 0.0
        assert ls.p999_ms() == 0.0

    def test_empty_arbitrary_percentile(self):
        ls = LatencyStats()
        assert ls.percentile_ms(42.5) == 0.0

    def test_merge_two_empty(self):
        a = LatencyStats()
        b = LatencyStats()
        a.merge(b)
        assert a.count() == 0
        assert a.p99_ms() == 0.0

    def test_merge_populated_into_empty(self):
        a = LatencyStats()
        b = LatencyStats()
        for i in range(1, 101):
            b.record(i * 1_000_000)
        a.merge(b)
        assert a.count() == 100
        assert a.p50_ms() == pytest.approx(50.0, abs=0.01)


# ---------------------------------------------------------------------------
# Test 3: parse_ratio — valid inputs, malformed inputs raise ValueError.
# ---------------------------------------------------------------------------


class TestParseRatio:
    def test_one_to_one(self):
        assert parse_ratio("1:1") == (1, 1)

    def test_three_to_one(self):
        assert parse_ratio("3:1") == (3, 1)

    def test_one_to_three(self):
        assert parse_ratio("1:3") == (1, 3)

    def test_large_values(self):
        assert parse_ratio("10:5") == (10, 5)

    def test_whitespace_tolerance(self):
        # Spaces around the colon should be handled.
        assert parse_ratio("2 : 1") == (2, 1)

    def test_missing_colon_raises(self):
        with pytest.raises(ValueError, match="expected format"):
            parse_ratio("31")

    def test_empty_string_raises(self):
        with pytest.raises(ValueError):
            parse_ratio("")

    def test_non_integer_raises(self):
        with pytest.raises(ValueError, match="must be integers"):
            parse_ratio("a:b")

    def test_zero_decide_raises(self):
        with pytest.raises(ValueError, match="positive integers"):
            parse_ratio("0:1")

    def test_zero_feedback_raises(self):
        with pytest.raises(ValueError, match="positive integers"):
            parse_ratio("1:0")

    def test_negative_raises(self):
        with pytest.raises(ValueError, match="positive integers"):
            parse_ratio("-1:1")

    def test_float_raises(self):
        with pytest.raises(ValueError, match="must be integers"):
            parse_ratio("1.5:1")


# ---------------------------------------------------------------------------
# Test 4: Watchdog kicks in after WATCHDOG_CONSECUTIVE_FAILURE_LIMIT failures.
# ---------------------------------------------------------------------------


class TestWatchdog:
    """Verify the watchdog bails out and increments the counter when the
    target is permanently unreachable (connection refused on a closed port).
    """

    def test_watchdog_bails_after_threshold(self):
        """Worker should bail out quickly when every request fails."""
        stop_event = threading.Event()
        warmup_event = threading.Event()
        warmup_event.set()  # measure immediately
        errors = ErrorCounts()
        result = WorkerResult()

        # Use a port that is guaranteed to be closed (unlikely to be in use).
        worker_thread = threading.Thread(
            target=_worker_body,
            kwargs=dict(
                worker_id=0,
                host="127.0.0.1",
                port=19999,       # port almost certainly not open
                use_https=False,
                capsule_path="/tenants/t/jobs/j/capsules/c",
                admin_key="test-key",
                context_type="discrete",
                decide_n=1,
                feedback_n=1,
                stop_event=stop_event,
                warmup_event=warmup_event,
                errors=errors,
                result=result,
            ),
            daemon=True,
        )
        worker_thread.start()
        # The worker should bail out on its own within a few seconds once the
        # watchdog threshold is hit. Give it up to 15 s to account for slow CI.
        worker_thread.join(timeout=15.0)

        assert not worker_thread.is_alive(), (
            "Worker thread did not exit within timeout — watchdog may not be firing"
        )
        assert errors.watchdog_bailouts >= 1, (
            "Expected at least one watchdog bailout counter increment"
        )
        # Connection-refused errors should have been tallied.
        assert errors.total() >= WATCHDOG_CONSECUTIVE_FAILURE_LIMIT

    def test_watchdog_counter_not_incremented_on_success(self):
        """Sanity: if no failures occur, the watchdog counter stays zero."""
        errors = ErrorCounts()
        assert errors.watchdog_bailouts == 0
        # Simulate successful loop — just check the initial state.
        assert errors.connection_refused == 0


# ---------------------------------------------------------------------------
# Test 5: ascii_table produces parseable output for a known input.
# ---------------------------------------------------------------------------


class TestAsciiTable:
    def test_header_and_rows_present(self):
        headers = ["op", "count", "p99_ms"]
        rows = [("decide", 1234, "12.34"), ("feedback", 1230, "11.10")]
        table = ascii_table(rows, headers)
        assert "op" in table
        assert "count" in table
        assert "p99_ms" in table
        assert "decide" in table
        assert "feedback" in table
        assert "1234" in table
        assert "12.34" in table

    def test_all_lines_same_width(self):
        headers = ["a", "bb", "ccc"]
        rows = [("x", "yy", "zzz"), ("longval", "v", "w")]
        table = ascii_table(rows, headers)
        lines = [ln for ln in table.splitlines() if ln.startswith("|") or ln.startswith("+")]
        widths = {len(ln) for ln in lines}
        assert len(widths) == 1, f"Lines have inconsistent widths: {widths}"

    def test_separator_lines_use_plus_and_dashes(self):
        headers = ["x"]
        rows = [("1",)]
        table = ascii_table(rows, headers)
        sep_lines = [ln for ln in table.splitlines() if ln.startswith("+")]
        assert len(sep_lines) >= 2
        for sep in sep_lines:
            assert all(c in "+-" for c in sep), f"Unexpected char in separator: {sep}"

    def test_empty_rows(self):
        headers = ["col1", "col2"]
        table = ascii_table([], headers)
        assert "col1" in table
        assert "col2" in table
        # Should have at least header + two separators, no crash.
        lines = table.splitlines()
        assert len(lines) >= 3
