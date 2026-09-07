"""Unit tests for syntra_queue. Run with: pytest tests/"""
from __future__ import annotations

from unittest.mock import MagicMock, patch

import requests as r

from syntra_queue import Backend, BackendPick, QueueClient, _BackendTracker


# ---------------------------------------------------------------------------
# 1. Tracker neutral features when empty
# ---------------------------------------------------------------------------

def test_backend_tracker_neutral_features_when_empty():
    t = _BackendTracker()
    avg_lat, err_rate = t.features("backend_a")
    assert avg_lat == 500.0
    assert err_rate == 0.5


# ---------------------------------------------------------------------------
# 2. Tracker computes error-rate + avg latency over rolling window
# ---------------------------------------------------------------------------

def test_backend_tracker_computes_stats():
    t = _BackendTracker()
    for _ in range(8):
        t.record("backend_a", success=True, latency_ms=100.0)
    for _ in range(2):
        t.record("backend_a", success=False, latency_ms=2000.0)

    avg_lat, err_rate = t.features("backend_a")
    # 8 * 100 + 2 * 2000 = 4800 / 10 = 480
    assert abs(avg_lat - 480.0) < 0.01
    assert abs(err_rate - 0.2) < 0.01


# ---------------------------------------------------------------------------
# 3. Backend.from_option lookup with OOB fallback
# ---------------------------------------------------------------------------

def test_backend_from_option():
    assert Backend.from_option(0).name == "backend_a"
    assert Backend.from_option(1).name == "backend_b"
    assert Backend.from_option(2).name == "backend_c"
    assert Backend.from_option(99).name == "backend_a"  # OOB falls back to index 0


# ---------------------------------------------------------------------------
# 4. Successful pick+report round-trip (/decide AND /feedback called)
# ---------------------------------------------------------------------------

@patch("syntra_queue.http_lib.post")
def test_pick_and_report_round_trip(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_abc123",
        "decisions": [{"chosen_option": 1}],  # "backend_b"
        "refused": False,
    }
    decide_resp.raise_for_status = MagicMock()

    feedback_resp = MagicMock()
    feedback_resp.raise_for_status = MagicMock()

    def post_side_effect(url, **kwargs):
        if "/decide" in url:
            return decide_resp
        return feedback_resp

    mock_post.side_effect = post_side_effect

    client = QueueClient("http://test", "/path", "key")
    pick = client.pick(request_size_kb=10.0, queue_depths={"backend_a": 5, "backend_b": 2, "backend_c": 8})

    assert pick.backend_name == "backend_b"
    assert pick.decision_id == "dec_abc123"

    client.report(pick.decision_id, pick.backend_name, success=True, latency_ms=50.0)

    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/decide") for p in paths)
    assert any(p.endswith("/feedback") for p in paths)


# ---------------------------------------------------------------------------
# 5. pick falls back to round-robin when Syntra is down
# ---------------------------------------------------------------------------

@patch("syntra_queue.http_lib.post")
def test_pick_falls_back_to_round_robin_when_syntra_down(mock_post):
    mock_post.side_effect = r.ConnectionError("can't reach syntra")

    client = QueueClient("http://test", "/path", "key")
    picks = [client.pick(request_size_kb=1.0, queue_depths={}) for _ in range(3)]

    # No decisionId when Syntra is unreachable.
    assert all(p.decision_id is None for p in picks)
    # Round-robin cycles through backends.
    names = [p.backend_name for p in picks]
    assert names == ["backend_a", "backend_b", "backend_c"]


# ---------------------------------------------------------------------------
# 6. pick uses round-robin on refusal (still posts /feedback for audit)
# ---------------------------------------------------------------------------

@patch("syntra_queue.http_lib.post")
def test_pick_round_robins_on_refusal_and_posts_feedback(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_xyz",
        "refused": True,
        "refusalReason": "ood",
    }
    decide_resp.raise_for_status = MagicMock()
    mock_post.return_value = decide_resp

    client = QueueClient("http://test", "/path", "key")
    pick = client.pick(request_size_kb=1.0, queue_depths={})

    # Refusal still yields a decisionId for the audit log.
    assert pick.decision_id == "dec_xyz"

    # report() should fire /feedback with that decisionId.
    client.report(pick.decision_id, pick.backend_name, success=True, latency_ms=20.0)

    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/feedback") for p in paths)


# ---------------------------------------------------------------------------
# 7. report failure doesn't break the calling flow
# ---------------------------------------------------------------------------

@patch("syntra_queue.http_lib.post")
def test_report_failure_doesnt_break_flow(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_1",
        "decisions": [{"chosen_option": 0}],
        "refused": False,
    }
    decide_resp.raise_for_status = MagicMock()

    def post_side_effect(url, **kwargs):
        if "/decide" in url:
            return decide_resp
        raise r.ConnectionError("feedback failed")

    mock_post.side_effect = post_side_effect

    client = QueueClient("http://test", "/path", "key")
    pick = client.pick(request_size_kb=5.0, queue_depths={})

    # report must not raise even when the feedback POST fails.
    client.report(pick.decision_id, pick.backend_name, success=True, latency_ms=30.0)

    assert pick.backend_name == "backend_a"
