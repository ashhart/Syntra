"""Official Python SDK for the Syntra decision runtime.

Stdlib only (``urllib``, ``json``, ``dataclasses``) — no transitive
dependencies, so it can sit in a serving path without a supply-chain
footprint.

    from syntra_client import SyntraClient, scope_read

    client = SyntraClient("http://127.0.0.1:8787", token=read_token)
    decision = client.decide("acme", "llm-routing", "model-router",
                             {"contextKey": "support-low-cost"})
    print(decision.chosen_option, decision.decision_id)
"""

from .client import SyntraClient
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
from .models import (
    Decision,
    DecisionItem,
    Token,
    scope_admin,
    scope_read,
    scope_tenant_admin,
)

__version__ = "0.1.0"

__all__ = [
    "__version__",
    "AuthError",
    "BadRequestError",
    "ConflictError",
    "Decision",
    "DecisionItem",
    "HttpStatusError",
    "NotFoundError",
    "PayloadTooLargeError",
    "RateLimitedError",
    "ServerError",
    "SyntraClient",
    "SyntraError",
    "Token",
    "TransportError",
    "scope_admin",
    "scope_read",
    "scope_tenant_admin",
]
