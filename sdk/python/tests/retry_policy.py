#!/usr/bin/env python3
"""Retry-policy and error-mapping regression test for the Syntra SDK.

`smoke.py` covers the happy path against a real appliance; it cannot
reach the branch that matters most here — which calls may be replayed
after a failure. `decide` and `feedback` append to the decision log and
mutate learned weights, so replaying one double-counts a reward; reads
and installs must be replayed, or a transient 5xx becomes a caller error.
Those rules are invisible to an end-to-end run against a healthy server,
so this test drives the client against a scripted local HTTP server that
counts attempts per route.

Plain assertions, no test runner:

    python3 sdk/python/tests/retry_policy.py
"""

from __future__ import annotations

import json
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from syntra_client import (  # noqa: E402
    RateLimitedError,
    ServerError,
    SyntraClient,
    TransportError,
)

CAPSULE_PATH = "/v1/tenants/a/jobs/j/capsules/c"
LIMITED_ROUTE = "/v1/tenants/a/jobs/j/capsules/limited/report"
LIMITED_PATH = "GET " + LIMITED_ROUTE
ATTEMPTS: dict[str, int] = {}


class ScriptedServer(BaseHTTPRequestHandler):
    """Returns 5xx a fixed number of times per route, then succeeds."""

    failures = {"GET " + CAPSULE_PATH + "/report": 2,
                "POST " + CAPSULE_PATH + "/install": 1,
                "POST " + CAPSULE_PATH + "/decide": 99,
                "POST " + CAPSULE_PATH + "/feedback": 99}

    def log_message(self, *args) -> None:  # pragma: no cover
        pass

    def _respond(self, status: int, body: bytes, headers=()) -> None:
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def handle_request(self) -> None:
        key = f"{self.command} {self.path.split('?')[0]}"
        ATTEMPTS[key] = ATTEMPTS.get(key, 0) + 1
        if self.path.split("?")[0] == LIMITED_ROUTE:
            self._respond(
                429,
                json.dumps({"error": "rate limit exceeded",
                            "retryAfterSeconds": 7}).encode(),
                headers=[("Retry-After", "7")],
            )
            return
        if self.failures.get(key, 0) >= ATTEMPTS[key]:
            self._respond(500, json.dumps({"error": "boom"}).encode())
            return
        self._respond(200, json.dumps({"ok": True, "hash": "a" * 64}).encode())

    do_GET = do_POST = do_DELETE = handle_request


def check(condition: bool, description: str) -> None:
    if not condition:
        raise AssertionError(description)
    print(f"  ✓ {description}", flush=True)


def attempts(key: str) -> int:
    return ATTEMPTS.get(key, 0)


def run(client: SyntraClient) -> None:
    # Reads are replayed: two injected 500s, then success on attempt 3.
    started = time.monotonic()
    report = client.report("a", "j", "c")
    check(report == {"ok": True, "hash": "a" * 64}, "report eventually succeeds")
    check(attempts("GET " + CAPSULE_PATH + "/report") == 3,
          "GET retried until retries+1 attempts (retries=2 -> 3)")
    check(time.monotonic() - started >= 0.6,
          "retries are spaced by exponential backoff (>= 0.2 + 0.4 s)")

    # Installs are replayed: reinstalling the same bytes is a no-op write.
    check(client.install_capsule("a", "j", "c", b"LYCN-payload") == "a" * 64,
          "install eventually succeeds")
    check(attempts("POST " + CAPSULE_PATH + "/install") == 2,
          "install retried once on 5xx")

    # decide/feedback are never replayed, however the failure looks.
    for name, call, key in (
        ("decide",
         lambda: client.decide("a", "j", "c", {"contextKey": "k"}),
         "POST " + CAPSULE_PATH + "/decide"),
        ("feedback",
         lambda: client.feedback("a", "j", "c", "dec_1", 0.5),
         "POST " + CAPSULE_PATH + "/feedback"),
    ):
        try:
            call()
            raise AssertionError(f"{name} should have raised ServerError")
        except ServerError:
            pass
        check(attempts(key) == 1,
              f"{name} is never retried — exactly one attempt")

    # 429 carries the server's back-off hint and is not replayed either.
    try:
        client.report("a", "j", "limited")
        raise AssertionError("429 should have raised RateLimitedError")
    except RateLimitedError as error:
        check(error.retry_after == 7.0,
              "RateLimitedError.retry_after reads Retry-After (7.0)")
        check(error.body == {"error": "rate limit exceeded",
                             "retryAfterSeconds": 7},
              "RateLimitedError keeps the server body for callers")
    check(attempts(LIMITED_PATH) == 1, "429 is not retried")

    # An unreachable appliance fails fast, and not one decide is replayed.
    dead = SyntraClient("http://127.0.0.1:1", token="t", timeout=1.0, retries=2)
    try:
        dead.decide("a", "j", "c", {"contextKey": "k"})
        raise AssertionError("unreachable host should raise TransportError")
    except TransportError as error:
        check("decide" in str(error) and "/127.0.0.1" not in str(error),
              "TransportError names the call, not a reconnect-worthy URL")
    try:
        dead.report("a", "j", "c")
        raise AssertionError("unreachable host should raise TransportError")
    except TransportError:
        pass


def main() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    server = ThreadingHTTPServer(("127.0.0.1", port), ScriptedServer)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    print(f"Syntra Python SDK retry-policy test — scripted server :{port}",
          flush=True)
    try:
        run(SyntraClient(f"http://127.0.0.1:{port}", token="t",
                         timeout=2.0, retries=2))
    except BaseException as error:  # noqa: BLE001
        print(f"\nFAIL: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    finally:
        server.shutdown()
        server.server_close()
    print("\nPASS: retry policy and error mapping — all checks green", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
