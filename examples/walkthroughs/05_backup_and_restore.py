"""Backup and restore walkthrough.

Demonstrates:
  - Installing a capsule and driving ~30 feedback events (crossing the Warmup
    threshold so learned weights exist).
  - POST /admin/backup to save the store bundle locally as JSON.
  - POST /admin/restore to replay the bundle back onto the same server.
  - Verifying via GET /report that learned weights match pre-restore values.

Prerequisites:
  - Syntra running at $SYNTRA_URL (default http://localhost:8787).
  - Admin key in $SYNTRA_ADMIN_KEY or --admin-key.
  - `syntra` CLI on PATH.

Usage:
    python3 05_backup_and_restore.py [--syntra-url URL] [--admin-key KEY]

Apache-2.0.
"""
from __future__ import annotations
import argparse, json, os, random, subprocess, tempfile
import urllib.request, urllib.error

CAPSULE_YAML = """\
name: backup-demo
options: [opt_a, opt_b, opt_c]
reward:
  type: continuous
  range: [-1.0, 1.0]
"""


def api_raw(url, method, path, body=None, token=None, raw=None):
    data = raw or (json.dumps(body).encode() if body else None)
    hdrs = {"Content-Type": "application/octet-stream" if raw else "application/json"}
    if token:
        hdrs["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(f"{url}{path}", data=data, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(req) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def api(url, method, path, body=None, token=None, raw=None):
    status, b = api_raw(url, method, path, body=body, token=token, raw=raw)
    try:
        return status, json.loads(b)
    except Exception:
        return status, {"_raw": b.decode(errors="replace")}


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


def weights(report):
    strats = report.get("strategies", [])
    return [o.get("weight", 0.0) for o in strats[0].get("options", [])] if strats else []


def sec(t): print(f"\n{'='*55}\n  {t}\n{'='*55}")


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--syntra-url", default=os.environ.get("SYNTRA_URL", "http://localhost:8787"))
    p.add_argument("--admin-key",  default=os.environ.get("SYNTRA_ADMIN_KEY", ""))
    p.add_argument("--tenant", default="demo"); p.add_argument("--job", default="backup")
    p.add_argument("--capsule", default="weights")
    p.add_argument("--feedback-rounds", type=int, default=35)
    p.add_argument("--backup-file", default="syntra-backup.json")
    p.add_argument("--seed", type=int, default=13)
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

    sec(f"2. Driving {args.feedback_rounds} decide/feedback cycles")
    n_ok = 0
    for _ in range(args.feedback_rounds):
        s, resp = api(url, "POST", f"{cp}/decide",
                      body={"contextKey": f"c{rng.randint(0,4)}"}, token=key)
        if s != 200 or not resp.get("decisions"): continue
        chosen = resp["decisions"][0]["chosen_option"]
        reward = 0.8 if chosen == 0 else -0.2 + rng.uniform(-0.1, 0.1)
        s2, _ = api(url, "POST", f"{cp}/feedback",
                    body={"decisionId": resp["decisionId"], "reward": reward}, token=key)
        if s2 == 200: n_ok += 1
    print(json.dumps({"feedback_ok": n_ok}))

    sec("3. Pre-backup /report weights")
    status, pre = api(url, "GET", f"{cp}/report", token=key)
    if status != 200: raise SystemExit(f"Report failed {status}: {pre}")
    pre_w = weights(pre)
    print(json.dumps({"pre_weights": [round(w, 6) for w in pre_w]}))

    sec("4. Creating backup (POST /admin/backup)")
    status, backup_bytes = api_raw(url, "POST", "/admin/backup", token=key)
    if status != 200: raise SystemExit(f"Backup failed {status}")
    bfile = os.path.abspath(args.backup_file)
    with open(bfile, "wb") as f:
        f.write(backup_bytes)
    print(json.dumps({"backup_file": bfile, "size_bytes": len(backup_bytes)}))

    sec("5. Restoring from backup (POST /admin/restore)")
    status, rr = api(url, "POST", "/admin/restore", token=key, raw=backup_bytes)
    if status != 200: raise SystemExit(f"Restore failed {status}: {rr}")
    print(json.dumps({"ok": rr.get("ok"), "filesRestored": rr.get("filesRestored")}))

    sec("6. Post-restore weights + diff")
    status, post = api(url, "GET", f"{cp}/report", token=key)
    if status != 200: raise SystemExit(f"Post-restore report failed {status}: {post}")
    post_w = weights(post)
    print(json.dumps({"post_weights": [round(w, 6) for w in post_w]}))
    all_match = True
    for i, (pre, post_v) in enumerate(zip(pre_w, post_w)):
        delta = abs(pre - post_v); match = delta < 1e-6
        if not match: all_match = False
        print(json.dumps({"option": i, "pre": round(pre,6), "post": round(post_v,6),
                           "delta": round(delta,9), "match": match}))
    print(json.dumps({"weights_survived_restore": all_match}))
    print("\nDone.")


if __name__ == "__main__":
    main()
