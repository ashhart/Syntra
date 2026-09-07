"""Unit tests for syntra_llm. Run with: pytest tests/"""
from __future__ import annotations

from unittest.mock import MagicMock, patch

import requests as r

from syntra_llm import LLMRouter, Model, _RequestTracker


def test_request_tracker_neutral_features_when_empty():
    t = _RequestTracker()
    f = t.features("unknown_tier")
    assert f["avg_quality"] == 0.5
    assert f["avg_latency_ms"] == 1000.0
    assert f["avg_cost_usd"] == 0.01


def test_request_tracker_windowed_averages():
    t = _RequestTracker()
    t.record("pro", quality=1.0, latency_ms=500.0, cost_usd=0.02)
    t.record("pro", quality=0.0, latency_ms=1500.0, cost_usd=0.04)
    f = t.features("pro")
    assert abs(f["avg_quality"] - 0.5) < 0.01
    assert abs(f["avg_latency_ms"] - 1000.0) < 0.01
    assert abs(f["avg_cost_usd"] - 0.03) < 0.001


def test_model_from_option():
    assert Model.from_option(0).name == "cheap_fast"
    assert Model.from_option(1).name == "balanced"
    assert Model.from_option(2).name == "expensive_accurate"
    assert Model.from_option(99).name == "cheap_fast"  # OOB falls back to index 0


@patch("syntra_llm.http_lib.post")
def test_choose_and_report_round_trip(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_abc123",
        "decisions": [{"chosen_option": 2}],  # expensive_accurate
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

    router = LLMRouter("http://test", "/path", "key")
    decision = router.choose(
        prompt_token_count=4000,
        task_complexity=0.9,
        customer_tier="enterprise",
    )
    assert decision.model_name == "expensive_accurate"
    assert decision.decision_id == "dec_abc123"

    router.report(
        decision_id=decision.decision_id,
        model_name=decision.model_name,
        quality=0.95,
        latency_ms=1800.0,
        cost_usd=0.05,
    )

    # Both /decide and /feedback must have been called.
    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/decide") for p in paths)
    assert any(p.endswith("/feedback") for p in paths)


@patch("syntra_llm.http_lib.post")
def test_choose_falls_back_when_syntra_down(mock_post):
    mock_post.side_effect = r.ConnectionError("can't reach syntra")

    fallback = Model("my_fallback", "custom fallback model")
    router = LLMRouter("http://test", "/path", "key", fallback_model=fallback)
    decision = router.choose(
        prompt_token_count=100,
        task_complexity=0.3,
        customer_tier="free",
    )
    assert decision.model_name == "my_fallback"
    assert decision.decision_id is None
    # Only the decide attempt was made; no feedback call.
    assert mock_post.call_count == 1


@patch("syntra_llm.http_lib.post")
def test_choose_uses_fallback_on_refusal(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_xyz",
        "refused": True,
        "refusalReason": "ood",
    }
    decide_resp.raise_for_status = MagicMock()
    mock_post.return_value = decide_resp

    router = LLMRouter("http://test", "/path", "key")
    decision = router.choose(
        prompt_token_count=500,
        task_complexity=0.5,
        customer_tier="pro",
    )
    # Fallback model is used.
    assert decision.model_name == "balanced"
    # decisionId is still returned from the refusal response.
    assert decision.decision_id == "dec_xyz"

    # /feedback is posted so the bandit's audit log records this attempt.
    router.report(
        decision_id=decision.decision_id,
        model_name=decision.model_name,
        quality=0.7,
        latency_ms=600.0,
        cost_usd=0.01,
    )
    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/feedback") for p in paths)


@patch("syntra_llm.http_lib.post")
def test_report_failure_does_not_break_caller(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_1",
        "decisions": [{"chosen_option": 1}],
        "refused": False,
    }
    decide_resp.raise_for_status = MagicMock()

    def post_side_effect(url, **kwargs):
        if "/decide" in url:
            return decide_resp
        raise r.ConnectionError("feedback endpoint down")

    mock_post.side_effect = post_side_effect

    router = LLMRouter("http://test", "/path", "key")
    decision = router.choose(
        prompt_token_count=800,
        task_complexity=0.4,
        customer_tier="pro",
    )
    assert decision.model_name == "balanced"

    # report must not raise even though the /feedback POST fails.
    router.report(
        decision_id=decision.decision_id,
        model_name=decision.model_name,
        quality=0.8,
        latency_ms=900.0,
        cost_usd=0.015,
    )
