#!/usr/bin/env python3
"""Install flagship demo capsules into the running demo Syntra.

Idempotent: re-running replaces the graph, learning config, and any
hierarchical sidecar in place.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ADMIN_KEY = os.environ["LYCAN_ADMIN_KEY"]
SYNTRA_URL = os.environ.get("SYNTRA_URL", "http://127.0.0.1:8787")
CAPSULES_ROOT = os.environ.get("SYNTRA_CAPSULES_ROOT", "/syntra/demo/capsules")

# (source-dir-name, tenant, job, capsule).
CAPSULES: list[tuple[str, str, str, str]] = [
    ("predictive-autoscaling",          "demo", "autoscale",  "orders"),
    ("anomaly-routing",                 "demo", "routing",    "api"),
    ("seasonal-fraud-threshold",        "demo", "fraud",      "threshold"),
    ("shared-state-action-embeddings",  "demo", "embeddings", "router"),
    ("hierarchical-region-routing",     "demo", "region",     "router"),
]


def _req(
    method: str,
    path: str,
    body: bytes | None = None,
    content_type: str | None = None,
) -> dict:
    req = urllib.request.Request(SYNTRA_URL + path, data=body, method=method)
    req.add_header("Authorization", f"Bearer {ADMIN_KEY}")
    if content_type:
        req.add_header("Content-Type", content_type)
    with urllib.request.urlopen(req, timeout=15) as resp:
        raw = resp.read()
    if not raw:
        return {}
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return {"raw": raw[:200].decode("utf-8", errors="replace")}


def _compile(source_dir: str) -> bytes:
    """Run `lycan compile program.lycs` in source_dir and return the .lyc bytes."""
    lycs = os.path.join(source_dir, "program.lycs")
    lyc = os.path.join(source_dir, "program.lyc")
    # Recompile in-container; we don't trust the shipped .lyc to match.
    result = subprocess.run(
        ["lycan", "compile", lycs],
        capture_output=True,
        text=True,
        check=False,
        cwd=source_dir,
    )
    if result.returncode != 0:
        sys.stderr.write(
            f"[install] lycan compile failed for {source_dir}:\n"
            f"  stdout: {result.stdout.strip()}\n"
            f"  stderr: {result.stderr.strip()}\n"
        )
        raise SystemExit(2)
    with open(lyc, "rb") as f:
        return f.read()


def _install_one(name: str, tenant: str, job: str, capsule: str) -> None:
    source_dir = os.path.join(CAPSULES_ROOT, name)
    if not os.path.isdir(source_dir):
        sys.stderr.write(f"[install] source dir missing: {source_dir}\n")
        raise SystemExit(2)

    lyc_bytes = _compile(source_dir)

    base = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
    install_resp = _req(
        "POST",
        f"{base}/install",
        body=lyc_bytes,
        content_type="application/octet-stream",
    )

    learning_path = os.path.join(source_dir, "learning.json")
    if os.path.isfile(learning_path):
        with open(learning_path, "rb") as f:
            learning_body = f.read()
        _req(
            "PUT",
            f"{base}/learning",
            body=learning_body,
            content_type="application/json",
        )
        learning_note = "+ learning.json"
    else:
        learning_note = "(no learning.json)"

    # Upload the hierarchical sidecar; without it the runtime falls back
    # to flat AdaptiveChoice over leaf names.
    hier_path = os.path.join(source_dir, "hierarchical_spec.json")
    if os.path.isfile(hier_path):
        with open(hier_path, "rb") as f:
            hier_body = f.read()
        _req(
            "PUT",
            f"{base}/hierarchical_spec",
            body=hier_body,
            content_type="application/json",
        )
        learning_note += " + hierarchical_spec.json"

    hashed = (install_resp.get("hash") or "")[:12]
    print(
        f"[install] {name:35s} -> {tenant}/{job}/{capsule:12s} "
        f"hash={hashed:12s} {learning_note}",
        flush=True,
    )


def main() -> int:
    print(f"[install] installing {len(CAPSULES)} flagship capsules into {SYNTRA_URL}", flush=True)
    for name, tenant, job, capsule in CAPSULES:
        _install_one(name, tenant, job, capsule)
    print("[install] all capsules installed", flush=True)
    return 0


if __name__ == "__main__":
    last_err: Exception | None = None
    for attempt in range(8):
        try:
            sys.exit(main())
        except urllib.error.URLError as e:
            last_err = e
            sys.stderr.write(f"[install] retry {attempt + 1}/8 after URL error: {e}\n")
            time.sleep(1.5)
    sys.stderr.write(f"[install] giving up after retries: {last_err}\n")
    sys.exit(1)
