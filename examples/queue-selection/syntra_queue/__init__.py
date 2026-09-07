"""Syntra-driven backend/queue selection for distributed systems.

Usage:
    from syntra_queue import QueueClient

    client = QueueClient(
        syntra_url="http://localhost:8787",
        capsule_path="/tenants/myteam/jobs/queue/capsules/router",
        admin_key="...",
    )
    pick = client.pick(request_size_kb=12.5, queue_depths={"backend_a": 5, "backend_b": 42, "backend_c": 0})
    # ... send request to pick.backend_name ...
    client.report(pick.decision_id, pick.backend_name, success=True, latency_ms=87.3)

Each call:
  1. Aggregates per-backend rolling latency + error-rate stats into a feature vector,
  2. Calls Syntra /decide to select a backend,
  3. Executes your logic with the chosen backend,
  4. Reports the outcome (success + latency) back to /feedback.

Option names match the capsule installed by setup_capsule.py (backend_a, backend_b,
backend_c by default — count configurable via SYNTRA_QUEUE_N_BACKENDS).
"""

from __future__ import annotations

import threading
from collections import deque
from dataclasses import dataclass
from typing import Deque, Dict, List, Optional, Tuple

import requests as http_lib


# Default backend list — option index must match the capsule YAML's `options:` list
# installed by setup_capsule.py.  Override by replacing _DEFAULT_BACKENDS before
# constructing QueueClient.
_DEFAULT_BACKENDS: List["Backend"] = []  # populated below


@dataclass(frozen=True)
class Backend:
    name: str

    @staticmethod
    def from_option(option_index: int) -> "Backend":
        if 0 <= option_index < len(_DEFAULT_BACKENDS):
            return _DEFAULT_BACKENDS[option_index]
        return _DEFAULT_BACKENDS[0]


_DEFAULT_BACKENDS.extend([
    Backend("backend_a"),
    Backend("backend_b"),
    Backend("backend_c"),
])


@dataclass
class BackendPick:
    backend_name: str
    decision_id: Optional[str]


class _BackendTracker:
    """Per-backend rolling window of (success, latency_ms). Drives feature vectors."""

    def __init__(self, window: int = 100) -> None:
        self.window = window
        self._latencies: Dict[str, Deque[float]] = {}
        self._errors: Dict[str, Deque[int]] = {}
        self.lock = threading.Lock()

    def _ensure(self, backend: str) -> None:
        if backend not in self._latencies:
            self._latencies[backend] = deque(maxlen=self.window)
            self._errors[backend] = deque(maxlen=self.window)

    def record(self, backend: str, success: bool, latency_ms: float) -> None:
        with self.lock:
            self._ensure(backend)
            self._latencies[backend].append(float(latency_ms))
            self._errors[backend].append(0 if success else 1)

    def features(self, backend: str) -> Tuple[float, float]:
        """Return (avg_recent_latency_ms, recent_error_rate) for a backend.

        Returns neutral values (500.0, 0.5) when no observations have been
        recorded yet, following the retry-tuning convention of picking a
        mid-range neutral prior.
        """
        with self.lock:
            self._ensure(backend)
            lats = list(self._latencies[backend])
            errs = list(self._errors[backend])

        if not lats:
            return 500.0, 0.5

        avg_latency = sum(lats) / len(lats)
        error_rate = sum(errs) / len(errs)
        return avg_latency, error_rate


class QueueClient:
    """Asks Syntra to pick a backend queue for each request.

    Falls back to round-robin across known backends whenever Syntra is
    unreachable, returns a refusal, or returns a malformed response.
    Refusal still posts /feedback for the audit log.
    """

    def __init__(
        self,
        syntra_url: str,
        capsule_path: str,
        admin_key: str,
        backends: Optional[List[Backend]] = None,
        timeout_seconds: float = 2.0,
    ) -> None:
        self.syntra_url = syntra_url.rstrip("/")
        self.capsule_path = capsule_path
        self.admin_key = admin_key
        self.backends: List[Backend] = backends if backends is not None else list(_DEFAULT_BACKENDS)
        self.timeout = timeout_seconds
        self.tracker = _BackendTracker()
        self._auth_header = {"Authorization": f"Bearer {admin_key}"}
        self._rr_index = 0
        self._rr_lock = threading.Lock()

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def pick(self, request_size_kb: float, queue_depths: Dict[str, int]) -> BackendPick:
        """Ask Syntra to select a backend.

        Aggregates per-backend rolling stats and the per-backend queue depth
        into a single feature vector, then calls /decide.  Falls back to
        round-robin if Syntra is unreachable.

        Parameters
        ----------
        request_size_kb:
            Size of the current request payload in kilobytes.
        queue_depths:
            Current queue depth per backend, keyed by backend name.
            Missing backends default to 0.
        """
        features = self._build_features(request_size_kb, queue_depths)
        backend, decision_id = self._decide(features)
        return BackendPick(backend_name=backend.name, decision_id=decision_id)

    def report(
        self,
        decision_id: Optional[str],
        backend_name: str,
        success: bool,
        latency_ms: float,
    ) -> None:
        """Record the outcome of a request and send feedback to Syntra.

        Reward formula: (1.0 if success else 0.0) - 0.0001 * latency_ms,
        clamped to [-1.0, 1.0].

        Feedback failures are silently swallowed so they never break the
        calling flow.
        """
        self.tracker.record(backend_name, success, latency_ms)
        if decision_id is not None:
            self._send_feedback(decision_id, success, latency_ms)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _build_features(
        self, request_size_kb: float, queue_depths: Dict[str, int]
    ) -> Dict[str, float]:
        """Aggregate per-backend stats into a single feature vector.

        With multiple backends the feature vector uses the mean latency and
        max error rate across all backends, along with the mean queue depth
        and the supplied request size.
        """
        if not self.backends:
            return {
                "avg_recent_latency_ms": 500.0,
                "recent_error_rate": 0.5,
                "current_queue_depth": 0.0,
                "request_size_kb": float(request_size_kb),
            }

        avg_latencies = []
        error_rates = []
        for backend in self.backends:
            avg_lat, err_rate = self.tracker.features(backend.name)
            avg_latencies.append(avg_lat)
            error_rates.append(err_rate)

        mean_latency = sum(avg_latencies) / len(avg_latencies)
        # Use the highest error rate across backends as a conservative signal.
        max_error_rate = max(error_rates)

        depths = [float(queue_depths.get(b.name, 0)) for b in self.backends]
        mean_depth = sum(depths) / len(depths)

        return {
            "avg_recent_latency_ms": mean_latency,
            "recent_error_rate": max_error_rate,
            "current_queue_depth": mean_depth,
            "request_size_kb": float(request_size_kb),
        }

    def _decide(
        self, features: Dict[str, float]
    ) -> Tuple[Backend, Optional[str]]:
        try:
            resp = http_lib.post(
                f"{self.syntra_url}{self.capsule_path}/decide",
                headers=self._auth_header,
                json={"features": features},
                timeout=self.timeout,
            )
            resp.raise_for_status()
            data = resp.json()
            decision_id = data.get("decisionId")
            if data.get("refused"):
                # Still return the decisionId so _send_feedback posts an audit entry.
                return self._round_robin(), decision_id
            decisions = data.get("decisions") or []
            if not decisions:
                return self._round_robin(), None
            option_idx = decisions[0].get("chosen_option", 0)
            return Backend.from_option(option_idx), decision_id
        except (http_lib.RequestException, ValueError, KeyError):
            return self._round_robin(), None

    def _round_robin(self) -> Backend:
        with self._rr_lock:
            idx = self._rr_index % len(self.backends)
            self._rr_index += 1
        return self.backends[idx]

    def _send_feedback(
        self, decision_id: str, success: bool, latency_ms: float
    ) -> None:
        raw_reward = (1.0 if success else 0.0) - 0.0001 * latency_ms
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


__all__ = ["QueueClient", "Backend", "BackendPick", "_BackendTracker"]
