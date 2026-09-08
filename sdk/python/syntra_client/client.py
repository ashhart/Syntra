"""Stdlib-only HTTP client for the Syntra decision runtime.

Targets the canonical ``/v1`` surface (the unversioned paths are
deprecated aliases the runtime still serves). ``GET /health``,
``GET /ready`` and ``GET /metrics`` are infra endpoints and deliberately
have no ``/v1`` form — see ``docs/openapi.yaml``.

Retry policy: idempotent reads and capsule installs are retried on
``5xx`` and transport failures with exponential backoff. ``decide`` and
``feedback`` are **never** retried: both can append to the decision log
and mutate learned state, so a retry would double-count a learning
effect. Send ``feedback`` at-most-once and reconcile with
``GET .../decisions`` if you need exactly-once semantics.
"""

from __future__ import annotations

import http.client
import json
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Dict, List, Mapping, NoReturn, Optional

from .errors import (
    AuthError,
    BadRequestError,
    ConflictError,
    HttpStatusError,
    NotFoundError,
    PayloadTooLargeError,
    RateLimitedError,
    ServerError,
    SyntraError,
    TransportError,
)
from .models import REDACTED, Decision, Token

__all__ = ["SyntraClient"]

# `install_capsule`'s payload parameter is named `bytes` (matching the
# documented SDK signature), which shadows the builtin inside that body.
# These module-scope aliases are the escape hatch.
_BYTES = bytes
_BYTES_LIKE = (bytes, bytearray, memoryview)
_BACKOFF_BASE_SECONDS = 0.2
_BACKOFF_CAP_SECONDS = 2.0
_MAX_ERROR_DETAIL_CHARS = 400


class SyntraClient:
    """Synchronous client for one Syntra appliance.

    Args:
        base_url: Appliance root, e.g. ``http://127.0.0.1:8787``. Any
            trailing slash is ignored; API paths are appended as-is.
        token: Bearer token — a scoped token from
            :meth:`create_token`, or the legacy ``LYCAN_ADMIN_KEY``.
            ``None`` works only against a server started with
            ``--dev-mode``.
        timeout: Per-request socket timeout in seconds.
        retries: Extra attempts for idempotent calls only. ``0`` disables
            retrying.

    The bearer token is never included in logs, messages, or ``repr()``.
    """

    def __init__(
        self,
        base_url: str,
        token: Optional[str] = None,
        timeout: float = 10.0,
        retries: int = 2,
    ) -> None:
        if not base_url:
            raise ValueError("base_url is required")
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        if retries < 0:
            raise ValueError("retries must be >= 0")
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = float(timeout)
        self.retries = int(retries)

    def __repr__(self) -> str:
        return (
            f"SyntraClient(base_url={self.base_url!r}, token={REDACTED}, "
            f"timeout={self.timeout!r}, retries={self.retries!r})"
        )

    __str__ = __repr__

    # ── Infra (unversioned by design) ────────────────────────────────────

    def health(self) -> Dict[str, Any]:
        """``GET /health`` — liveness probe; ``{"ok": true, "service": ...}``.

        No auth is required by the runtime, so no token is sent.
        """
        return self._json("GET", "/health", auth=False)

    def ready(self) -> Dict[str, Any]:
        """``GET /ready`` — store-writability probe (``503`` when unwritable)."""
        return self._json("GET", "/ready", auth=False)

    def metrics(self) -> str:
        """``GET /metrics`` — Prometheus text exposition, returned verbatim."""
        _, payload, _ = self._request("GET", "/metrics", auth=False)
        return payload.decode("utf-8", "replace")

    def whoami(self) -> Dict[str, Any]:
        """``GET /v1/auth/whoami`` — the principal and scope for this token."""
        return self._json("GET", "/v1/auth/whoami")

    # ── Tenants / jobs ───────────────────────────────────────────────────

    def list_tenants(self) -> List[str]:
        """``GET /v1/tenants``."""
        return list(self._json("GET", "/v1/tenants").get("tenants") or [])

    def create_job(
        self,
        tenant: str,
        job: str,
        name: Optional[str] = None,
        description: str = "",
        metadata: Optional[Mapping[str, Any]] = None,
    ) -> Dict[str, Any]:
        """``POST /v1/tenants/{tenant}/jobs`` — returns the job record.

        Raises :class:`~syntra_client.errors.ConflictError` on a duplicate
        job id. Capsule installs create their job directory implicitly,
        so this call is only needed to set a name/description.
        """
        body: Dict[str, Any] = {"id": job, "name": name or job}
        if description:
            body["description"] = description
        if metadata is not None:
            body["metadata"] = dict(metadata)
        return self._json("POST", self._jobs_path(tenant), json_body=body)["job"]

    # ── Capsules ─────────────────────────────────────────────────────────

    def capsule_path(self, tenant: str, job: str, capsule: str) -> str:
        """Canonical ``/v1`` capsule path, with path segments escaped."""
        return "/v1/tenants/%s/jobs/%s/capsules/%s" % (
            self._seg(tenant),
            self._seg(job),
            self._seg(capsule),
        )

    def install_capsule(
        self, tenant: str, job: str, capsule: str, bytes: bytes
    ) -> str:
        """``POST .../install`` — uploads raw ``.lyc`` bytes; returns the SHA-256.

        The body is the compiled graph binary itself
        (``Content-Type: application/octet-stream``), not JSON and not
        base64. The returned hash is what ``GET .../report`` reports as
        ``hash`` and what ``audit.jsonl`` records for this install, so
        comparing it against ``sha256`` of the bytes you sent confirms the
        runtime stored exactly what you uploaded.

        Retried on ``5xx`` like the reads: reinstalling identical bytes is
        a no-op write of ``current.lyc``.
        """
        if not isinstance(bytes, _BYTES_LIKE):
            raise TypeError("capsule payload must be bytes")
        payload = _BYTES(bytes)
        path = self.capsule_path(tenant, job, capsule) + "/install"
        _, raw, _ = self._request(
            "POST",
            path,
            body=payload,
            content_type="application/octet-stream",
            idempotent=True,
        )
        response = self._require_dict(raw)
        graph_hash = response.get("hash")
        if not isinstance(graph_hash, str):
            raise SyntraError(
                "install response carries no 'hash' field",
                method="POST",
                path=path,
                body=response,
            )
        return graph_hash

    def delete_capsule(self, tenant: str, job: str, capsule: str) -> Dict[str, Any]:
        """``DELETE .../capsules/{capsule}``."""
        return self._json("DELETE", self.capsule_path(tenant, job, capsule))

    # ── Decide / feedback ────────────────────────────────────────────────

    def decide(
        self,
        tenant: str,
        job: str,
        capsule: str,
        context: Mapping[str, Any],
        learn: bool = False,
    ) -> Decision:
        """``POST .../decide`` — returns a :class:`~syntra_client.models.Decision`.

        ``context`` is the request body as the runtime expects it:
        ``{"contextKey": "rush_hour"}`` for a discrete-context capsule, or
        ``{"features": {...}}`` for a feature-context capsule, with an
        optional ``input`` object alongside either. It is passed through
        unchanged.

        ``learn=True`` adds ``?learn=true`` for in-band weight mutation;
        the default is the shadow-mode read path. A ``read``-scoped token
        is downgraded to read-only server-side, so check
        ``decision.learned`` rather than assuming the flag took effect.

        Never retried — see the module docstring.
        """
        if not isinstance(context, Mapping):
            raise TypeError("context must be a mapping")
        params = {"learn": "true"} if learn else None
        _, body, _ = self._request(
            "POST",
            self.capsule_path(tenant, job, capsule) + "/decide",
            params=params,
            json_body=dict(context),
        )
        return Decision.from_response(self._require_dict(body))

    def feedback(
        self,
        tenant: str,
        job: str,
        capsule: str,
        decision_id: str,
        reward: Optional[float] = None,
        components: Optional[Mapping[str, float]] = None,
        decision_index: Optional[int] = None,
    ) -> bool:
        """``POST .../feedback`` — ``True`` when the runtime recorded the reward.

        Give either a scalar ``reward`` or a ``components`` map (reduced
        server-side by the installed ``reward_spec.json``), never both:
        the runtime prefers ``reward`` and silently ignores
        ``components`` when both are present.

        ``decision_index`` targets one entry of a multi-node
        ``decisions[]`` array (server default ``0``).

        Never retried — feedback mutates learned state.
        """
        if reward is None and components is None:
            raise ValueError("either reward or components is required")
        if reward is not None and components is not None:
            raise ValueError(
                "pass reward or components, not both: the server ignores "
                "components whenever reward is present"
            )
        body: Dict[str, Any] = {"decisionId": decision_id}
        if reward is not None:
            body["reward"] = float(reward)
        else:
            body["components"] = dict(components or {})
        if decision_index is not None:
            body["decisionIndex"] = int(decision_index)
        _, raw, _ = self._request(
            "POST",
            self.capsule_path(tenant, job, capsule) + "/feedback",
            json_body=body,
        )
        payload = self._require_dict(raw)
        return bool(payload.get("ok", False))

    # ── Inspection ───────────────────────────────────────────────────────

    def report(self, tenant: str, job: str, capsule: str) -> Dict[str, Any]:
        """``GET .../report`` — live graph view: ``hash``, ``strategies[]``,
        per-option weights/tries, ``warmup``, ``algorithm``, ``metaBandit``."""
        return self._json(
            "GET", self.capsule_path(tenant, job, capsule) + "/report"
        )

    def contexts(self, tenant: str, job: str, capsule: str) -> Dict[str, Any]:
        """``GET .../contexts`` — one row per ``(nodeId, contextKey)``."""
        return self._json(
            "GET", self.capsule_path(tenant, job, capsule) + "/contexts"
        )

    def memory(self, tenant: str, job: str, capsule: str) -> Dict[str, Any]:
        """``GET .../memory`` — full schema-v7 memory sidecar (can be large)."""
        return self._json(
            "GET", self.capsule_path(tenant, job, capsule) + "/memory"
        )

    def decisions(self, tenant: str, job: str, capsule: str) -> List[Dict[str, Any]]:
        """``GET .../decisions`` — the append-only decision log.

        The server replies with NDJSON; each non-empty line is parsed into
        a dict, in log order.
        """
        _, raw, _ = self._request(
            "GET", self.capsule_path(tenant, job, capsule) + "/decisions"
        )
        return self._parse_ndjson(raw)

    # ── Admin ────────────────────────────────────────────────────────────

    def create_token(
        self,
        scope: Mapping[str, Any],
        label: str,
        ttl_seconds: Optional[int] = None,
    ) -> Token:
        """``POST /v1/admin/tokens`` — issues a scoped token (admin only).

        ``scope`` is one of :func:`~syntra_client.models.scope_admin`,
        :func:`~syntra_client.models.scope_tenant_admin`,
        :func:`~syntra_client.models.scope_read`. The raw token value is
        returned exactly once by the server; keep
        :attr:`Token.hash` for revocation.
        """
        if not label:
            raise ValueError("label is required")
        body: Dict[str, Any] = {"scope": dict(scope), "label": label}
        if ttl_seconds is not None:
            body["ttlSeconds"] = int(ttl_seconds)
        payload = self._json("POST", "/v1/admin/tokens", json_body=body)
        return Token.from_response(payload)

    def list_tokens(self) -> List[Dict[str, Any]]:
        """``GET /v1/admin/tokens`` — token records (raw values never returned)."""
        return list(self._json("GET", "/v1/admin/tokens").get("tokens") or [])

    def revoke_token(self, token_hash: str) -> bool:
        """``DELETE /v1/admin/tokens/{tokenHash}`` — ``True`` when revoked."""
        payload = self._json("DELETE", f"/v1/admin/tokens/{self._seg(token_hash)}")
        return bool(payload.get("revoked", payload.get("ok", False)))

    # ── Transport ────────────────────────────────────────────────────────

    def _jobs_path(self, tenant: str) -> str:
        return f"/v1/tenants/{self._seg(tenant)}/jobs"

    @staticmethod
    def _seg(value: str) -> str:
        return urllib.parse.quote(str(value), safe="")

    def _json(
        self,
        method: str,
        path: str,
        *,
        params: Optional[Mapping[str, str]] = None,
        json_body: Optional[Mapping[str, Any]] = None,
        auth: bool = True,
    ) -> Dict[str, Any]:
        _, raw, _ = self._request(
            method, path, params=params, json_body=json_body, auth=auth
        )
        return self._require_dict(raw)

    def _request(
        self,
        method: str,
        path: str,
        *,
        params: Optional[Mapping[str, str]] = None,
        body: Optional[bytes] = None,
        json_body: Optional[Mapping[str, Any]] = None,
        content_type: Optional[str] = None,
        auth: bool = True,
        idempotent: Optional[bool] = None,
    ):
        """One logical call; retried with backoff for idempotent requests.

        Idempotency defaults to the method: ``GET`` is retry-safe, every
        other verb is not unless the caller opts in — installs do, since
        reinstalling identical bytes is a no-op write (see
        :meth:`install_capsule`), while ``decide``/``feedback`` never do.

        Only ``TransportError`` and :class:`ServerError` (``5xx``) are
        retried — every 4xx is a deterministic answer, and re-sending it
        just burns latency.
        """
        if json_body is not None:
            if body is not None:
                raise ValueError("body and json_body are mutually exclusive")
            body = json.dumps(json_body).encode("utf-8")
            content_type = content_type or "application/json"
        if idempotent is None:
            idempotent = method == "GET"
        attempts = self.retries + 1 if idempotent else 1
        last_error: Optional[SyntraError] = None
        for attempt in range(attempts):
            try:
                return self._transact(
                    method, path, params, body, content_type, auth
                )
            except (TransportError, ServerError) as error:
                last_error = error
                if attempt + 1 >= attempts:
                    break
                delay = min(
                    _BACKOFF_CAP_SECONDS,
                    _BACKOFF_BASE_SECONDS * (2 ** attempt),
                )
                time.sleep(delay)
        assert last_error is not None  # loop body always sets it before break
        raise last_error

    def _transact(
        self,
        method: str,
        path: str,
        params: Optional[Mapping[str, str]],
        body: Optional[bytes],
        content_type: Optional[str],
        auth: bool,
    ):
        url = self.base_url + path
        if params:
            url = f"{url}?{urllib.parse.urlencode(dict(params))}"
        request = urllib.request.Request(url, data=body, method=method)
        if content_type:
            request.add_header("Content-Type", content_type)
        request.add_header("Accept", "application/json, text/plain;q=0.9")
        if auth and self.token:
            request.add_header("Authorization", f"Bearer {self.token}")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as resp:
                return resp.status, resp.read(), dict(resp.headers)
        except urllib.error.HTTPError as error:
            raw = error.read() or b""
            headers = dict(error.headers or {})
            self._raise_for_status(error.code, raw, headers, method, path)
        except (urllib.error.URLError, TimeoutError, http.client.HTTPException,
                ConnectionError) as error:
            # Never interpolate the URL: it can carry credentials in the
            # userinfo component, and the token is in a header anyway.
            reason = getattr(error, "reason", None) or error
            raise TransportError(
                f"{method} {path}: {reason}", method=method, path=path
            ) from error
        raise TransportError(  # pragma: no cover - urlopen returned neither
            f"{method} {path}: no response", method=method, path=path
        )

    def _raise_for_status(
        self,
        status: int,
        raw: bytes,
        headers: Mapping[str, str],
        method: str,
        path: str,
    ) -> NoReturn:
        parsed = self._decode_json(raw)
        detail = None
        if isinstance(parsed, dict):
            for key in ("error", "message", "reason"):
                value = parsed.get(key)
                if isinstance(value, str) and value:
                    detail = value
                    break
        if detail is None:
            text = raw.decode("utf-8", "replace").strip()
            detail = (text[:_MAX_ERROR_DETAIL_CHARS] + "…") if len(
                text
            ) > _MAX_ERROR_DETAIL_CHARS else (text or f"HTTP {status}")
        message = f"{method} {path} -> {status}: {detail}"
        kwargs = {"status": status, "body": parsed, "method": method, "path": path}
        if status in (401, 403):
            raise AuthError(message, **kwargs)
        if status == 400:
            raise BadRequestError(message, **kwargs)
        if status == 404:
            raise NotFoundError(message, **kwargs)
        if status == 409:
            raise ConflictError(message, **kwargs)
        if status == 413:
            raise PayloadTooLargeError(message, **kwargs)
        if status == 429:
            raise RateLimitedError(
                message,
                retry_after=self._retry_after(headers, parsed),
                **kwargs,
            )
        if 500 <= status <= 599:
            raise ServerError(message, **kwargs)
        raise HttpStatusError(message, **kwargs)

    @staticmethod
    def _retry_after(
        headers: Mapping[str, str], parsed: Any
    ) -> Optional[float]:
        for name, value in headers.items():
            if name.lower() == "retry-after":
                try:
                    return float(value)
                except (TypeError, ValueError):
                    break
        if isinstance(parsed, dict):
            value = parsed.get("retryAfterSeconds")
            if isinstance(value, (int, float)):
                return float(value)
        return None

    # ── Payload helpers ──────────────────────────────────────────────────

    @staticmethod
    def _decode_json(raw: bytes) -> Any:
        text = raw.decode("utf-8", "replace").strip()
        if not text:
            return None
        try:
            return json.loads(text)
        except ValueError:
            return text

    @classmethod
    def _require_dict(cls, raw: bytes) -> Dict[str, Any]:
        parsed = cls._decode_json(raw)
        if not isinstance(parsed, dict):
            raise SyntraError(
                "expected a JSON object response, got "
                f"{type(parsed).__name__}"
            )
        return parsed

    @staticmethod
    def _parse_ndjson(raw: bytes) -> List[Dict[str, Any]]:
        rows: List[Dict[str, Any]] = []
        text = raw.decode("utf-8", "replace")
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            rows.append(json.loads(line))
        return rows
