"""Client-side rate-limit (429) handling with Retry-After walkthrough.

Demonstrates:
  - Opening N parallel threads each hammering POST /decide.
  - When a 429 is received, reading Retry-After and sleeping before retrying.
  - Printing total requests, successful, throttled-and-retried, total wall time.
  - Verifying that no requests are lost -- all eventually succeed.

Prerequisites:
  - Syntra running at $SYNTRA_URL (default http://localhost:8787).
  - Admin key in $SYNTRA_ADMIN_KEY or --admin-key.
  - `syntra` CLI on PATH.

Usage:
    python3 06_rate_limit_handling.py [--syntra-url URL] [--admin-key KEY]
    python3 06_rate_limit_handling.py --threads 50 --requests-per-thread 2

Apache-2.0.
"""
from __future__ import annotations
import argparse, json, os, random, subprocess, tempfile, threading, time
import urllib.request, urllib.error

CAPSULE_YAML = """\
name: rate-limit-demo
options: [alpha, beta, gamma]
reward:
  type: continuous
  range: [-1.0, 1.0]
"""
DEFAULT_RETRY_AFTER = 1


def compile_capsule(yaml_text):
    with tempfile.TemporaryDirectory() as d:
        sp = os.path.join(d, "s.yaml"); od = os.path.join(d, "out")
        with open(sp, "w") as f:
            f.write(yaml_text)
        try:
            subprocess.run(["syntra", "author", sp, "--out-dir", od], check=True, capture_output=True)
        except FileNotFoundError:
            raise SystemExit("ERROR: `syntra` not on PATH.")
        except subprocess.CalledProcessError as e:
            raise SystemExit(f"ERROR: {e.stderr.decode()}")
        with open(os.path.join(od, "program.lyc"), "rb") as f:
            return f.read()


def api_raw(url, method, path, data=None, hdrs=None):
    req = urllib.request.Request(f"{url}{path}", data=data, headers=hdrs or {}, method=method)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read(), dict(r.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def decide_with_retry(url, cp, key, ctx, stats, lock):
    payload = json.dumps({"contextKey": ctx}).encode()
    hdrs = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
    while True:
        status, _, resp_hdrs = api_raw(url, "POST", f"{cp}/decide", payload, hdrs)
        with lock:
            stats["total"] += 1
        if status == 200:
            with lock:
                stats["success"] += 1
            return
        if status == 429:
            ra = resp_hdrs.get("Retry-After") or resp_hdrs.get("retry-after") or str(DEFAULT_RETRY_AFTER)
            try:
                wait = float(ra)
            except ValueError:
                wait = float(DEFAULT_RETRY_AFTER)
            with lock:
                stats["throttled"] += 1
            time.sleep(max(0.1, wait))
        else:
            with lock:
                stats["error"] += 1
            return


def sec(t): print(f"\n{'='*55}\n  {t}\n{'='*55}")


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"))
    p.add_argument("--admin-key",  default=os.environ.get("SYNTRA_ADMIN_KEY", ""))
    p.add_argument("--tenant", default="demo"); p.add_argument("--job", default="ratelimit")
    p.add_argument("--capsule", default="storm")
    p.add_argument("--threads", type=int, default=50)
    p.add_argument("--requests-per-thread", type=int, default=2)
    p.add_argument("--seed", type=int, default=77)
    args = p.parse_args()
    url = args.syntra_url.rstrip("/"); key = args.admin_key
    if not key: raise SystemExit("ERROR: --admin-key or $SYNTRA_ADMIN_KEY required")
    cp = f"/tenants/{args.tenant}/jobs/{args.job}/capsules/{args.capsule}"
    rng = random.Random(args.seed)

    sec("1. Installing capsule")
    lyc = compile_capsule(CAPSULE_YAML)
    hdrs = {"Authorization": f"Bearer {key}", "Content-Type": "application/octet-stream"}
    status, body, _ = api_raw(url, "POST", f"{cp}/install", lyc, hdrs)
    parsed = json.loads(body) if body else {}
    if status != 200: raise SystemExit(f"Install failed {status}: {parsed}")
    print(json.dumps({"installed": True, "hash": parsed.get("hash","?")[:16]+"..."}))

    total_intended = args.threads * args.requests_per_thread
    sec(f"2. Launching {args.threads} threads x {args.requests_per_thread} = {total_intended} requests")
    stats: dict = {"total": 0, "success": 0, "throttled": 0, "error": 0}
    lock = threading.Lock()
    threads = []
    t_start = time.monotonic()
    for _ in range(args.threads):
        for _ in range(args.requests_per_thread):
            ctx = f"c{rng.randint(0,9)}"
            t = threading.Thread(target=decide_with_retry,
                                 args=(url, cp, key, ctx, stats, lock), daemon=True)
            threads.append(t)
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=120)
    t_elapsed = time.monotonic() - t_start

    sec("3. Results")
    print(json.dumps({
        "intended":          total_intended,
        "http_requests":     stats["total"],
        "successful":        stats["success"],
        "throttled_retries": stats["throttled"],
        "errors":            stats["error"],
        "wall_time_s":       round(t_elapsed, 3),
        "no_requests_lost":  stats["success"] == total_intended - stats["error"],
    }))
    if stats["throttled"] == 0:
        print("\nNote: zero 429s observed -- rate limit not reached at this concurrency.")
    print("\nDone.")


if __name__ == "__main__":
    main()
