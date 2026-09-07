"""Unit tests for syntra_retry. Run with: pytest tests/"""
from __future__ import annotations

from unittest.mock import MagicMock, patch

import requests as r

from syntra_retry import RetryClient, RetryPolicy, _EndpointTracker


def test_endpoint_tracker_neutral_features_when_empty():
    t = _EndpointTracker()
    f = t.features("unknown.com")
    assert f["recent_failure_rate"] == 0.5
    assert f["p99_latency_ms"] == 1000.0
    assert 0 <= f["hour"] < 24


def test_endpoint_tracker_computes_failure_rate():
    t = _EndpointTracker()
    for _ in range(8):
        t.record("api.example.com", success=True, latency_ms=100)
    for _ in range(2):
        t.record("api.example.com", success=False, latency_ms=2000)
    f = t.features("api.example.com")
    assert abs(f["recent_failure_rate"] - 0.2) < 0.01


def test_retry_policy_from_option():
    assert RetryPolicy.from_option(0).name == "none"
    assert RetryPolicy.from_option(1).name == "single"
    assert RetryPolicy.from_option(2).name == "triple"
    assert RetryPolicy.from_option(99).name == "none"  # OOB falls back to index 0


@patch("syntra_retry.http_lib.post")
@patch("syntra_retry.http_lib.request")
def test_request_uses_syntra_policy(mock_http, mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_abc123",
        "decisions": [{"chosen_option": 2}],  # "triple"
        "refused": False,
    }
    decide_resp.raise_for_status = MagicMock()
    mock_post.return_value = decide_resp

    http_resp = MagicMock()
    http_resp.status_code = 200
    mock_http.return_value = http_resp

    client = RetryClient("http://test", "/path", "key")
    response = client.request("GET", "https://api.example.com/x")
    assert response.status_code == 200

    # Decide was called, then feedback (post called twice total).
    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/decide") for p in paths)
    assert any(p.endswith("/feedback") for p in paths)


@patch("syntra_retry.http_lib.post")
@patch("syntra_retry.http_lib.request")
def test_request_falls_back_when_syntra_down(mock_http, mock_post):
    mock_post.side_effect = r.ConnectionError("can't reach syntra")
    http_resp = MagicMock()
    http_resp.status_code = 200
    mock_http.return_value = http_resp

    fallback = RetryPolicy("custom_fallback", 5, 100, 1.5)
    client = RetryClient("http://test", "/path", "key", fallback_policy=fallback)
    response = client.request("GET", "https://api.example.com/x")
    assert response.status_code == 200
    # No decisionId means feedback was never attempted.
    assert mock_post.call_count == 1  # only the decide attempt


@patch("syntra_retry.http_lib.post")
@patch("syntra_retry.http_lib.request")
def test_request_uses_fallback_on_refusal(mock_http, mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_xyz",
        "refused": True,
        "refusalReason": "ood",
    }
    decide_resp.raise_for_status = MagicMock()
    mock_post.return_value = decide_resp

    http_resp = MagicMock()
    http_resp.status_code = 200
    mock_http.return_value = http_resp

    client = RetryClient("http://test", "/path", "key")
    response = client.request("GET", "https://api.example.com/x")
    assert response.status_code == 200
    # Refusal still carries a decisionId → feedback fires anyway so the
    # bandit's audit log records this attempt was made.
    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/feedback") for p in paths)


@patch("syntra_retry.http_lib.post")
@patch("syntra_retry.http_lib.request")
def test_feedback_failure_doesnt_break_request(mock_http, mock_post):
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

    http_resp = MagicMock()
    http_resp.status_code = 200
    mock_http.return_value = http_resp

    client = RetryClient("http://test", "/path", "key")
    response = client.request("GET", "https://api.example.com/x")
    assert response.status_code == 200
