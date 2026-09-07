"""Minimal end-to-end example for the syntra_queue domain pack.

Prerequisites:
  • Syntra running (e.g. `docker run -p 8787:8787 syntra:demo` or bare metal).
  • Queue capsule installed: `python setup_capsule.py`.
  • Env vars set: SYNTRA_ADMIN_KEY, optionally SYNTRA_URL and
    SYNTRA_CAPSULE_PATH.
"""
from __future__ import annotations

import os
import random
import time

from syntra_queue import QueueClient


def main() -> None:
    syntra_url = os.environ.get("SYNTRA_URL", "http://localhost:8787")
    admin_key = os.environ["SYNTRA_ADMIN_KEY"]
    capsule_path = os.environ.get(
        "SYNTRA_CAPSULE_PATH",
        "/tenants/myteam/jobs/queue/capsules/router",
    )

    client = QueueClient(
        syntra_url=syntra_url,
        capsule_path=capsule_path,
        admin_key=admin_key,
    )

    # Simulate 30 requests with randomized queue depths and sizes.
    for i in range(30):
        queue_depths = {
            "backend_a": random.randint(0, 200),
            "backend_b": random.randint(0, 50),
            "backend_c": random.randint(100, 500),
        }
        request_size_kb = random.uniform(1.0, 500.0)

        pick = client.pick(request_size_kb=request_size_kb, queue_depths=queue_depths)
        print(f"[{i:02d}] selected={pick.backend_name}  decision_id={pick.decision_id}")

        # Simulate sending the request.
        start = time.monotonic()
        time.sleep(random.uniform(0.005, 0.05))
        latency_ms = (time.monotonic() - start) * 1000.0
        success = random.random() > 0.1  # 90 % success rate

        client.report(
            decision_id=pick.decision_id,
            backend_name=pick.backend_name,
            success=success,
            latency_ms=latency_ms,
        )
        print(f"       success={success}  latency_ms={latency_ms:.1f}")


if __name__ == "__main__":
    main()
