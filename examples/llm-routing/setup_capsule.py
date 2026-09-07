"""Install the LLM routing capsule on a running Syntra instance.

Prerequisites:
  • Syntra reachable at $SYNTRA_URL (default http://localhost:8787)
  • $SYNTRA_ADMIN_KEY exported
  • `syntra` binary on PATH (used to compile the YAML spec)

Configuration via environment variables:
  SYNTRA_URL          Syntra base URL (default http://localhost:8787)
  SYNTRA_ADMIN_KEY    Required. Bearer token for the Syntra API.
  SYNTRA_TENANT       Tenant name (default myteam)
  SYNTRA_JOB          Job name   (default llm)
  SYNTRA_CAPSULE      Capsule name (default router)
  SYNTRA_LLM_OPTIONS  Comma-separated option names
                      (default cheap_fast,balanced,expensive_accurate)

Run once before using LLMRouter:

    export SYNTRA_ADMIN_KEY=...
    python setup_capsule.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile

import requests


def _build_capsule_spec(options: list[str]) -> str:
    option_lines = "\n".join(f"  - {opt}" for opt in options)
    return f"""name: llm-routing
options:
{option_lines}
reward:
  type: continuous
  range: [-1.0, 1.0]
"""


LEARNING_CONFIG = {
    "refusal": {"enabled": False},
    "contextSpec": {
        "type": "features",
        "features": [
            {
                "name": "prompt_token_count",
                "type": {"kind": "continuous", "range": [0.0, 100000.0]},
            },
            {
                "name": "task_complexity",
                "type": {"kind": "continuous", "range": [0.0, 1.0]},
            },
            {
                "name": "customer_tier",
                "type": {
                    "kind": "categorical",
                    "values": ["free", "pro", "enterprise"],
                },
            },
            {
                "name": "hour",
                "type": {"kind": "cyclic", "period": 24.0},
            },
        ],
    },
}


def main() -> int:
    syntra_url = os.environ.get("SYNTRA_URL", "http://localhost:8787").rstrip("/")
    admin_key = os.environ.get("SYNTRA_ADMIN_KEY")
    if not admin_key:
        print("ERROR: SYNTRA_ADMIN_KEY is required", file=sys.stderr)
        return 1

    tenant = os.environ.get("SYNTRA_TENANT", "myteam")
    job = os.environ.get("SYNTRA_JOB", "llm")
    capsule = os.environ.get("SYNTRA_CAPSULE", "router")
    capsule_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"

    raw_options = os.environ.get("SYNTRA_LLM_OPTIONS", "cheap_fast,balanced,expensive_accurate")
    options = [o.strip() for o in raw_options.split(",") if o.strip()]
    if not options:
        print("ERROR: SYNTRA_LLM_OPTIONS produced an empty list", file=sys.stderr)
        return 1

    capsule_spec = _build_capsule_spec(options)

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
            headers=headers,
            data=lyc_bytes,
        )
        r.raise_for_status()
        print(f"installed capsule at {capsule_path} with options: {options}")

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
