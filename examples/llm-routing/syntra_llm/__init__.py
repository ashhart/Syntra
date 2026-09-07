"""Syntra-driven LLM model routing.

Usage:
    from syntra_llm import LLMRouter

    router = LLMRouter(
        syntra_url="http://localhost:8787",
        capsule_path="/tenants/myteam/jobs/llm/capsules/router",
        admin_key="...",
    )
    decision = router.choose(
        prompt_token_count=1200,
        task_complexity=0.7,
        customer_tier="pro",
    )
    # ... call the LLM model at decision.model_name ...
    router.report(
        decision_id=decision.decision_id,
        model_name=decision.model_name,
        quality=0.9,
        latency_ms=850.0,
        cost_usd=0.003,
    )

Each choose/report cycle:
  1. /decide on Syntra with current request features (prompt_token_count,
     task_complexity, customer_tier, hour),
  2. caller executes the LLM call with the chosen model,
  3. /feedback to Syntra with a reward derived from quality, latency, and cost.

Option names by default: cheap_fast, balanced, expensive_accurate.
"""

from __future__ import annotations

import math
import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass
from typing import Any, Deque, Dict, List, Optional, Tuple

import requests as http_lib


# Default model table — option index → concrete model route. Index ordering
# must match the capsule YAML's `options:` list installed by setup_capsule.py.
_DEFAULT_MODELS: List["Model"] = []  # populated below


@dataclass(frozen=True)
class Model:
    name: str
    description: str

    @staticmethod
    def from_option(option_index: int) -> "Model":
        """Return the model for the given option index; fall back to index 0 if OOB."""
        if 0 <= option_index < len(_DEFAULT_MODELS):
            return _DEFAULT_MODELS[option_index]
        return _DEFAULT_MODELS[0]


_DEFAULT_MODELS.extend([
    Model("cheap_fast",         "Low-cost, fast model for simple tasks"),
    Model("balanced",           "Mid-tier model balancing quality and cost"),
    Model("expensive_accurate", "High-quality model for complex tasks"),
])


@dataclass
class RouteDecision:
    model_name: str
    decision_id: Optional[str]


class _RequestTracker:
    """Per-customer-tier rolling window of (quality, latency_ms, cost_usd).

    Informational only — the primary feature vector for /decide is request-time
    (prompt_token_count, task_complexity, customer_tier, hour).
    """

    def __init__(self, window: int = 100) -> None:
        self.window = window
        self.qualities: Dict[str, Deque[float]] = defaultdict(lambda: deque(maxlen=window))
        self.latencies: Dict[str, Deque[float]] = defaultdict(lambda: deque(maxlen=window))
        self.costs: Dict[str, Deque[float]] = defaultdict(lambda: deque(maxlen=window))
        self.lock = threading.Lock()

    def record(self, tier: str, quality: float, latency_ms: float, cost_usd: float) -> None:
        with self.lock:
            self.qualities[tier].append(float(quality))
            self.latencies[tier].append(float(latency_ms))
            self.costs[tier].append(float(cost_usd))

    def features(self, tier: str) -> Dict[str, float]:
        with self.lock:
            quals = list(self.qualities[tier])
            lats = list(self.latencies[tier])
            costs = list(self.costs[tier])
        if not quals:
            return {
                "avg_quality": 0.5,
                "avg_latency_ms": 1000.0,
                "avg_cost_usd": 0.01,
            }
        return {
            "avg_quality": sum(quals) / len(quals),
            "avg_latency_ms": sum(lats) / len(lats),
            "avg_cost_usd": sum(costs) / len(costs),
        }


class LLMRouter:
    """Asks Syntra for a model route per request.

    Falls back to ``fallback_model`` whenever Syntra is unreachable, returns a
    refusal, or returns a malformed response.

    Reward formula (configurable via constructor kwargs):
        reward = quality_weight * quality
                 - latency_weight * clamp(latency_ms / 3000, 0, 1)
                 - cost_weight    * clamp(cost_usd   / 0.10, 0, 1)
        reward clamped to [-1, 1].

    Default weights: quality=0.6, latency=0.2, cost=0.2.
    """

    def __init__(
        self,
        syntra_url: str,
        capsule_path: str,
        admin_key: str,
        fallback_model: Optional[Model] = None,
        timeout_seconds: float = 2.0,
        quality_weight: float = 0.6,
        latency_weight: float = 0.2,
        cost_weight: float = 0.2,
    ) -> None:
        self.syntra_url = syntra_url.rstrip("/")
        self.capsule_path = capsule_path
        self.admin_key = admin_key
        self.timeout = timeout_seconds
        self.fallback_model = fallback_model or Model.from_option(1)  # balanced
        self.tracker = _RequestTracker()
        self._auth_header = {"Authorization": f"Bearer {admin_key}"}
        self.quality_weight = quality_weight
        self.latency_weight = latency_weight
        self.cost_weight = cost_weight

    def choose(
        self,
        prompt_token_count: int,
        task_complexity: float,
        customer_tier: str,
    ) -> RouteDecision:
        """Ask Syntra which model to use for this request.

        Parameters
        ----------
        prompt_token_count:
            Number of tokens in the prompt (0–100000).
        task_complexity:
            Caller-supplied estimate of task difficulty in [0, 1].
        customer_tier:
            One of "free", "pro", "enterprise".

        Returns
        -------
        RouteDecision with model_name and decision_id (None if Syntra was
        unreachable and fallback was used without a /decide call).
        """
        hour = (time.time() / 3600.0) % 24.0
        features: Dict[str, Any] = {
            "prompt_token_count": float(prompt_token_count),
            "task_complexity": float(task_complexity),
            "customer_tier": customer_tier,
            "hour": hour,
        }
        model, decision_id = self._get_model(features)
        return RouteDecision(model_name=model.name, decision_id=decision_id)

    def report(
        self,
        decision_id: Optional[str],
        model_name: str,
        quality: float,
        latency_ms: float,
        cost_usd: float,
    ) -> None:
        """Send reward feedback to Syntra and record in local tracker.

        Parameters
        ----------
        decision_id:
            The decision_id from the corresponding RouteDecision. If None
            (e.g. Syntra was unreachable), feedback is skipped.
        model_name:
            The model name that was used (from RouteDecision.model_name).
        quality:
            Caller-assessed output quality in [0, 1].
        latency_ms:
            End-to-end latency of the LLM call in milliseconds.
        cost_usd:
            Dollar cost of the LLM call.
        """
        # Infer tier from model_name is not reliable — tracker is keyed by
        # model_name so it is still useful for monitoring purposes.
        self.tracker.record(model_name, quality, latency_ms, cost_usd)

        if decision_id is not None:
            self._send_feedback(decision_id, quality, latency_ms, cost_usd)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _get_model(
        self, features: Dict[str, Any]
    ) -> Tuple[Model, Optional[str]]:
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
                return self.fallback_model, data.get("decisionId")
            decisions = data.get("decisions") or []
            if not decisions:
                return self.fallback_model, None
            option_idx = decisions[0].get("chosen_option", 0)
            return Model.from_option(option_idx), data.get("decisionId")
        except (http_lib.RequestException, ValueError, KeyError):
            return self.fallback_model, None

    def _send_feedback(
        self,
        decision_id: str,
        quality: float,
        latency_ms: float,
        cost_usd: float,
    ) -> None:
        latency_norm = min(latency_ms / 3000.0, 1.0)
        cost_norm = min(cost_usd / 0.10, 1.0)
        raw_reward = (
            self.quality_weight * quality
            - self.latency_weight * latency_norm
            - self.cost_weight * cost_norm
        )
        reward = max(-1.0, min(1.0, raw_reward))
        try:
            http_lib.post(
                f"{self.syntra_url}{self.capsule_path}/feedback",
                headers=self._auth_header,
                json={"decisionId": decision_id, "reward": reward},
                timeout=self.timeout,
            )
        except http_lib.RequestException:
            # Feedback failures must never break the caller's flow.
            pass


__all__ = ["LLMRouter", "Model", "RouteDecision", "_RequestTracker"]
