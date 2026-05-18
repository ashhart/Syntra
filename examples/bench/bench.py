#!/usr/bin/env python3
"""
bench.py — Syntra throughput and latency benchmark harness.

Drives concurrent decide/feedback traffic against a running Syntra instance,
measures p50/p95/p99/p999 latencies and per-operation throughput, then reports
JSON to stdout and a human-readable ASCII table to stderr.

Usage:
    python bench.py \\
        --syntra-url http://localhost:8787 \\
        --admin-key dev-key \\
        --tenant acme --job bench --capsule perf \\
        --concurrency 16 --duration-seconds 30 --warmup-seconds 5 \\
        [--ratio 3:1] [--context-type discrete|features]
"""
from __future__ import annotations

import argparse
import bisect
import http.client
import json
import math
import os
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Optional

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

BENCH_CAPSULE_SPEC_DISCRETE = """\
name: bench-capsule
options:
  - option_a
  - option_b
  - option_c
  - option_d
reward:
  type: continuous
  range: [-1.0, 1.0]
"""

BENCH_CAPSULE_SPEC_FEATURES = """\
name: bench-capsule
options:
  - option_a
  - option_b
  - option_c
  - option_d
reward:
  type: continuous
  range: [-1.0, 1.0]
"""

BENCH_LEARNING_CONFIG_DISCRETE = {
    "refusal": {"enabled": False},
    "contextSpec": {"type": "discrete"},
}

BENCH_LEARNING_CONFIG_FEATURES = {
    "refusal": {"enabled": False},
    "contextSpec": {
        "type": "features",
        "features": [
            {"name": "load", "type": {"kind": "continuous", "range": [0.0, 1.0]}},
            {"name": "hour", "type": {"kind": "cyclic", "period": 24.0}},
        ],
    },
}

CONTEXT_KEYS = ["ctx_low", "ctx_mid", "ctx_high", "ctx_peak"]

WATCHDOG_CONSECUTIVE_FAILURE_LIMIT = 100

# ---------------------------------------------------------------------------
# Latency statistics (no numpy — hand-rolled with bisect)
# ---------------------------------------------------------------------------


class LatencyStats:
    """Accumulates latency samples (in nanoseconds) and computes percentiles."""

    def __init__(self) -> None:
        self._samples: list[int] = []
        self._sorted_cache: Optional[list[int]] = None
        self._dirty = False

    def record(self, ns: int) -> None:
        self._samples.append(ns)
        self._dirty = True

    def count(self) -> int:
        return len(self._samples)

    def _sorted(self) -> list[int]:
        if self._dirty or self._sorted_cache is None:
            self._sorted_cache = sorted(self._samples)
            self._dirty = False
        return self._sorted_cache

    def percentile_ms(self, p: float) -> float:
        """Return the p-th percentile (0-100) in milliseconds.

        Uses nearest-rank interpolation. Returns 0.0 on empty input.
        """
        s = self._sorted()
        if not s:
            return 0.0
        if p <= 0.0:
            return s[0] / 1_000_000.0
        if p >= 100.0:
            return s[-1] / 1_000_000.0
        rank = math.ceil(p / 100.0 * len(s)) - 1
        rank = max(0, min(rank, len(s) - 1))
        return s[rank] / 1_000_000.0

    def p50_ms(self) -> float:
        return self.percentile_ms(50.0)

    def p95_ms(self) -> float:
        return self.percentile_ms(95.0)

    def p99_ms(self) -> float:
        return self.percentile_ms(99.0)

    def p999_ms(self) -> float:
        return self.percentile_ms(99.9)

    def merge(self, other: "LatencyStats") -> None:
        """Merge another LatencyStats into this one (for aggregating workers)."""
        self._samples.extend(other._samples)
        self._dirty = True


# ---------------------------------------------------------------------------
# Ratio parsing
# ---------------------------------------------------------------------------


def parse_ratio(ratio_str: str) -> tuple[int, int]:
    """Parse a 'decide:feedback' ratio string into (decide_n, feedback_n).

    Examples:
        parse_ratio("1:1")  -> (1, 1)
        parse_ratio("3:1")  -> (3, 1)

    Raises ValueError on malformed input.
    """
    if not ratio_str or ":" not in ratio_str:
        raise ValueError(f"Invalid ratio '{ratio_str}': expected format 'N:M' (e.g. '3:1')")
    parts = ratio_str.split(":", 1)
    if len(parts) != 2:
        raise ValueError(f"Invalid ratio '{ratio_str}': expected exactly one ':'")
    try:
        d = int(parts[0].strip())
        f = int(parts[1].strip())
    except ValueError:
        raise ValueError(f"Invalid ratio '{ratio_str}': both parts must be integers")
    if d <= 0 or f <= 0:
        raise ValueError(f"Invalid ratio '{ratio_str}': both parts must be positive integers")
    return d, f


# ---------------------------------------------------------------------------
# Per-thread persistent HTTP connection
# ---------------------------------------------------------------------------

_thread_local = threading.local()


def _get_connection(host: str, port: int, use_https: bool) -> http.client.HTTPConnection:
    """Return (or create) a thread-local persistent HTTP connection."""
    key = (host, port, use_https)
    conn = getattr(_thread_local, "conn", None)
    conn_key = getattr(_thread_local, "conn_key", None)
    if conn is None or conn_key != key:
        if use_https:
            import http.client as hc
            conn = hc.HTTPSConnection(host, port, timeout=10)
        else:
            conn = http.client.HTTPConnection(host, port, timeout=10)
        _thread_local.conn = conn
        _thread_local.conn_key = key
    return conn


def _parse_url(url: str):
    """Return (scheme, host, port, path_prefix) from a base URL."""
    parsed = urllib.parse.urlparse(url)
    scheme = parsed.scheme.lower()
    host = parsed.hostname or "localhost"
    if parsed.port:
        port = parsed.port
    else:
        port = 443 if scheme == "https" else 80
    path_prefix = parsed.path.rstrip("/")
    return scheme, host, port, path_prefix


def _do_request(
    host: str,
    port: int,
    use_https: bool,
    method: str,
    path: str,
    body: bytes,
    admin_key: str,
) -> tuple[int, bytes]:
    """Execute one HTTP request using the thread-local persistent connection.

    Returns (status_code, response_body_bytes).
    Reconnects once on BrokenPipeError / ConnectionResetError.
    """
    headers = {
        "Authorization": f"Bearer {admin_key}",
        "Content-Type": "application/json",
        "Connection": "keep-alive",
        "Content-Length": str(len(body)),
    }

    def attempt(conn: http.client.HTTPConnection) -> tuple[int, bytes]:
        conn.request(method, path, body=body, headers=headers)
        resp = conn.getresponse()
        data = resp.read()
        return resp.status, data

    conn = _get_connection(host, port, use_https)
    try:
        return attempt(conn)
    except (BrokenPipeError, ConnectionResetError, http.client.RemoteDisconnected,
            http.client.CannotSendRequest):
        # Reconnect once.
        conn.close()
        conn = _get_connection(host, port, use_https)
        # Force a fresh connection object.
        if use_https:
            import http.client as hc
            conn = hc.HTTPSConnection(host, port, timeout=10)
        else:
            conn = http.client.HTTPConnection(host, port, timeout=10)
        _thread_local.conn = conn
        return attempt(conn)


# ---------------------------------------------------------------------------
# Error counters (shared across workers, protected by a lock)
# ---------------------------------------------------------------------------


@dataclass
class ErrorCounts:
    connection_refused: int = 0
    timeout: int = 0
    http_5xx: int = 0
    http_4xx: int = 0
    watchdog_bailouts: int = 0
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def add_connection_refused(self) -> None:
        with self._lock:
            self.connection_refused += 1

    def add_timeout(self) -> None:
        with self._lock:
            self.timeout += 1

    def add_http_5xx(self) -> None:
        with self._lock:
            self.http_5xx += 1

    def add_http_4xx(self) -> None:
        with self._lock:
            self.http_4xx += 1

    def add_watchdog_bailout(self) -> None:
        with self._lock:
            self.watchdog_bailouts += 1

    def total(self) -> int:
        return (self.connection_refused + self.timeout
                + self.http_5xx + self.http_4xx)


# ---------------------------------------------------------------------------
# Worker result accumulator (per-worker, merged after run)
# ---------------------------------------------------------------------------


@dataclass
class WorkerResult:
    decide_latencies: LatencyStats = field(default_factory=LatencyStats)
    feedback_latencies: LatencyStats = field(default_factory=LatencyStats)


# ---------------------------------------------------------------------------
# Worker thread body
# ---------------------------------------------------------------------------


def _worker_body(
    worker_id: int,
    host: str,
    port: int,
    use_https: bool,
    capsule_path: str,
    admin_key: str,
    context_type: str,
    decide_n: int,
    feedback_n: int,
    stop_event: threading.Event,
    warmup_event: threading.Event,
    errors: ErrorCounts,
    result: WorkerResult,
) -> None:
    """Main loop for a single worker thread.

    Alternates between `decide_n` decide calls and `feedback_n` feedback calls.
    After warmup is complete, records latencies into `result`.
    Bails out if WATCHDOG_CONSECUTIVE_FAILURE_LIMIT consecutive failures occur.
    """
    decide_path = f"{capsule_path}/decide"
    feedback_path = f"{capsule_path}/feedback"

    consecutive_failures = 0
    pending_decision_ids: list[str] = []

    # Rotate through context keys and feature values.
    ctx_idx = worker_id % len(CONTEXT_KEYS)
    load_val = (worker_id % 10) / 10.0
    hour_val = float(worker_id % 24)

    def build_decide_body() -> bytes:
        nonlocal ctx_idx, load_val, hour_val
        if context_type == "features":
            payload = {
                "features": {
                    "load": load_val,
                    "hour": hour_val,
                }
            }
            load_val = (load_val + 0.1) % 1.0
            hour_val = (hour_val + 1.0) % 24.0
        else:
            key = CONTEXT_KEYS[ctx_idx % len(CONTEXT_KEYS)]
            ctx_idx += 1
            payload = {"contextKey": key}
        return json.dumps(payload).encode()

    def build_feedback_body(decision_id: str) -> bytes:
        import random
        reward = round(random.uniform(-0.5, 1.0), 3)
        return json.dumps({"decisionId": decision_id, "reward": reward}).encode()

    def record_error(status: Optional[int], exc: Optional[Exception]) -> None:
        nonlocal consecutive_failures
        consecutive_failures += 1
        if exc is not None:
            exc_str = str(exc).lower()
            if "refused" in exc_str or "connection refused" in exc_str:
                errors.add_connection_refused()
            elif "timed out" in exc_str or "timeout" in exc_str:
                errors.add_timeout()
            else:
                errors.add_connection_refused()
        elif status is not None:
            if status >= 500:
                errors.add_http_5xx()
            elif status >= 400:
                errors.add_http_4xx()

    while not stop_event.is_set():
        # --- decide phase ---
        for _ in range(decide_n):
            if stop_event.is_set():
                return
            body = build_decide_body()
            t0 = time.perf_counter_ns()
            try:
                status, resp_bytes = _do_request(
                    host, port, use_https, "POST", decide_path, body, admin_key
                )
                elapsed_ns = time.perf_counter_ns() - t0
                if status == 200:
                    consecutive_failures = 0
                    if warmup_event.is_set():
                        result.decide_latencies.record(elapsed_ns)
                    # Extract decisionId for feedback.
                    try:
                        resp_json = json.loads(resp_bytes)
                        dec_id = resp_json.get("decisionId")
                        if dec_id:
                            pending_decision_ids.append(dec_id)
                    except (json.JSONDecodeError, AttributeError):
                        pass
                else:
                    record_error(status, None)
            except OSError as exc:
                record_error(None, exc)
            except Exception as exc:
                record_error(None, exc)

            if consecutive_failures >= WATCHDOG_CONSECUTIVE_FAILURE_LIMIT:
                errors.add_watchdog_bailout()
                return

        # --- feedback phase ---
        for _ in range(feedback_n):
            if stop_event.is_set():
                return
            if not pending_decision_ids:
                # Nothing to feed back — skip this cycle.
                break
            decision_id = pending_decision_ids.pop(0)
            body = build_feedback_body(decision_id)
            t0 = time.perf_counter_ns()
            try:
                status, _resp = _do_request(
                    host, port, use_https, "POST", feedback_path, body, admin_key
                )
                elapsed_ns = time.perf_counter_ns() - t0
                if status == 200:
                    consecutive_failures = 0
                    if warmup_event.is_set():
                        result.feedback_latencies.record(elapsed_ns)
                else:
                    record_error(status, None)
            except OSError as exc:
                record_error(None, exc)
            except Exception as exc:
                record_error(None, exc)

            if consecutive_failures >= WATCHDOG_CONSECUTIVE_FAILURE_LIMIT:
                errors.add_watchdog_bailout()
                return


# ---------------------------------------------------------------------------
# Capsule authoring
# ---------------------------------------------------------------------------


def author_and_install_capsule(
    syntra_url: str,
    admin_key: str,
    tenant: str,
    job: str,
    capsule: str,
    context_type: str,
) -> None:
    """Compile and install a benchmark capsule via `syntra author` + HTTP."""
    spec_yaml = (
        BENCH_CAPSULE_SPEC_FEATURES
        if context_type == "features"
        else BENCH_CAPSULE_SPEC_DISCRETE
    )
    learning_config = (
        BENCH_LEARNING_CONFIG_FEATURES
        if context_type == "features"
        else BENCH_LEARNING_CONFIG_DISCRETE
    )

    capsule_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
    base_url = syntra_url.rstrip("/")

    with tempfile.TemporaryDirectory() as tmpdir:
        spec_path = os.path.join(tmpdir, "bench.yaml")
        out_dir = os.path.join(tmpdir, "out")
        with open(spec_path, "w") as fh:
            fh.write(spec_yaml)

        try:
            subprocess.run(
                ["syntra", "author", spec_path, "--out-dir", out_dir],
                check=True,
                capture_output=True,
            )
        except FileNotFoundError:
            print(
                "ERROR: `syntra` binary not on PATH. Install Syntra and ensure it is on PATH.",
                file=sys.stderr,
            )
            sys.exit(1)
        except subprocess.CalledProcessError as exc:
            print(f"ERROR: `syntra author` failed: {exc.stderr.decode()}", file=sys.stderr)
            sys.exit(1)

        lyc_path = os.path.join(out_dir, "program.lyc")
        with open(lyc_path, "rb") as fh:
            lyc_bytes = fh.read()

    headers_bin = {
        "Authorization": f"Bearer {admin_key}",
        "Content-Type": "application/octet-stream",
    }
    req = urllib.request.Request(
        f"{base_url}{capsule_path}/install",
        data=lyc_bytes,
        method="POST",
        headers=headers_bin,
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            resp.read()
    except urllib.error.HTTPError as exc:
        print(f"ERROR: capsule install failed: {exc.code} {exc.reason}", file=sys.stderr)
        sys.exit(1)
    except OSError as exc:
        print(f"ERROR: cannot reach Syntra at {base_url}: {exc}", file=sys.stderr)
        sys.exit(1)

    # PUT learning config.
    learning_bytes = json.dumps(learning_config).encode()
    headers_json = {
        "Authorization": f"Bearer {admin_key}",
        "Content-Type": "application/json",
    }
    req2 = urllib.request.Request(
        f"{base_url}{capsule_path}/learning",
        data=learning_bytes,
        method="PUT",
        headers=headers_json,
    )
    try:
        with urllib.request.urlopen(req2, timeout=15) as resp:
            resp.read()
    except urllib.error.HTTPError as exc:
        print(
            f"WARNING: learning config PUT failed: {exc.code} {exc.reason}",
            file=sys.stderr,
        )
    except OSError as exc:
        print(f"WARNING: learning config PUT failed: {exc}", file=sys.stderr)

    print(
        f"[bench] capsule installed at {capsule_path} (context_type={context_type})",
        file=sys.stderr,
    )


# ---------------------------------------------------------------------------
# ASCII table reporter
# ---------------------------------------------------------------------------


def ascii_table(rows: list[tuple], headers: list[str]) -> str:
    """Format rows as a fixed-width ASCII table. Returns the table string."""
    col_widths = [len(h) for h in headers]
    str_rows = []
    for row in rows:
        str_row = [str(cell) for cell in row]
        str_rows.append(str_row)
        for i, cell in enumerate(str_row):
            if i < len(col_widths):
                col_widths[i] = max(col_widths[i], len(cell))

    sep = "+" + "+".join("-" * (w + 2) for w in col_widths) + "+"
    header_line = "|" + "|".join(
        f" {h:<{col_widths[i]}} " for i, h in enumerate(headers)
    ) + "|"
    lines = [sep, header_line, sep]
    for str_row in str_rows:
        line = "|" + "|".join(
            f" {cell:<{col_widths[i]}} " for i, cell in enumerate(str_row)
        ) + "|"
        lines.append(line)
    lines.append(sep)
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Main run loop
# ---------------------------------------------------------------------------


def run_bench(
    syntra_url: str,
    admin_key: str,
    tenant: str,
    job: str,
    capsule: str,
    concurrency: int,
    duration_seconds: float,
    warmup_seconds: float,
    decide_n: int,
    feedback_n: int,
    context_type: str,
    ratio_str: str,
    author_capsule: bool,
) -> dict:
    """Run the benchmark and return the result dict."""

    if author_capsule:
        author_and_install_capsule(
            syntra_url, admin_key, tenant, job, capsule, context_type
        )

    scheme, host, port, _prefix = _parse_url(syntra_url)
    use_https = scheme == "https"
    capsule_path = f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}"

    stop_event = threading.Event()
    warmup_event = threading.Event()
    errors = ErrorCounts()

    worker_results = [WorkerResult() for _ in range(concurrency)]

    print(
        f"[bench] starting {concurrency} workers, "
        f"warmup={warmup_seconds}s, duration={duration_seconds}s, "
        f"ratio={decide_n}:{feedback_n}, context_type={context_type}",
        file=sys.stderr,
    )

    futures = []
    with ThreadPoolExecutor(max_workers=concurrency, thread_name_prefix="bench") as pool:
        for i in range(concurrency):
            f = pool.submit(
                _worker_body,
                worker_id=i,
                host=host,
                port=port,
                use_https=use_https,
                capsule_path=capsule_path,
                admin_key=admin_key,
                context_type=context_type,
                decide_n=decide_n,
                feedback_n=feedback_n,
                stop_event=stop_event,
                warmup_event=warmup_event,
                errors=errors,
                result=worker_results[i],
            )
            futures.append(f)

        # Warmup phase.
        print(f"[bench] warmup {warmup_seconds}s ...", file=sys.stderr)
        time.sleep(warmup_seconds)
        warmup_event.set()

        # Measurement phase.
        print(f"[bench] measuring for {duration_seconds}s ...", file=sys.stderr)
        measure_start = time.perf_counter()
        time.sleep(duration_seconds)
        measure_end = time.perf_counter()
        actual_duration = measure_end - measure_start

        # Stop all workers.
        stop_event.set()

    # Aggregate results.
    decide_stats = LatencyStats()
    feedback_stats = LatencyStats()
    for wr in worker_results:
        decide_stats.merge(wr.decide_latencies)
        feedback_stats.merge(wr.feedback_latencies)

    decide_count = decide_stats.count()
    feedback_count = feedback_stats.count()
    decide_rps = decide_count / actual_duration if actual_duration > 0 else 0.0
    feedback_rps = feedback_count / actual_duration if actual_duration > 0 else 0.0
    total_rps = (decide_count + feedback_count) / actual_duration if actual_duration > 0 else 0.0
    p99_decide = decide_stats.p99_ms()
    p99_feedback = feedback_stats.p99_ms()
    total_errors = errors.total()

    result = {
        "config": {
            "concurrency": concurrency,
            "duration_s": duration_seconds,
            "warmup_s": warmup_seconds,
            "ratio": ratio_str,
            "context_type": context_type,
            "syntra_url": syntra_url,
            "capsule_path": capsule_path,
        },
        "ops": {
            "decide": {
                "count": decide_count,
                "throughput_rps": round(decide_rps, 1),
                "p50_ms": round(decide_stats.p50_ms(), 2),
                "p95_ms": round(decide_stats.p95_ms(), 2),
                "p99_ms": round(p99_decide, 2),
                "p999_ms": round(decide_stats.p999_ms(), 2),
                "errors": errors.http_4xx + errors.http_5xx,
            },
            "feedback": {
                "count": feedback_count,
                "throughput_rps": round(feedback_rps, 1),
                "p50_ms": round(feedback_stats.p50_ms(), 2),
                "p95_ms": round(feedback_stats.p95_ms(), 2),
                "p99_ms": round(p99_feedback, 2),
                "p999_ms": round(feedback_stats.p999_ms(), 2),
                "errors": 0,
            },
        },
        "errors": {
            "connection_refused": errors.connection_refused,
            "timeout": errors.timeout,
            "http_5xx": errors.http_5xx,
            "http_4xx": errors.http_4xx,
            "watchdog_bailouts": errors.watchdog_bailouts,
        },
    }

    # Human-readable stderr report.
    _print_report(result, total_rps, max(p99_decide, p99_feedback), total_errors, concurrency, duration_seconds)

    return result


def _print_report(
    result: dict,
    total_rps: float,
    p99_combined: float,
    total_errors: int,
    concurrency: int,
    duration_s: float,
) -> None:
    """Print a human-readable ASCII table + summary line to stderr."""
    ops = result["ops"]

    rows = [
        (
            "decide",
            ops["decide"]["count"],
            f"{ops['decide']['throughput_rps']:.1f}",
            f"{ops['decide']['p50_ms']:.2f}",
            f"{ops['decide']['p95_ms']:.2f}",
            f"{ops['decide']['p99_ms']:.2f}",
            f"{ops['decide']['p999_ms']:.2f}",
            ops["decide"]["errors"],
        ),
        (
            "feedback",
            ops["feedback"]["count"],
            f"{ops['feedback']['throughput_rps']:.1f}",
            f"{ops['feedback']['p50_ms']:.2f}",
            f"{ops['feedback']['p95_ms']:.2f}",
            f"{ops['feedback']['p99_ms']:.2f}",
            f"{ops['feedback']['p999_ms']:.2f}",
            ops["feedback"]["errors"],
        ),
    ]
    headers = ["op", "count", "rps", "p50_ms", "p95_ms", "p99_ms", "p999_ms", "errors"]

    table = ascii_table(rows, headers)
    print("\n" + table, file=sys.stderr)

    errs = result["errors"]
    error_detail = (
        f"conn_refused={errs['connection_refused']} "
        f"timeout={errs['timeout']} "
        f"5xx={errs['http_5xx']} "
        f"4xx={errs['http_4xx']} "
        f"watchdog_bailouts={errs['watchdog_bailouts']}"
    )
    summary = (
        f"[bench] {duration_s:.0f}s @ N={concurrency}: "
        f"{total_rps:.0f} ops/s, p99 {p99_combined:.1f} ms, "
        f"{total_errors} errors  [{error_detail}]"
    )
    print(summary, file=sys.stderr)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_arg_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="Syntra throughput and latency benchmark harness.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    p.add_argument("--syntra-url", default="http://localhost:8787",
                   help="Base URL of the Syntra instance.")
    p.add_argument("--admin-key", default=os.environ.get("SYNTRA_ADMIN_KEY", "dev-key"),
                   help="Syntra admin key (or set $SYNTRA_ADMIN_KEY).")
    p.add_argument("--tenant", default="bench",
                   help="Tenant identifier.")
    p.add_argument("--job", default="perf",
                   help="Job identifier.")
    p.add_argument("--capsule", default="harness",
                   help="Capsule identifier.")
    p.add_argument("--concurrency", type=int, default=8,
                   help="Number of concurrent worker threads.")
    p.add_argument("--duration-seconds", type=float, default=30.0,
                   help="Measurement window duration in seconds.")
    p.add_argument("--warmup-seconds", type=float, default=5.0,
                   help="Warmup period in seconds (results discarded).")
    p.add_argument("--ratio", default="1:1",
                   help="decide:feedback ratio, e.g. '3:1' means 3 decides then 1 feedback per cycle.")
    p.add_argument("--context-type", choices=["discrete", "features"], default="discrete",
                   help="Context type: 'discrete' uses contextKey strings, "
                        "'features' uses a typed feature vector (enables LinUCB).")
    p.add_argument("--no-author", action="store_true",
                   help="Skip capsule authoring and installation (capsule must already exist).")
    return p


def main() -> int:
    parser = build_arg_parser()
    args = parser.parse_args()

    try:
        decide_n, feedback_n = parse_ratio(args.ratio)
    except ValueError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    result = run_bench(
        syntra_url=args.syntra_url,
        admin_key=args.admin_key,
        tenant=args.tenant,
        job=args.job,
        capsule=args.capsule,
        concurrency=args.concurrency,
        duration_seconds=args.duration_seconds,
        warmup_seconds=args.warmup_seconds,
        decide_n=decide_n,
        feedback_n=feedback_n,
        context_type=args.context_type,
        ratio_str=args.ratio,
        author_capsule=not args.no_author,
    )

    # JSON result to stdout.
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
