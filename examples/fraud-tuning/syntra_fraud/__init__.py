"""Syntra-driven fraud threshold selection for transaction scoring.

Usage:
    from syntra_fraud import FraudClient

    client = FraudClient(
        syntra_url="http://localhost:8787",
        capsule_path="/tenants/myteam/jobs/fraud/capsules/threshold",
        admin_key="...",
    )
    decision = client.score({
        "merchant_id": "merch_42",
        "risk_score": 0.73,
        "ticket_size_usd": 120.0,
    })
    if decision.block:
        reject_transaction()
    else:
        process_transaction()
        # later, after outcome is known:
        client.report_outcome(decision.decision_id, was_fraud=False)

Each call to score():
  1. /decide on Syntra with current merchant features (recent_fraud_rate,
     transaction_volume_per_hour, avg_ticket_size_usd, hour),
  2. compares risk_score against the chosen threshold,
  3. /feedback to Syntra with the outcome reward when report_outcome() is called.

Option names: block_at_0_5, block_at_0_6, block_at_0_7, block_at_0_8,
block_at_0_9 — transactions with risk_score above the threshold are blocked.
"""

from __future__ import annotations

import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass
from typing import Any, Deque, Dict, Optional, Tuple

import requests as http_lib


# Default threshold table — option index -> concrete threshold value. Index
# ordering must match the capsule YAML's `options:` list installed by
# setup_capsule.py.
_DEFAULT_THRESHOLDS: list["Threshold"] = []  # populated below


@dataclass(frozen=True)
class Threshold:
    name: str
    value: float

    @staticmethod
    def from_option(option_index: int) -> "Threshold":
        if 0 <= option_index < len(_DEFAULT_THRESHOLDS):
            return _DEFAULT_THRESHOLDS[option_index]
        return _DEFAULT_THRESHOLDS[2]  # OOB falls back to 0.7 (safe middle)


_DEFAULT_THRESHOLDS.extend([
    Threshold("block_at_0_5", 0.5),
    Threshold("block_at_0_6", 0.6),
    Threshold("block_at_0_7", 0.7),
    Threshold("block_at_0_8", 0.8),
    Threshold("block_at_0_9", 0.9),
])


@dataclass(frozen=True)
class ScoreDecision:
    threshold: float
    block: bool
    decision_id: Optional[str]


class _MerchantTracker:
    """Per-merchant rolling window of observed outcomes. Drives feature vectors."""

    def __init__(self, window: int = 100) -> None:
        self.window = window
        # Each entry is 1 (fraud) or 0 (legit) for fraud_rate calculation.
        self.fraud_flags: Dict[str, Deque[int]] = defaultdict(lambda: deque(maxlen=window))
        # Ticket sizes for avg_ticket_size_usd.
        self.ticket_sizes: Dict[str, Deque[float]] = defaultdict(lambda: deque(maxlen=window))
        # Timestamps (epoch seconds) of each transaction for volume/hour calc.
        self.timestamps: Dict[str, Deque[float]] = defaultdict(lambda: deque(maxlen=window))
        self.lock = threading.Lock()

    def record(self, merchant_id: str, was_fraud: bool, ticket_size_usd: float) -> None:
        with self.lock:
            now = time.time()
            self.fraud_flags[merchant_id].append(1 if was_fraud else 0)
            self.ticket_sizes[merchant_id].append(float(ticket_size_usd))
            self.timestamps[merchant_id].append(now)

    def features(self, merchant_id: str) -> Dict[str, float]:
        with self.lock:
            flags = list(self.fraud_flags[merchant_id])
            sizes = list(self.ticket_sizes[merchant_id])
            stamps = list(self.timestamps[merchant_id])
        hour = (time.time() / 3600.0) % 24.0
        if not flags:
            return {
                "recent_fraud_rate": 0.0,
                "transaction_volume_per_hour": 0.0,
                "avg_ticket_size_usd": 0.0,
                "hour": hour,
            }
        recent_fraud_rate = sum(flags) / len(flags)
        avg_ticket = sum(sizes) / len(sizes) if sizes else 0.0
        # Volume: count transactions in the last 60 minutes.
        now = time.time()
        cutoff = now - 3600.0
        volume = sum(1 for ts in stamps if ts >= cutoff)
        return {
            "recent_fraud_rate": recent_fraud_rate,
            "transaction_volume_per_hour": float(volume),
            "avg_ticket_size_usd": avg_ticket,
            "hour": hour,
        }


class FraudClient:
    """Transaction scorer that asks Syntra for a risk threshold per decision.

    Falls back to ``fallback_threshold`` whenever Syntra is unreachable,
    returns a refusal, or returns no decision. The fallback is also used if
    any field of the /decide response is malformed.
    """

    def __init__(
        self,
        syntra_url: str,
        capsule_path: str,
        admin_key: str,
        fallback_threshold: Optional[float] = None,
        timeout_seconds: float = 2.0,
    ) -> None:
        self.syntra_url = syntra_url.rstrip("/")
        self.capsule_path = capsule_path
        self.admin_key = admin_key
        self.timeout = timeout_seconds
        self.fallback_threshold = fallback_threshold if fallback_threshold is not None else 0.7
        self.tracker = _MerchantTracker()
        self._auth_header = {"Authorization": f"Bearer {admin_key}"}

    def score(self, transaction: Dict[str, Any]) -> ScoreDecision:
        """Score a transaction and return a block/allow decision.

        Args:
            transaction: dict with keys:
                - merchant_id (str)
                - risk_score (float, 0-1)
                - ticket_size_usd (float)

        Returns:
            ScoreDecision with threshold, block flag, and decision_id for
            reporting outcomes later.
        """
        merchant_id = transaction.get("merchant_id", "")
        risk_score = float(transaction.get("risk_score", 0.0))
        ticket_size_usd = float(transaction.get("ticket_size_usd", 0.0))

        features = self.tracker.features(merchant_id)
        threshold, decision_id = self._get_threshold(features)
        block = risk_score > threshold
        return ScoreDecision(threshold=threshold, block=block, decision_id=decision_id)

    def report_outcome(
        self,
        decision_id: Optional[str],
        was_fraud: bool,
        false_positive_cost: float = 50.0,
        fraud_loss_cost: float = 200.0,
        merchant_id: str = "",
        ticket_size_usd: float = 0.0,
        blocked: bool = False,
    ) -> None:
        """Report the true outcome of a scored transaction to Syntra.

        Reward formula (clamped to [-1, 1]):
          - Correct decision (blocked real fraud OR allowed legit): +1.0
          - False positive (blocked legit transaction): -false_positive_cost / 200.0
          - Missed fraud (allowed fraudulent transaction): -fraud_loss_cost / 200.0

        Also records the outcome in the local merchant tracker for future
        feature computation.
        """
        # Update the rolling tracker regardless of whether Syntra is reachable.
        if merchant_id:
            self.tracker.record(merchant_id, was_fraud, ticket_size_usd)

        if decision_id is None:
            return

        if blocked and not was_fraud:
            # False positive: blocked a legitimate transaction.
            reward = -(false_positive_cost / 200.0)
        elif not blocked and was_fraud:
            # Missed fraud: allowed a fraudulent transaction through.
            reward = -(fraud_loss_cost / 200.0)
        else:
            # Correct: either caught fraud or allowed legit.
            reward = 1.0

        reward = max(-1.0, min(1.0, reward))

        try:
            http_lib.post(
                f"{self.syntra_url}{self.capsule_path}/feedback",
                headers=self._auth_header,
                json={"decisionId": decision_id, "reward": reward},
                timeout=self.timeout,
            )
        except http_lib.RequestException:
            # Feedback failures must never break the calling flow.
            pass

    def _get_threshold(self, features: Dict[str, float]) -> Tuple[float, Optional[str]]:
        try:
            resp = http_lib.post(
                f"{self.syntra_url}{self.capsule_path}/decide",
                headers=self._auth_header,
                json={"features": features},
                timeout=self.timeout,
            )
            resp.raise_for_status()
            data = resp.json()
            if data.get("refused"):
                return self.fallback_threshold, data.get("decisionId")
            decisions = data.get("decisions") or []
            if not decisions:
                return self.fallback_threshold, None
            option_idx = decisions[0].get("chosen_option", 2)
            threshold = Threshold.from_option(option_idx)
            return threshold.value, data.get("decisionId")
        except (http_lib.RequestException, ValueError, KeyError):
            return self.fallback_threshold, None


__all__ = ["FraudClient", "Threshold", "ScoreDecision", "_MerchantTracker"]
