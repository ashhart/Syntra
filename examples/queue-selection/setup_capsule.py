"""Install the queue-selection capsule on a running Syntra instance.

Prerequisites:
  • Syntra reachable at $SYNTRA_URL (default http://localhost:8787)
  • $SYNTRA_ADMIN_KEY exported
  • `syntra` binary on PATH (used to compile the YAML spec)

Run once before using QueueClient:

    export SYNTRA_ADMIN_KEY=...
    python setup_capsule.py

The number of backend options defaults to 3 and can be overridden:

    SYNTRA_QUEUE_N_BACKENDS=5 python setup_capsule.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile

import requests


def _build_capsule_spec(n_backends: int) -> str:
    options_block = "\n".join(
        f"  - backend_{chr(ord('a') + i)}" for i in range(n_backends)
    )
    return f"""name: queue-selection
options:
{options_block}
reward:
  type: continuous
  range: [-1.0, 1.0]
"""


LEARNING_CONFIG = {
    "refusal": {"enabled": False},
    "contextSpec": {
        "type": "features",
        "features": [
            {"name": "avg_recent_latency_ms", "type": {"kind": "continuous", "range": [0.0, 5000.0]}},
            {"name": "recent_error_rate",     "type": {"kind": "continuous", "range": [0.0, 1.0]}},
            {"name": "current_queue_depth",   "type": {"kind": "continuous", "range": [0.0, 1000.0]}},
            {"name": "request_size_kb",       "type": {"kind": "continuous", "range": [0.0, 10000.0]}},
        ],
    },
}


def main() -> int:
    syntra_url = os.environ.get("SYNTRA_URL", "http://localhost:8787").rstrip("/")
    admin_key = os.environ.get("SYNTRA_ADMIN_KEY")
    if not admin_key:
        print("ERROR: SYNTRA_ADMIN_KEY is required", file=sys.stderr)
        return 1

    n_backends = int(os.environ.get("SYNTRA_QUEUE_N_BACKENDS", "3"))
    if n_backends < 1:
        print("ERROR: SYNTRA_QUEUE_N_BACKENDS must be >= 1", file=sys.stderr)
        return 1

    tenant = os.environ.get("SYNTRA_TENANT", "myteam")
    job = os.environ.get("SYNTRA_JOB", "queue")
    capsule = os.environ.get("SYNTRA_CAPSULE", "router")
    capsule_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"

    capsule_spec = _build_capsule_spec(n_backends)

    with tempfile.TemporaryDirectory() as tmpdir:
        spec_path = os.path.join(tmpdir, "spec.yaml")
        out_dir = os.path.join(tmpdir, "out")
        with open(spec_path, "w") as f:
            f.write(capsule_spec)

        try:
            subprocess.run(
                ["syntra", "author", spec_path, "--out-dir", out_dir],
                check=True,
            )
        except FileNotFoundError:
            print("ERROR: `syntra` binary not on PATH. Install Syntra first.", file=sys.stderr)
            return 1
        except subprocess.CalledProcessError as e:
            print(f"ERROR: syntra author failed: {e}", file=sys.stderr)
            return 1

        with open(os.path.join(out_dir, "program.lyc"), "rb") as f:
            lyc_bytes = f.read()

        headers = {"Authorization": f"Bearer {admin_key}"}

        r = requests.post(
            f"{syntra_url}{capsule_path}/install",
            headers=headers, data=lyc_bytes,
        )
        r.raise_for_status()
        print(f"installed capsule at {capsule_path} with {n_backends} backend(s)")

        r = requests.put(
            f"{syntra_url}{capsule_path}/learning",
            headers={**headers, "Content-Type": "application/json"},
            data=json.dumps(LEARNING_CONFIG),
        )
        r.raise_for_status()
        print("attached feature-context learning config (LinUCB-eligible)")

        print()
        print("Set these for your application:")
        print(f"  export SYNTRA_URL={syntra_url}")
        print(f"  export SYNTRA_CAPSULE_PATH={capsule_path}")
        print("  export SYNTRA_ADMIN_KEY=<your-key>")
        return 0


if __name__ == "__main__":
    sys.exit(main())
