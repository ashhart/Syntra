"""Minimal end-to-end example for the syntra_retry domain pack.

Prerequisites:
  • Syntra running (e.g. `docker run -p 8787:8787 syntra:demo` or bare metal).
  • Retry capsule installed: `python setup_capsule.py` (bare metal only —
    the demo image pre-installs an equivalent capsule, but at a different
    path; adjust SYNTRA_CAPSULE_PATH accordingly).
  • Env vars set: SYNTRA_ADMIN_KEY, optionally SYNTRA_URL and
    SYNTRA_CAPSULE_PATH.
"""
from __future__ import annotations

import os

from syntra_retry import RetryClient


def main() -> None:
    syntra_url = os.environ.get("SYNTRA_URL", "http://localhost:8787")
    admin_key = os.environ["SYNTRA_ADMIN_KEY"]
    capsule_path = os.environ.get(
        "SYNTRA_CAPSULE_PATH",
        "/tenants/myteam/jobs/retry/capsules/router",
    )

    client = RetryClient(
        syntra_url=syntra_url,
        capsule_path=capsule_path,
        admin_key=admin_key,
    )

    # Reliable endpoint — Syntra should converge on the cheapest policy ("none").
    r = client.request("GET", "https://httpbin.org/status/200", timeout=5.0)
    print(f"reliable endpoint: status={r.status_code}")

    # Flaky endpoint — Syntra should start preferring policies with retries.
    for i in range(20):
        try:
            r = client.request(
                "GET",
                "https://httpbin.org/status/200,500,500,500",
                timeout=5.0,
            )
            print(f"flaky[{i:02d}]: status={r.status_code}")
        except Exception as e:
            print(f"flaky[{i:02d}]: failed: {e}")


if __name__ == "__main__":
    main()
