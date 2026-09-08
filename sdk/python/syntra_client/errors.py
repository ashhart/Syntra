"""Typed exceptions raised by :mod:`syntra_client`.

Every failure surfaces as a :class:`SyntraError` subclass, so callers can
distinguish "the appliance said no" (HTTP status + server message) from
"the appliance was unreachable" (:class:`TransportError`).

Server messages come from the runtime's error bodies
(``{"error": "..."}``); ``__str__`` always includes the method, path and
status so a bare traceback is actionable. The bearer token is never part
of any message.
"""

from __future__ import annotations

from typing import Any, Optional


class SyntraError(Exception):
    """Base class for every error raised by the client."""

    def __init__(
        self,
        message: str,
        *,
        status: Optional[int] = None,
        body: Optional[Any] = None,
        method: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.status = status
        self.body = body
        self.method = method
        self.path = path


class TransportError(SyntraError):
    """The request never produced an HTTP response (DNS, refused, timeout).

    Raised after retries are exhausted for idempotent calls, and
    immediately for non-idempotent ones (``decide`` / ``feedback``), where
    a retry could double-apply a learning effect.
    """


class HttpStatusError(SyntraError):
    """A response with a non-2xx status code."""


class BadRequestError(HttpStatusError):
    """``400`` — malformed body, unknown context shape, bad graph bytes."""


class AuthError(HttpStatusError):
    """``401``/``403`` — missing, unknown, or insufficiently-scoped token."""


class NotFoundError(HttpStatusError):
    """``404`` — unknown tenant/job/capsule, or an unknown ``decisionId``."""


class ConflictError(HttpStatusError):
    """``409`` — resource already exists (e.g. duplicate job id)."""


class PayloadTooLargeError(HttpStatusError):
    """``413`` — request body exceeds the server's 4 MB cap."""


class RateLimitedError(HttpStatusError):
    """``429`` — token-bucket throttle for this principal.

    ``retry_after`` is the server's ``Retry-After`` hint in seconds
    (``None`` when the response omitted it).
    """

    def __init__(
        self,
        message: str,
        *,
        retry_after: Optional[float] = None,
        status: Optional[int] = 429,
        body: Optional[Any] = None,
        method: Optional[str] = None,
        path: Optional[str] = None,
    ) -> None:
        super().__init__(
            message, status=status, body=body, method=method, path=path
        )
        self.retry_after = retry_after


class ServerError(HttpStatusError):
    """``5xx`` — runtime fault. Retried for idempotent calls only."""
