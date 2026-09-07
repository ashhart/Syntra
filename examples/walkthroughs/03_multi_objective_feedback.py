"""Multi-objective (components-based) reward walkthrough.

Demonstrates:
  - Installing a 3-option capsule with a reward_spec declaring three
    components: quality, latency_ms, cost_usd.
  - Sending 60 decide/feedback cycles using per-component rewards.
  - Inspecting per-(option, component) Q estimates from /memory.
  - Showing that the bandit shifts weight toward options with high quality
    and low latency.

Prerequisites:
  - Syntra running at $SYNTRA_URL (default http://localhost:8787).
  - Admin key in $SYNTRA_ADMIN_KEY or --admin-key.
  - `syntra` CLI on PATH.

Usage:
    python3 03_multi_objective_feedback.py [--syntra-url URL] [--admin-key KEY]

Apache-2.0.
"""
from __future__ import annotations
import argparse, json, os, random, subprocess, tempfile
import urllib.request, urllib.error

CAPSULE_YAML = """\
name: multi-obj-demo
options: [fast, balanced, accurate]
reward:
  type: continuous
  range: [-1.0, 1.0]
"""
REWARD_SPEC = {"components": [
    {"name": "quality",    "weight": 0.5,  "normalize": "minmax", "range": [0.0, 1.0]},
    {"name": "latency_ms", "weight": -0.3, "normalize": "minmax", "range": [0.0, 2000.0]},
    {"name": "cost_usd",   "weight": -0.2, "normalize": "budget", "budget": 0.05},
]}
OPTION_PROFILES = {
    0: {"quality": 0.55, "latency_ms": 120.0,  "cost_usd": 0.002},
    1: {"quality": 0.70, "latency_ms": 400.0,  "cost_usd": 0.010},
    2: {"quality": 0.90, "latency_ms": 1200.0, "cost_usd": 0.040},
}
NAMES = ["fast", "balanced", "accurate"]


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
        sp = os.path.join(d, "s.yaml")
        od = os.path.join(d, "out")
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
    p.add_argument("--tenant", default="demo"); p.add_argument("--job", default="multiobj")
    p.add_argument("--capsule", default="routing")
    p.add_argument("--rounds", type=int, default=60); p.add_argument("--seed", type=int, default=7)
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

    sec("2. Attaching multi-component reward_spec")
    status, _ = api(url, "PUT", f"{cp}/reward_spec", body=REWARD_SPEC, token=key)
    if status != 200: raise SystemExit(f"PUT /reward_spec failed {status}")
    print(json.dumps({"reward_spec_attached": True}))

    sec(f"3. Running {args.rounds} decide/feedback cycles")
    for i in range(args.rounds):
        status, resp = api(url, "POST", f"{cp}/decide",
                           body={"contextKey": f"u{rng.randint(0,4)}"}, token=key)
        if status != 200 or not resp.get("decisions"): continue
        chosen = resp["decisions"][0]["chosen_option"]
        profile = OPTION_PROFILES.get(chosen, OPTION_PROFILES[1])
        noise = {k: max(0.0, v + rng.gauss(0, v*0.05)) for k, v in profile.items()}
        noise["quality"] = min(1.0, noise["quality"])
        api(url, "POST", f"{cp}/feedback",
            body={"decisionId": resp["decisionId"], "components": noise}, token=key)
        if i < 3 or i >= args.rounds - 2:
            print(json.dumps({"round": i, "chosen": NAMES[chosen],
                               "comp": {k: round(v, 4) for k, v in noise.items()}}))

    sec("4. Per-(option, component) Q estimates from /memory")
    status, mem = api(url, "GET", f"{cp}/memory", token=key)
    if status != 200: print(f"Cannot load memory: {status}"); return
    for nid, strategy in mem.get("strategies", {}).items():
        agg_r: dict = {}; agg_c: dict = {}
        for _ctx, bucket in strategy.get("contexts", {}).items():
            for oi, stat in enumerate(bucket.get("stats", [])):
                for comp, val in stat.get("objectiveRewards", {}).items():
                    agg_r.setdefault(comp, [0.0]*3)[oi] += val
                for comp, cnt in stat.get("objectiveCounts", {}).items():
                    agg_c.setdefault(comp, [0]*3)[oi] += cnt
        print(f"  node_id={nid}")
        for comp in ["quality", "latency_ms", "cost_usd"]:
            r = agg_r.get(comp, [None]*3); c = agg_c.get(comp, [0]*3)
            q = [round(r[i]/c[i], 4) if c[i] > 0 else None for i in range(3)]
            print(json.dumps({"component": comp, "q_by_option": dict(zip(NAMES, q))}))

    sec("5. Final option weights from /report")
    status, report = api(url, "GET", f"{cp}/report", token=key)
    if status == 200:
        for strat in report.get("strategies", []):
            for opt in strat.get("options", []):
                print(json.dumps({"option": NAMES[opt["option"]],
                                   "tries": opt.get("tries", 0),
                                   "weight": round(opt.get("weight", 0), 4)}))
    print("\nDone.")


if __name__ == "__main__":
    main()
