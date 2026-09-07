"""Syntra-driven retry policy selection for HTTP clients.

Usage:
    from syntra_retry import RetryClient

    client = RetryClient(
        syntra_url="http://localhost:8787",
        capsule_path="/tenants/myteam/jobs/retry/capsules/router",
        admin_key="...",
    )
    response = client.request("GET", "https://api.example.com/users")

Each request:
  1. /decide on Syntra with current endpoint features (failure rate, p99, hour),
  2. real HTTP request with the chosen retry policy applied,
  3. /feedback to Syntra with success + total latency.

Option names match the F1 demo capsule (none, single, triple, exponential_fast,
exponential_slow) so the same `syntra:demo` image works as the integration target.
"""

from __future__ import annotations

import threading
import time
from collections import defaultdict, deque
from dataclasses import dataclass
from typing import Any, Deque, Dict, Optional, Tuple
from urllib.parse import urlparse

import requests as http_lib


# Default policy table — option index → concrete retry behavior. Index ordering
# must match the capsule YAML's `options:` list installed by setup_capsule.py.
_DEFAULT_POLICIES: list["RetryPolicy"] = []  # populated below


@dataclass(frozen=True)
class RetryPolicy:
    name: str
    max_retries: int
    initial_backoff_ms: int
    backoff_multiplier: float

    @staticmethod
    def from_option(option_index: int) -> "RetryPolicy":
        if 0 <= option_index < len(_DEFAULT_POLICIES):
            return _DEFAULT_POLICIES[option_index]
        return _DEFAULT_POLICIES[0]


_DEFAULT_POLICIES.extend([
    RetryPolicy("none",              max_retries=0, initial_backoff_ms=0,   backoff_multiplier=1.0),
    RetryPolicy("single",            max_retries=1, initial_backoff_ms=0,   backoff_multiplier=1.0),
    RetryPolicy("triple",            max_retries=3, initial_backoff_ms=0,   backoff_multiplier=1.0),
    RetryPolicy("exponential_fast",  max_retries=3, initial_backoff_ms=100, backoff_multiplier=2.0),
    RetryPolicy("exponential_slow",  max_retries=3, initial_backoff_ms=500, backoff_multiplier=2.0),
])


@dataclass(frozen=True)
class RequestOutcome:
    success: bool
    total_latency_ms: float
    retries_used: int
    status_code: Optional[int]


class _EndpointTracker:
    """Per-host rolling window of (success, latency_ms). Drives feature vectors."""

    def __init__(self, window: int = 100) -> None:
        self.window = window
        self.outcomes: Dict[str, Deque[int]] = defaultdict(lambda: deque(maxlen=window))
        self.latencies: Dict[str, Deque[float]] = defaultdict(lambda: deque(maxlen=window))
        self.lock = threading.Lock()

    def record(self, endpoint: str, success: bool, latency_ms: float) -> None:
        with self.lock:
            self.outcomes[endpoint].append(1 if success else 0)
            self.latencies[endpoint].append(float(latency_ms))

    def features(self, endpoint: str) -> Dict[str, float]:
        with self.lock:
            outs = list(self.outcomes[endpoint])
            lats = sorted(self.latencies[endpoint])
        hour = (time.time() / 3600.0) % 24.0
        if not outs:
            return {"recent_failure_rate": 0.5, "p99_latency_ms": 1000.0, "hour": hour}
        failure_rate = 1.0 - (sum(outs) / len(outs))
        if lats:
            idx = max(0, int(len(lats) * 0.99) - 1)
            p99 = lats[idx]
        else:
            p99 = 1000.0
        return {"recent_failure_rate": failure_rate, "p99_latency_ms": p99, "hour": hour}


class RetryClient:
    """HTTP client that asks Syntra for a retry policy per request.

    Falls back to ``fallback_policy`` whenever Syntra is unreachable, returns a
    refusal, or returns no decision. The fallback is also used if any field of
    the /decide response is malformed.
    """

    def __init__(
        self,
        syntra_url: str,
        capsule_path: str,
        admin_key: str,
        fallback_policy: Optional[RetryPolicy] = None,
        timeout_seconds: float = 2.0,
    ) -> None:
        self.syntra_url = syntra_url.rstrip("/")
        self.capsule_path = capsule_path
        self.admin_key = admin_key
        self.timeout = timeout_seconds
        self.fallback_policy = fallback_policy or RetryPolicy.from_option(1)
        self.tracker = _EndpointTracker()
        self._auth_header = {"Authorization": f"Bearer {admin_key}"}

    def request(self, method: str, url: str, **kwargs: Any) -> http_lib.Response:
        endpoint = self._endpoint_key(url)
        features = self.tracker.features(endpoint)
        policy, decision_id = self._get_policy(features)
        outcome, response = self._execute_with_policy(method, url, policy, **kwargs)
        self.tracker.record(endpoint, outcome.success, outcome.total_latency_ms)
        if decision_id is not None:
            self._send_feedback(decision_id, outcome)
        if response is not None:
            return response
        raise http_lib.ConnectionError(f"All retries exhausted for {method} {url}")

    @staticmethod
    def _endpoint_key(url: str) -> str:
        parsed = urlparse(url)
        return parsed.netloc or url

    def _get_policy(self, features: Dict[str, float]) -> Tuple[RetryPolicy, Optional[str]]:
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
                return self.fallback_policy, data.get("decisionId")
            decisions = data.get("decisions") or []
            if not decisions:
                return self.fallback_policy, None
            option_idx = decisions[0].get("chosen_option", 0)
            return RetryPolicy.from_option(option_idx), data.get("decisionId")
        except (http_lib.RequestException, ValueError, KeyError):
            return self.fallback_policy, None

    def _execute_with_policy(
        self, method: str, url: str, policy: RetryPolicy, **kwargs: Any,
    ) -> Tuple[RequestOutcome, Optional[http_lib.Response]]:
        start = time.time()
        retries_used = 0
        response: Optional[http_lib.Response] = None
        backoff_ms = policy.initial_backoff_ms

        for attempt in range(policy.max_retries + 1):
            try:
                response = http_lib.request(method, url, **kwargs)
                if response.status_code < 500:
                    elapsed_ms = (time.time() - start) * 1000.0
                    return RequestOutcome(
                        success=response.status_code < 400,
                        total_latency_ms=elapsed_ms,
                        retries_used=retries_used,
                        status_code=response.status_code,
                    ), response
            except http_lib.RequestException:
                pass

            if attempt < policy.max_retries:
                retries_used += 1
                if backoff_ms > 0:
                    time.sleep(backoff_ms / 1000.0)
                    backoff_ms = int(backoff_ms * policy.backoff_multiplier)

        elapsed_ms = (time.time() - start) * 1000.0
        return RequestOutcome(
            success=False,
            total_latency_ms=elapsed_ms,
            retries_used=retries_used,
            status_code=response.status_code if response else None,
        ), response

    def _send_feedback(self, decision_id: str, outcome: RequestOutcome) -> None:
        # Reward = success_bit − latency_penalty. Stays inside the capsule's
        # [-1.0, 1.0] continuous range. Tunable; see setup_capsule.py.
        latency_penalty = min(outcome.total_latency_ms / 10000.0, 1.0)
        reward = (1.0 if outcome.success else 0.0) - 0.3 * latency_penalty
        try:
            http_lib.post(
                f"{self.syntra_url}{self.capsule_path}/feedback",
                headers=self._auth_header,
                json={"decisionId": decision_id, "reward": reward},
                timeout=self.timeout,
            )
        except http_lib.RequestException:
            # Feedback failures must never break the user's request flow.
            pass


__all__ = ["RetryClient", "RetryPolicy", "RequestOutcome", "_EndpointTracker"]
