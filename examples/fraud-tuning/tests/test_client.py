"""Unit tests for syntra_fraud. Run with: pytest tests/"""
from __future__ import annotations

from unittest.mock import MagicMock, patch

import requests as r

from syntra_fraud import FraudClient, Threshold, _MerchantTracker


def test_merchant_tracker_neutral_features_when_empty():
    t = _MerchantTracker()
    f = t.features("unknown_merchant")
    assert f["recent_fraud_rate"] == 0.0
    assert f["transaction_volume_per_hour"] == 0.0
    assert f["avg_ticket_size_usd"] == 0.0
    assert 0 <= f["hour"] < 24


def test_merchant_tracker_computes_fraud_rate():
    t = _MerchantTracker()
    for _ in range(8):
        t.record("merch_001", was_fraud=False, ticket_size_usd=100.0)
    for _ in range(2):
        t.record("merch_001", was_fraud=True, ticket_size_usd=500.0)
    f = t.features("merch_001")
    assert abs(f["recent_fraud_rate"] - 0.2) < 0.01


def test_threshold_from_option():
    assert Threshold.from_option(0).value == 0.5
    assert Threshold.from_option(1).value == 0.6
    assert Threshold.from_option(2).value == 0.7
    assert Threshold.from_option(3).value == 0.8
    assert Threshold.from_option(4).value == 0.9
    assert Threshold.from_option(99).value == 0.7  # OOB falls back to index 2 (0.7)


@patch("syntra_fraud.http_lib.post")
def test_score_and_report_round_trip(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_fraud_abc",
        "decisions": [{"chosen_option": 2}],  # block_at_0_7
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

    client = FraudClient("http://test", "/path", "key")

    # Risk score above threshold -> blocked.
    decision = client.score({
        "merchant_id": "merch_42",
        "risk_score": 0.85,
        "ticket_size_usd": 300.0,
    })
    assert decision.threshold == 0.7
    assert decision.block is True
    assert decision.decision_id == "dec_fraud_abc"

    client.report_outcome(
        decision.decision_id,
        was_fraud=True,
        merchant_id="merch_42",
        ticket_size_usd=300.0,
        blocked=True,
    )

    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/decide") for p in paths)
    assert any(p.endswith("/feedback") for p in paths)


@patch("syntra_fraud.http_lib.post")
def test_score_falls_back_when_syntra_down(mock_post):
    mock_post.side_effect = r.ConnectionError("can't reach syntra")

    client = FraudClient("http://test", "/path", "key", fallback_threshold=0.6)

    decision = client.score({
        "merchant_id": "merch_fallback",
        "risk_score": 0.65,
        "ticket_size_usd": 100.0,
    })
    # Used fallback threshold of 0.6; 0.65 > 0.6 -> block.
    assert decision.threshold == 0.6
    assert decision.block is True
    assert decision.decision_id is None
    # Only one post attempt (the /decide that failed), no feedback.
    assert mock_post.call_count == 1


@patch("syntra_fraud.http_lib.post")
def test_score_uses_fallback_on_refusal(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_refused",
        "refused": True,
        "refusalReason": "ood",
    }
    decide_resp.raise_for_status = MagicMock()
    mock_post.return_value = decide_resp

    client = FraudClient("http://test", "/path", "key")

    decision = client.score({
        "merchant_id": "merch_ood",
        "risk_score": 0.55,
        "ticket_size_usd": 50.0,
    })
    # Fallback threshold is 0.7; 0.55 <= 0.7 -> allow.
    assert decision.threshold == 0.7
    assert decision.block is False
    # Refusal still returns a decisionId for audit; report_outcome should
    # post feedback so the bandit's audit log records this attempt.
    client.report_outcome(
        decision.decision_id,
        was_fraud=False,
        merchant_id="merch_ood",
        ticket_size_usd=50.0,
        blocked=False,
    )
    paths = [call.args[0] for call in mock_post.call_args_list]
    assert any(p.endswith("/feedback") for p in paths)


@patch("syntra_fraud.http_lib.post")
def test_report_failure_doesnt_break_calling_flow(mock_post):
    decide_resp = MagicMock()
    decide_resp.json.return_value = {
        "decisionId": "dec_2",
        "decisions": [{"chosen_option": 2}],
        "refused": False,
    }
    decide_resp.raise_for_status = MagicMock()

    def post_side_effect(url, **kwargs):
        if "/decide" in url:
            return decide_resp
        raise r.ConnectionError("feedback failed")

    mock_post.side_effect = post_side_effect

    client = FraudClient("http://test", "/path", "key")
    decision = client.score({
        "merchant_id": "merch_err",
        "risk_score": 0.4,
        "ticket_size_usd": 80.0,
    })
    assert decision.threshold == 0.7
    assert decision.block is False

    # report_outcome must not raise even when feedback POST fails.
    client.report_outcome(
        decision.decision_id,
        was_fraud=False,
        merchant_id="merch_err",
        ticket_size_usd=80.0,
        blocked=False,
    )
