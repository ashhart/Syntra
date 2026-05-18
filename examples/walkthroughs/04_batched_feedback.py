"""Batched feedback throughput comparison walkthrough.

Demonstrates:
  - Drive 1000 decide requests serially to collect (decisionId, chosen_option)
    pairs.
  - Build a batch payload of 1000 feedback events.
  - Compare wall-clock time of POST /feedback/batch vs 1000 individual
    POST /feedback calls.
  - Print the speedup ratio.

Prerequisites:
  - Syntra running at $SYNTRA_URL (default http://localhost:8787).
  - Admin key in $SYNTRA_ADMIN_KEY or --admin-key.
  - `syntra` CLI on PATH.

Usage:
    python3 04_batched_feedback.py [--syntra-url URL] [--admin-key KEY] [--rounds N]

Apache-2.0.
"""
from __future__ import annotations
import argparse, json, os, random, subprocess, tempfile, time
import urllib.request, urllib.error

CAPSULE_YAML = """\
name: batch-demo
options: [option_a, option_b, option_c]
reward:
  type: continuous
  range: [-1.0, 1.0]
"""


def api(url, method, path, body=None, token=None, raw=None):
    data = raw or (json.dumps(body).encode() if body else None)
    hdrs = {"Content-Type": "application/octet-stream" if raw else "application/json"}
    if token:
        hdrs["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(f"{url}{path}", data=data, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        try:
            return e.code, json.loads(e.read())
        except Exception:
            return e.code, {"error": e.reason}


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


def sec(t): print(f"\n{'='*55}\n  {t}\n{'='*55}")


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"))
    p.add_argument("--admin-key",  default=os.environ.get("SYNTRA_ADMIN_KEY", ""))
    p.add_argument("--tenant", default="demo"); p.add_argument("--job", default="batchtest")
    p.add_argument("--capsule", default="events")
    p.add_argument("--rounds", type=int, default=1000); p.add_argument("--seed", type=int, default=99)
    args = p.parse_args()
    url = args.syntra_url.rstrip("/"); key = args.admin_key
    if not key: raise SystemExit("ERROR: --admin-key or $SYNTRA_ADMIN_KEY required")
    cp = f"/tenants/{args.tenant}/jobs/{args.job}/capsules/{args.capsule}"
    rng = random.Random(args.seed)

    sec("1. Installing capsule")
    lyc = compile_capsule(CAPSULE_YAML)
    status, body = api(url, "POST", f"{cp}/install", token=key, raw=lyc)
    if status != 200: raise SystemExit(f"Install failed {status}: {body}")
    print(json.dumps({"installed": True, "hash": body.get("hash","?")[:16]+"..."}))

    sec(f"2. Driving {args.rounds} decide requests")
    t0 = time.monotonic(); events = []; errors = 0
    for _ in range(args.rounds):
        status, resp = api(url, "POST", f"{cp}/decide",
                           body={"contextKey": f"c{rng.randint(0,19)}"}, token=key)
        if status != 200 or not resp.get("decisions"):
            errors += 1; continue
        events.append({"decisionId": resp["decisionId"],
                        "reward": round(rng.uniform(-1.0, 1.0), 4)})
    print(json.dumps({"decides_ok": len(events), "errors": errors,
                       "elapsed_s": round(time.monotonic()-t0, 3)}))
    if not events: raise SystemExit("No successful decides.")

    sec("3. Batch feedback (POST /feedback/batch)")
    batch_payload = {"events": events}
    t1 = time.monotonic()
    status, br = api(url, "POST", f"{cp}/feedback/batch", body=batch_payload, token=key)
    t_batch = time.monotonic() - t1
    print(json.dumps({"status": status, "okCount": br.get("okCount"),
                       "errCount": br.get("errCount"), "elapsed_s": round(t_batch, 4)}))

    sec("4. Individual feedback (N x POST /feedback)")
    t2 = time.monotonic(); ok = 0; err = 0
    for ev in events:
        s, _ = api(url, "POST", f"{cp}/feedback",
                   body={"decisionId": ev["decisionId"], "reward": ev["reward"]}, token=key)
        if s < 400: ok += 1
        else:       err += 1
    t_serial = time.monotonic() - t2
    print(json.dumps({"ok": ok, "err": err, "elapsed_s": round(t_serial, 3)}))

    sec("5. Speedup summary")
    speedup = t_serial / t_batch if t_batch > 0 else float("inf")
    print(json.dumps({"events": len(events), "batch_s": round(t_batch, 4),
                       "serial_s": round(t_serial, 3),
                       "speedup_ratio": round(speedup, 2),
                       "note": "HTTP round-trip reduction"}))
    print("\nDone.")


if __name__ == "__main__":
    main()
