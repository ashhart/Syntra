"""Minimal end-to-end example for the syntra_fraud domain pack.

Prerequisites:
  - Syntra running (e.g. `docker run -p 8787:8787 syntra:demo` or bare metal).
  - Fraud capsule installed: `python setup_capsule.py` (bare metal only).
  - Env vars set: SYNTRA_ADMIN_KEY, optionally SYNTRA_URL and
    SYNTRA_CAPSULE_PATH.
"""
from __future__ import annotations

import os
import random

from syntra_fraud import FraudClient


def main() -> None:
    syntra_url = os.environ.get("SYNTRA_URL", "http://localhost:8787")
    admin_key = os.environ["SYNTRA_ADMIN_KEY"]
    capsule_path = os.environ.get(
        "SYNTRA_CAPSULE_PATH",
        "/tenants/myteam/jobs/fraud/capsules/threshold",
    )

    client = FraudClient(
        syntra_url=syntra_url,
        capsule_path=capsule_path,
        admin_key=admin_key,
    )

    merchant_id = "merch_demo_001"

    # Simulate a stream of transactions with varying risk scores.
    for i in range(20):
        # Synthesize a transaction — in production these come from your scorer.
        risk_score = random.random()
        ticket_usd = round(random.uniform(5.0, 2000.0), 2)

        decision = client.score({
            "merchant_id": merchant_id,
            "risk_score": risk_score,
            "ticket_size_usd": ticket_usd,
        })

        action = "BLOCK" if decision.block else "ALLOW"
        print(
            f"[{i:02d}] risk={risk_score:.3f} threshold={decision.threshold:.1f} "
            f"ticket=${ticket_usd:.2f} -> {action}"
        )

        # Simulate the true outcome (in production, fraud labels come later).
        was_fraud = risk_score > 0.8  # pretend high-score = fraud for the demo

        client.report_outcome(
            decision.decision_id,
            was_fraud=was_fraud,
            merchant_id=merchant_id,
            ticket_size_usd=ticket_usd,
            blocked=decision.block,
        )


if __name__ == "__main__":
    main()
