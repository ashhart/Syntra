"""Continuous action-space pricing walkthrough.

Demonstrates:
  - Installing a 5-bucket capsule (b0..b4) that represents a continuous price
    range of [10, 60].
  - PUT /learning with actionSpace: {type: continuous, range: [10, 60], buckets: 5}
  - Sending 50 decide requests with random context keys.
  - Printing each response's chosen_option index and the corresponding
    chosenAction midpoint from the server.
  - Verifying that the midpoints equal (60-10)/5 * (i+0.5) + 10 for each
    bucket i: 15.0, 25.0, 35.0, 45.0, 55.0.

Prerequisites:
  - Syntra running at $SYNTRA_URL (default http://localhost:8787).
  - Admin key exported as $SYNTRA_ADMIN_KEY (or passed via --admin-key).
  - The `syntra` CLI binary on PATH for authoring the capsule from YAML.

Usage:
    python3 02_continuous_action_pricing.py [--syntra-url URL] [--admin-key KEY]

Apache-2.0.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import subprocess
import sys
import tempfile
import urllib.request
import urllib.error

CAPSULE_YAML = """\
name: pricing-continuous
options:
  - b0
  - b1
  - b2
  - b3
  - b4
reward:
  type: continuous
  range: [-1.0, 1.0]
"""

RANGE_LO = 10.0
RANGE_HI = 60.0
N_BUCKETS = 5
EXPECTED_MIDPOINTS = [
    RANGE_LO + (RANGE_HI - RANGE_LO) / N_BUCKETS * (i + 0.5)
    for i in range(N_BUCKETS)
]  # [15.0, 25.0, 35.0, 45.0, 55.0]


def api(url: str, method: str, path: str, body=None, token: str | None = None,
        raw_bytes: bytes | None = None) -> tuple[int, dict]:
    if raw_bytes is not None:
        data = raw_bytes
        headers = {"Content-Type": "application/octet-stream"}
    elif body is not None:
        data = json.dumps(body).encode()
        headers = {"Content-Type": "application/json"}
    else:
        data = None
        headers = {}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(f"{url}{path}", data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req) as resp:
            return resp.status, json.loads(resp.read())
    except urllib.error.HTTPError as exc:
        try:
            return exc.code, json.loads(exc.read())
        except Exception:
            return exc.code, {"error": exc.reason}


def compile_capsule(yaml_text: str) -> bytes:
    with tempfile.TemporaryDirectory() as tmpdir:
        spec_path = os.path.join(tmpdir, "spec.yaml")
        out_dir = os.path.join(tmpdir, "out")
        with open(spec_path, "w") as f:
            f.write(yaml_text)
        try:
            subprocess.run(
                ["syntra", "author", spec_path, "--out-dir", out_dir],
                check=True, capture_output=True,
            )
        except FileNotFoundError:
            raise SystemExit("ERROR: `syntra` binary not on PATH. Build it with `cargo build --release --bin syntra`.")
        except subprocess.CalledProcessError as exc:
            raise SystemExit(f"ERROR: syntra author failed: {exc.stderr.decode()}")
        lyc_path = os.path.join(out_dir, "program.lyc")
        with open(lyc_path, "rb") as f:
            return f.read()


def print_section(title: str) -> None:
    print(f"\n{'='*60}")
    print(f"  {title}")
    print(f"{'='*60}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"))
    parser.add_argument("--admin-key", default=os.environ.get("SYNTRA_ADMIN_KEY", ""))
    parser.add_argument("--tenant", default="demo")
    parser.add_argument("--job", default="pricing")
    parser.add_argument("--capsule", default="continuous")
    parser.add_argument("--rounds", type=int, default=50)
    parser.add_argument("--seed", type=int, default=42)
    args = parser.parse_args()

    url = args.syntra_url.rstrip("/")
    key = args.admin_key
    if not key:
        raise SystemExit("ERROR: --admin-key or $SYNTRA_ADMIN_KEY is required")

    cp = f"/tenants/{args.tenant}/jobs/{args.job}/capsules/{args.capsule}"
    rng = random.Random(args.seed)

    # ── 1. Author + install ────────────────────────────────────────────────
    print_section("1. Authoring and installing capsule")
    lyc_bytes = compile_capsule(CAPSULE_YAML)
    status, body = api(url, "POST", f"{cp}/install", token=key, raw_bytes=lyc_bytes)
    if status != 200:
        raise SystemExit(f"Install failed {status}: {body}")
    print(json.dumps({"installed": True, "hash": body.get("hash", "?")[:16] + "..."}))

    # ── 2. PUT learning config (continuous action space) ──────────────────
    print_section("2. Configuring continuous action space")
    learn_cfg = {
        "actionSpace": {
            "type": "continuous",
            "range": [RANGE_LO, RANGE_HI],
            "buckets": N_BUCKETS,
        },
        "refusal": {"enabled": False},
    }
    status, body = api(url, "PUT", f"{cp}/learning", body=learn_cfg, token=key)
    if status != 200:
        raise SystemExit(f"PUT /learning failed {status}: {body}")
    print(json.dumps({"actionSpace": body.get("config", {}).get("actionSpace", {})}))

    # ── 3. Drive 50 decides, collect midpoints ────────────────────────────
    print_section(f"3. Running {args.rounds} decide rounds")
    print(f"{'round':>6}  {'chosen_option':>13}  {'chosenAction':>12}  {'expected_mid':>12}  match")

    verified = True
    for i in range(args.rounds):
        ctx_key = f"ctx_{rng.randint(0, 9)}"
        status, resp = api(url, "POST", f"{cp}/decide",
                           body={"contextKey": ctx_key}, token=key)
        if status != 200:
            print(json.dumps({"round": i, "error": resp}))
            continue

        decisions = resp.get("decisions", [])
        if not decisions:
            print(json.dumps({"round": i, "refused": resp.get("refused"), "skipped": True}))
            continue

        chosen = decisions[0].get("chosen_option", -1)
        chosen_action = resp.get("chosenAction")

        expected = EXPECTED_MIDPOINTS[chosen] if 0 <= chosen < N_BUCKETS else None
        match = (
            chosen_action is not None
            and expected is not None
            and abs(chosen_action - expected) < 0.001
        )
        if not match:
            verified = False

        if i < 10 or i >= args.rounds - 5:
            print(f"{i:>6}  {chosen:>13}  {chosen_action!r:>12}  {expected!r:>12}  {'OK' if match else 'FAIL'}")

    print(f"\nMidpoint verification: {'PASS' if verified else 'FAIL'}")
    print(f"Expected midpoints: {EXPECTED_MIDPOINTS}")

    # ── 4. Quick smoke check ───────────────────────────────────────────────
    print_section("4. Learning config (final state)")
    status, body = api(url, "GET", f"{cp}/learning", token=key)
    if status == 200:
        print(json.dumps(body.get("actionSpace", body), indent=2))


if __name__ == "__main__":
    main()
