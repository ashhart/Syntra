#!/usr/bin/env python3
"""End-to-end smoke test for the Syntra Python SDK.

Boots a real appliance (`syntra serve`) against a temporary store on a
free loopback port, then drives the public SDK surface against it:
install, decide, feedback, report/contexts/memory/decisions, scoped-token
semantics, and the typed error mapping. No mocks, no test runner — plain
assertions, exit 0 on success, 1 on the first failure.

    python3 sdk/python/tests/smoke.py
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

TESTS_DIR = Path(__file__).resolve().parent
SDK_DIR = TESTS_DIR.parent
REPO_ROOT = SDK_DIR.parent.parent

sys.path.insert(0, str(SDK_DIR))

from syntra_client import (  # noqa: E402
    AuthError,
    BadRequestError,
    NotFoundError,
    SyntraClient,
    Token,
    scope_read,
)

CAPSULE_FILE = REPO_ROOT / "examples" / "demo_llm_model_router.lyc"
TENANT = "acme"
JOB = "llm-routing"
CAPSULE = "model-router"
CONTEXT = "support-low-cost"
ADMIN_KEY = "smoke-admin-key-" + ("0" * 24)
BUILD_TIMEOUT_SECONDS = 3600
SERVE_READY_TIMEOUT_SECONDS = 60


def log(step: str) -> None:
    print(f"  · {step}", flush=True)


def check(condition: bool, description: str) -> None:
    if not condition:
        raise AssertionError(description)
    print(f"  ✓ {description}", flush=True)


def raises(error_type, call, description: str, *, status: int | None = None):
    """Assert `call()` raises `error_type` (optionally with `status`)."""
    try:
        call()
    except error_type as error:
        if status is not None:
            check(
                getattr(error, "status", None) == status,
                f"{description} (status {status})",
            )
        else:
            check(True, description)
        return error
    except Exception as error:  # noqa: BLE001 — report the wrong type clearly
        raise AssertionError(
            f"{description}: raised {type(error).__name__}: {error}"
        ) from error
    raise AssertionError(f"{description}: no exception raised")


def resolve_binary() -> Path:
    binary = REPO_ROOT / "target" / "release" / "syntra"
    if binary.exists():
        log(f"using existing {binary.relative_to(REPO_ROOT)}")
        return binary
    log("target/release/syntra missing — cargo build --release --quiet")
    result = subprocess.run(
        ["cargo", "build", "--release", "--quiet", "--bin", "syntra"],
        cwd=REPO_ROOT,
        timeout=BUILD_TIMEOUT_SECONDS,
    )
    if result.returncode != 0 or not binary.exists():
        raise RuntimeError(
            f"cargo build failed (exit {result.returncode}); "
            "build target/release/syntra and re-run"
        )
    return binary


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def wait_for_health(addr: str, deadline: float) -> None:
    url = f"http://{addr}/health"
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1.0) as resp:
                if resp.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            last_error = error
        time.sleep(0.2)
    raise RuntimeError(f"server never became healthy at {url}: {last_error}")


def run_checks(admin: SyntraClient) -> None:
    base_url = admin.base_url

    # ── Infra ────────────────────────────────────────────────────────────
    health = admin.health()
    check(health.get("ok") is True and health.get("service") == "Syntra",
          f"GET /health -> {health}")

    # ── Job + install ────────────────────────────────────────────────────
    job = admin.create_job(TENANT, JOB, name="LLM Routing")
    check(job.get("id") == JOB, f"POST /v1/tenants/{TENANT}/jobs -> job '{JOB}'")

    payload = CAPSULE_FILE.read_bytes()
    expected_hash = hashlib.sha256(payload).hexdigest()
    installed_hash = admin.install_capsule(TENANT, JOB, CAPSULE, payload)
    check(installed_hash == expected_hash,
          f"install_capsule -> {installed_hash[:16]}… matches sha256 of the .lyc")

    fresh = admin.report(TENANT, JOB, CAPSULE)
    check(fresh.get("hash") == expected_hash,
          "report graph hash == installed graph hash (before any feedback)")
    fresh_weights = (fresh.get("strategies") or [{}])[0].get("graphWeights") or []
    check(len(fresh_weights) == 3 and abs(sum(fresh_weights) - 1.0) < 1e-3,
          f"fresh graph weights present: {fresh_weights}")

    raises(
        BadRequestError,
        lambda: admin.install_capsule(TENANT, JOB, "bogus-graph", b"not-a-lyc"),
        "installing non-.lyc bytes -> BadRequestError",
        status=400,
    )

    # ── Decide (shadow: no in-band learning) ─────────────────────────────
    decision = admin.decide(TENANT, JOB, CAPSULE, {"contextKey": CONTEXT})
    check(bool(decision.decision_id), f"decide -> decisionId {decision.decision_id}")
    check(decision.refused is False, "decide not refused")
    check(decision.chosen_option in (0, 1, 2),
          f"chosen_option present: {decision.chosen_option}")
    check(len(decision.decisions) == 1
          and len(decision.decisions[0].weights) == 3,
          f"decisions[0].weights = {list(decision.decisions[0].weights)}")
    check(decision.learned is False, "shadow decide did not learn")
    check(decision.raw.get("warmup", {}).get("state") in ("warmup", "active", "frozen"),
          f"warmup state reported: {decision.raw.get('warmup')}")
    check(decision.raw is not None and "decisionId" in decision.raw,
          "Decision.raw keeps the untouched server payload")

    # ── Feedback ─────────────────────────────────────────────────────────
    accepted = admin.feedback(TENANT, JOB, CAPSULE, decision.decision_id, 0.85)
    check(accepted is True, "feedback(reward=0.85) accepted")

    raises(
        NotFoundError,
        lambda: admin.feedback(TENANT, JOB, CAPSULE, "dec_does_not_exist", 1.0),
        "feedback for an unknown decisionId -> NotFoundError",
        status=404,
    )

    # ── Inspection ───────────────────────────────────────────────────────
    report = admin.report(TENANT, JOB, CAPSULE)
    strategies = report.get("strategies") or []
    check(len(strategies) == 1, "report has the capsule's strategy node")
    weights = strategies[0].get("graphWeights") or []
    check(len(weights) == 3 and abs(sum(weights) - 1.0) < 1e-3,
          f"report weights present: {weights}")
    moved = weights[decision.chosen_option] - fresh_weights[decision.chosen_option]
    check(moved > 0,
          f"feedback moved option {decision.chosen_option}'s weight "
          f"{fresh_weights[decision.chosen_option]} -> "
          f"{weights[decision.chosen_option]}")
    options = strategies[0].get("options") or []
    check([opt.get("weight") for opt in options] == weights,
          f"report per-option view matches graph weights: "
          f"{[opt.get('weight') for opt in options]}")

    contexts = admin.contexts(TENANT, JOB, CAPSULE)
    context_keys = [row.get("contextKey") for row in contexts.get("contexts") or []]
    check(CONTEXT in context_keys,
          f"contexts() bucketed under '{CONTEXT}': {context_keys}")

    memory = admin.memory(TENANT, JOB, CAPSULE)
    check(memory.get("version") == 7, "memory sidecar is schema v7")
    mem_nodes = memory.get("strategies") or {}
    check(len(mem_nodes) == 1 and CONTEXT in (mem_nodes[list(mem_nodes)[0]].get("contexts") or {}),
          "memory holds the per-context bucket")

    log_rows = admin.decisions(TENANT, JOB, CAPSULE)
    check(any(row.get("id") == decision.decision_id for row in log_rows),
          f"decisions() NDJSON parsed into {len(log_rows)} row(s), our decision present")

    # ── Scoped tokens ────────────────────────────────────────────────────
    read_token = admin.create_token(
        scope_read(TENANT, JOB, CAPSULE), "gateway-read", ttl_seconds=3600
    )
    check(isinstance(read_token, Token) and bool(read_token.token)
          and len(read_token.hash) == 64,
          f"create_token -> read-scoped token (hash {read_token.hash[:12]}…)")

    reader = SyntraClient(base_url, token=read_token.token, timeout=5.0)
    who = reader.whoami()
    check(who.get("kind") == "scoped_token"
          and who.get("scope", {}).get("kind") == "read",
          f"whoami -> {who.get('kind')} / scope {who.get('scope')}")

    shadow = reader.decide(TENANT, JOB, CAPSULE, {"contextKey": CONTEXT}, learn=True)
    check(shadow.chosen_option in (0, 1, 2),
          "read token may still decide")
    check(shadow.learned is False,
          "read token + learn=True is downgraded to read-only (learned=false)")

    raises(
        AuthError,
        lambda: reader.feedback(TENANT, JOB, CAPSULE, decision.decision_id, 1.0),
        "read token cannot post feedback -> AuthError",
        status=403,
    )

    # ── Auth failure + missing resources ─────────────────────────────────
    stranger = SyntraClient(base_url, token="definitely-not-a-real-token", timeout=5.0)
    raises(AuthError, lambda: stranger.decide(TENANT, JOB, CAPSULE,
                                             {"contextKey": CONTEXT}),
           "bad token -> AuthError", status=401)
    raises(NotFoundError, lambda: admin.decide(TENANT, JOB, "no-such-capsule",
                                              {"contextKey": CONTEXT}),
           "decide on an uninstalled capsule -> NotFoundError", status=404)

    # ── Metrics + secret hygiene ─────────────────────────────────────────
    metrics = admin.metrics()
    check("syntra_requests_total" in metrics and "syntra_warmup_state" in metrics,
          "GET /metrics exposes the Syntra series")

    check(read_token.token not in repr(reader)
          and read_token.token not in repr(read_token)
          and "[redacted]" in repr(reader),
          "repr() never leaks the bearer token")

    try:
        json.dumps({"client": repr(admin), "token": repr(read_token)})
    except TypeError as error:  # pragma: no cover - reprs must stay serialisable
        raise AssertionError(f"repr() output is not plain data: {error}") from error


def main() -> int:
    if not CAPSULE_FILE.exists():
        print(f"FAIL: missing fixture {CAPSULE_FILE}", file=sys.stderr)
        return 1
    try:
        binary = resolve_binary()
    except Exception as error:  # noqa: BLE001
        print(f"FAIL: {error}", file=sys.stderr)
        return 1

    port = free_port()
    addr = f"127.0.0.1:{port}"
    store = Path(tempfile.mkdtemp(prefix="syntra-smoke-store-"))
    logdir = Path(tempfile.mkdtemp(prefix="syntra-smoke-log-"))
    server_log = server = None
    print(f"Syntra Python SDK smoke test — {addr}, store {store}", flush=True)
    try:
        server_log = open(server_log_path := logdir / "server.log", "wb")
        server = subprocess.Popen(
            [str(binary), "serve", "--addr", addr, "--store", str(store),
             "--admin-key", ADMIN_KEY],
            stdout=server_log,
            stderr=subprocess.STDOUT,
            cwd=REPO_ROOT,
            env={**os.environ, "RUST_LOG": os.environ.get("RUST_LOG", "warn")},
        )
        wait_for_health(addr, time.monotonic() + SERVE_READY_TIMEOUT_SECONDS)
        log(f"server up (pid {server.pid})")

        admin = SyntraClient(f"http://{addr}", token=ADMIN_KEY, timeout=10.0, retries=2)
        run_checks(admin)
    except BaseException as error:  # noqa: BLE001 — always report + clean up
        detail = f"{type(error).__name__}: {error}"
        print(f"\nFAIL: {detail}", file=sys.stderr)
        if server is not None and server.poll() is not None:
            print(f"  server exited early with code {server.returncode}", file=sys.stderr)
        if server_log is not None:
            server_log.flush()
            try:
                tail = server_log_path.read_text(errors="replace").strip().splitlines()[-25:]
            except OSError:
                tail = []
            if tail:
                print("  server log (tail):", file=sys.stderr)
                for line in tail:
                    print(f"    {line}", file=sys.stderr)
        return 1
    finally:
        if server is not None:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=10)
        if server_log is not None:
            server_log.close()
        shutil.rmtree(store, ignore_errors=True)
        shutil.rmtree(logdir, ignore_errors=True)

    print("\nPASS: Syntra Python SDK smoke test — all checks green", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
