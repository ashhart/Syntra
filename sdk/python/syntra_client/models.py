"""Dataclasses returned by :class:`~syntra_client.client.SyntraClient`.

Every object keeps the untouched server payload in ``raw``, so fields the
SDK has not modelled yet (new in a later runtime release) stay reachable
without a client upgrade.

Server JSON is camelCase (``decisionId``) with two snake_case exceptions
inside decision items (``node_id``, ``chosen_option``); the field names
here follow the canonical shapes in ``docs/openapi.yaml``.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Dict, Optional, Tuple

REDACTED = "[redacted]"


@dataclass(frozen=True)
class DecisionItem:
    """One strategy-node choice inside a :class:`Decision`.

    ``node_id`` / ``chosen_option`` mirror the server's snake_case keys.
    """

    node_id: int
    chosen_option: int
    confidence: Optional[float] = None
    objective: Optional[str] = None
    weights: Tuple[float, ...] = ()
    activations: Optional[int] = None
    candidate_id: Optional[str] = None
    raw: Dict[str, Any] = field(default_factory=dict, compare=False, repr=False)

    @classmethod
    def from_response(cls, item: Dict[str, Any]) -> "DecisionItem":
        return cls(
            node_id=int(item["node_id"]),
            chosen_option=int(item["chosen_option"]),
            confidence=item.get("confidence"),
            objective=item.get("objective"),
            weights=tuple(item.get("weights") or ()),
            activations=item.get("activations"),
            candidate_id=item.get("candidateId"),
            raw=item,
        )


@dataclass(frozen=True)
class Decision:
    """Result of ``POST /v1/.../decide``.

    ``chosen_option`` is the first entry of the server's ``decisions[]``
    array and is ``None`` when the capsule refused the request (a refused
    decision carries no options — fall back to your default behaviour).
    ``learned`` is only ever true when ``learn=True`` was requested *and*
    the token's scope permits mutation; ``read``-scoped tokens are
    silently downgraded to read-only by the runtime.
    """

    decision_id: str
    chosen_option: Optional[int]
    context_key: Optional[str] = None
    algorithm: Optional[str] = None
    learned: bool = False
    refused: bool = False
    ood_score: Optional[float] = None
    decisions: Tuple[DecisionItem, ...] = ()
    warmup: Optional[Dict[str, Any]] = None
    confidence: Optional[Dict[str, Any]] = None
    result: Any = None
    stdout: Tuple[Any, ...] = ()
    raw: Dict[str, Any] = field(default_factory=dict, compare=False, repr=False)

    @property
    def confidence_value(self) -> Optional[float]:
        """Chosen-option weight of the first decision, when present."""
        return self.decisions[0].confidence if self.decisions else None

    @classmethod
    def from_response(cls, payload: Dict[str, Any]) -> "Decision":
        items = tuple(
            DecisionItem.from_response(item)
            for item in payload.get("decisions") or ()
        )
        return cls(
            decision_id=payload["decisionId"],
            chosen_option=items[0].chosen_option if items else None,
            context_key=payload.get("contextKey"),
            algorithm=payload.get("algorithm"),
            learned=bool(payload.get("learned", False)),
            refused=bool(payload.get("refused", False)),
            ood_score=payload.get("oodScore"),
            decisions=items,
            warmup=payload.get("warmup"),
            confidence=payload.get("confidence"),
            result=payload.get("result"),
            stdout=tuple(payload.get("stdout") or ()),
            raw=payload,
        )


@dataclass(frozen=True)
class Token:
    """A scoped bearer token issued by ``POST /v1/admin/tokens``.

    The raw value is returned by the server exactly once, so it is kept as
    a field — but both ``__repr__`` and ``__str__`` mask it, so a token
    cannot leak through logging, ``pprint``, or a stray traceback.
    """

    token: str
    hash: str
    scope: Dict[str, Any]
    expires_at: Optional[int] = None
    raw: Dict[str, Any] = field(default_factory=dict, compare=False, repr=False)

    @classmethod
    def from_response(cls, payload: Dict[str, Any]) -> "Token":
        return cls(
            token=payload["token"],
            hash=payload["hash"],
            scope=payload.get("scope") or {},
            expires_at=payload.get("expiresAt"),
            raw=payload,
        )

    def __repr__(self) -> str:
        return (
            f"Token(token={REDACTED}, hash={self.hash!r}, "
            f"scope={self.scope!r}, expires_at={self.expires_at!r})"
        )

    __str__ = __repr__


def scope_admin() -> Dict[str, Any]:
    """Admin scope: any route, any tenant."""
    return {"kind": "admin"}


def scope_tenant_admin(tenant: str) -> Dict[str, Any]:
    """Tenant-admin scope: all routes on one tenant."""
    return {"kind": "tenant_admin", "tenant": tenant}


def scope_read(tenant: str, job: str, capsule: str) -> Dict[str, Any]:
    """Read scope: ``/decide`` plus read-only inspection of one capsule.

    ``/feedback`` with this scope is rejected ``403``, and ``learn=true``
    on ``/decide`` is downgraded to read-only.
    """
    return {"kind": "read", "tenant": tenant, "job": job, "capsule": capsule}
