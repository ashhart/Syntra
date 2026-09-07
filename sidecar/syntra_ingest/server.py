"""HTTP server.

Flask was picked over `http.server` because the routing is trivial and Flask
costs us one dependency we already needed-ish (it's the same shape as every
other small Python sidecar in the org). If you'd rather avoid the dep,
porting to `http.server` is ~30 lines.

Two routes:

    GET /features/current   → 200, JSON snapshot
    GET /healthz            → 200 if any feature updated within
                              staleness_seconds, else 503.

Bind to localhost by default. There is no auth. See README.
"""

from __future__ import annotations

import logging

from flask import Flask, jsonify

from .cache import FeatureCache

log = logging.getLogger(__name__)


def create_app(cache: FeatureCache, staleness_seconds: float) -> Flask:
    app = Flask("syntra-ingest")

    @app.get("/features/current")
    def features_current():  # type: ignore[unused-variable]
        return jsonify(cache.snapshot())

    @app.get("/healthz")
    def healthz():  # type: ignore[unused-variable]
        age = cache.freshest_age()
        if age is None:
            return jsonify({"status": "cold", "reason": "cache empty"}), 503
        if age > staleness_seconds:
            return (
                jsonify(
                    {
                        "status": "stale",
                        "freshest_age_seconds": round(age, 3),
                        "staleness_seconds": staleness_seconds,
                    }
                ),
                503,
            )
        return jsonify(
            {"status": "ok", "freshest_age_seconds": round(age, 3)}
        )

    return app
