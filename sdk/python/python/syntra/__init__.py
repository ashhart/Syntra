"""Syntra: decisions that learn, in microseconds.

Two ways to decide:

* ``LocalDecider`` decides in-process on a copy of the capsule's published
  model (a few microseconds, no network hop, keeps working if the server is
  down). Decisions and rewards upload in the background; the server replays
  every uploaded decision to verify it, logs it with its propensity, and
  learns from its rewards.
* ``Client`` calls the server's ``/decide`` and ``/reward`` over HTTP, for
  services that prefer to keep no model in memory.

    from syntra import LocalDecider

    with LocalDecider("http://localhost:8787", token=TOKEN,
                      tenant="acme", job="prod", capsule="router") as router:
        d = router.decide({"task": "code", "promptTokens": 812})
        ...  # act on d.action
        router.reward(d.decision_id, 0.8)
"""

from __future__ import annotations

import http.client
import json
import select
import threading
import urllib.parse
from typing import Any, Iterable, Mapping, Optional, Sequence

from ._native import Decision, NativeDecider, SyntraError

__all__ = ["Client", "Decision", "HttpError", "LocalDecider", "SyntraError"]
__version__ = "0.2.0"


class HttpError(SyntraError):
    """The server answered with an error status (``status``, ``body``).

    The bearer token never appears in the message."""

    def __init__(self, message: str, *, status: int, body: Any = None) -> None:
        super().__init__(message)
        self.status = status
        self.body = body


class LocalDecider:
    """Decide in-process; learn on the server.

    Connecting fetches the capsule's published model. With
    ``sync_interval`` (seconds, default 1.0) a background thread uploads
    queued decisions and rewards and picks up newer models at that
    interval; pass ``None`` to call ``flush()`` and ``sync()`` yourself.

    A decider is safe to share between threads. Call ``close()`` (or use it
    as a context manager) to upload what is still queued before exiting.
    """

    def __init__(
        self,
        url: str,
        *,
        token: str,
        tenant: str,
        job: str,
        capsule: str,
        sync_interval: Optional[float] = 1.0,
        max_queue: int = 100_000,
    ) -> None:
        self._native = NativeDecider(url, token, tenant, job, capsule, max_queue)
        if sync_interval is not None:
            self._native.start_background(float(sync_interval))

    def decide(
        self,
        context: Optional[Mapping[str, Any]] = None,
        *,
        actions: Optional[Sequence[Mapping[str, Any]]] = None,
        exclude: Optional[Iterable[str]] = None,
        baseline: Optional[str] = None,
    ) -> Decision:
        """Choose an action for ``context``.

        ``actions`` replaces the spec's actions for this call (each
        ``{"id": ..., "features": {...}}``); ``exclude`` removes action ids;
        ``baseline`` names the incumbent action in ``baselineExplore`` mode.
        """
        return self._native.decide(
            context,
            None if actions is None else list(actions),
            None if exclude is None else list(exclude),
            baseline,
        )

    def reward(
        self,
        decision_id: str,
        reward: float,
        *,
        idempotency_key: Optional[str] = None,
        detail: Optional[Mapping[str, Any]] = None,
    ) -> None:
        """Queue the outcome of a decision (made here or on the server)."""
        self._native.reward(decision_id, float(reward), idempotency_key, detail)

    def flush(self) -> dict:
        """Upload queued decisions, then rewards. Returns counts."""
        return self._native.flush()

    def sync(self) -> bool:
        """Fetch a newer published model; True if the model changed."""
        return self._native.sync()

    def close(self) -> dict:
        """Stop the background thread and upload everything queued."""
        return self._native.close()

    @property
    def model_version(self) -> int:
        return self._native.model_version

    @property
    def model_tag(self) -> str:
        return self._native.model_tag

    @property
    def pending(self) -> int:
        """Decisions and rewards waiting to be uploaded."""
        return self._native.pending

    def __enter__(self) -> "LocalDecider":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def _dropped(conn: http.client.HTTPConnection) -> bool:
    """True if an idle kept-alive connection was closed by the server (its
    socket reads as ready: EOF). Checked before reuse, as urllib3 does."""
    sock = conn.sock
    if sock is None:
        return False
    try:
        readable, _, _ = select.select([sock], [], [], 0)
    except (OSError, ValueError):
        return True
    return bool(readable)


class Client:
    """Server-side decisions over HTTP (keep-alive, one connection per thread)."""

    def __init__(
        self,
        url: str,
        *,
        token: str,
        tenant: str,
        job: str,
        capsule: str,
        timeout: float = 10.0,
    ) -> None:
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme not in ("http", "https") or not parsed.hostname:
            raise ValueError(f"not an http(s) URL: {url!r}")
        self._scheme = parsed.scheme
        self._host = parsed.hostname
        self._port = parsed.port
        self._timeout = timeout
        self._headers = {
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        }
        base = parsed.path.rstrip("/")
        self._base = f"{base}/v1/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
        self._local = threading.local()
        self._lock = threading.Lock()
        self._conns: list = []

    def _conn(self) -> http.client.HTTPConnection:
        conn = getattr(self._local, "conn", None)
        if conn is not None and _dropped(conn):
            conn.close()
            conn = None
        if conn is None:
            cls = (
                http.client.HTTPSConnection
                if self._scheme == "https"
                else http.client.HTTPConnection
            )
            conn = cls(self._host, self._port, timeout=self._timeout)
            self._local.conn = conn
            with self._lock:
                self._conns.append(conn)
        return conn

    def close(self) -> None:
        """Close every kept-alive connection (from all threads)."""
        with self._lock:
            conns, self._conns = self._conns, []
        for conn in conns:
            conn.close()

    def __enter__(self) -> "Client":
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()

    def _call(self, method: str, tail: str, body: Any = None) -> Any:
        payload = None if body is None else json.dumps(body).encode()
        for attempt in (0, 1):
            conn = self._conn()
            try:
                conn.request(method, f"{self._base}/{tail}", payload, self._headers)
                resp = conn.getresponse()
                data = resp.read()
            except (ConnectionError, http.client.HTTPException, OSError):
                conn.close()
                self._local.conn = None
                with self._lock:
                    if conn in self._conns:
                        self._conns.remove(conn)
                # A kept-alive connection the server closed: retry once on
                # a fresh one.
                if attempt == 0 and method == "GET":
                    continue
                raise
            if resp.status >= 400:
                try:
                    parsed = json.loads(data)
                    msg = parsed.get("error", data.decode())
                except (ValueError, AttributeError):
                    parsed, msg = None, data.decode(errors="replace")
                raise HttpError(
                    f"{method} {tail}: HTTP {resp.status}: {msg}",
                    status=resp.status,
                    body=parsed,
                )
            return json.loads(data) if data else None
        raise SyntraError("unreachable")

    def decide(
        self,
        context: Optional[Mapping[str, Any]] = None,
        *,
        actions: Optional[Sequence[Mapping[str, Any]]] = None,
        exclude: Optional[Iterable[str]] = None,
        baseline: Optional[str] = None,
        event_id: Optional[str] = None,
    ) -> dict:
        """``POST .../decide``. ``event_id`` makes retries return the same decision."""
        body: dict = {"context": dict(context or {})}
        if actions is not None:
            body["actions"] = list(actions)
        if exclude is not None:
            body["excludedActions"] = list(exclude)
        if baseline is not None:
            body["baselineAction"] = baseline
        if event_id is not None:
            body["eventId"] = event_id
        return self._call("POST", "decide", body)

    def reward(
        self,
        decision_id: str,
        reward: float,
        *,
        idempotency_key: Optional[str] = None,
        detail: Optional[Mapping[str, Any]] = None,
    ) -> dict:
        """``POST .../reward``."""
        body: dict = {"decisionId": decision_id, "reward": float(reward)}
        if idempotency_key is not None:
            body["idempotencyKey"] = idempotency_key
        if detail is not None:
            body["detail"] = dict(detail)
        return self._call("POST", "reward", body)

    def put_spec(self, patch: Mapping[str, Any]) -> dict:
        """``PUT .../spec`` (JSON merge patch; creates the capsule)."""
        return self._call("PUT", "spec", dict(patch))

    def get_spec(self) -> dict:
        return self._call("GET", "spec")

    def model(self) -> dict:
        return self._call("GET", "model")

    def decision(self, decision_id: str) -> dict:
        return self._call("GET", f"decisions/{urllib.parse.quote(decision_id)}")
