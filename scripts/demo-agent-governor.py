#!/usr/bin/env python3
"""Syntra: Agent Governor — a learned policy plane between AI agents and
their tools.

A fleet of 6 agents across 2 tenants proposes tool calls. Every proposal is
a decide on the tenant's `governor` capsule, which chooses one guardrail
action (allow / cap / approve / block) and learns from delayed rewards
which one each context deserves. A structural budget rail sits UNDER the
learner: the capsule's feature program excludes every action except
`block` when the gateway's spend signal says the budget is spent, so those
decisions are block with probability 1 whatever the model has learned.

Run:  python3 scripts/demo-agent-governor.py
Env:  SYNTRA_BIN / LYCAN_BIN pick the binaries (default: the release build,
      built if missing). KEEP_DEMO_OUTPUT=1 keeps the temp store.
"""
import hashlib
import http.client
import json
import os
import random
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def binary(name):
    """$<NAME>_BIN, else the release build (built on first use)."""
    explicit = os.environ.get(f"{name.upper()}_BIN")
    if explicit:
        return explicit
    target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(ROOT, "target")
    path = os.path.join(target, "release", name)
    if not os.path.exists(path):
        print(f"  {name} release binary missing; building...")
        subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)
    return path


SYNTRA = binary("syntra")
LYCAN = binary("lycan")

BASE = tempfile.mkdtemp(prefix="syntra-agent-governor.")
STORE = os.path.join(BASE, "store")
KEY = "governor-demo-key"
with socket.socket() as _s:
    _s.bind(("127.0.0.1", 0))
    PORT = _s.getsockname()[1]
ADDR = f"127.0.0.1:{PORT}"
SEED = 20260908
rng = random.Random(SEED)
ACTIONS = ["allow", "cap", "approve", "block"]
EXECUTED = {"allow", "cap"}  # approve and block hold the call at the gate

PASS, FAIL = 0, 0


def check(label, cond, detail=""):
    global PASS, FAIL
    mark = "PASS" if cond else "FAIL"
    PASS, FAIL = PASS + bool(cond), FAIL + (not cond)
    print(f"  {mark}: {label}" + (f"  [{detail}]" if detail else ""))
    return cond


# ── capsule ──────────────────────────────────────────────────────────────
# The spec declares the four guardrail actions and a fixed seed, so the
# whole run is reproducible. Rewards range over [-2, 1].
SPEC = {
    "actions": [{"id": a} for a in ACTIONS],
    "reward": {"range": [-2.0, 1.0]},
    "seed": SEED,
}
# The feature program: (a) the budget rail, (b) a derived context feature.
#  (a) `budgetRemaining` and `estCost` arrive in the decide context
#      (gateway-side spend accounting; the capsule keeps no budget state).
#      When the call would spend the rest of the budget, every action but
#      `block` is excluded before sampling, so the learner cannot trade a
#      spent budget for an action however its weights look.
#  (b) `features.ctx` = agent|toolClass|isProd lets the linear model learn
#      a separate preference per traffic context.
PROGRAM = r"""
($ agent (!cap "runtime.inputGet" "agentId"))
($ cls (!cap "runtime.inputGet" "toolClass"))
($ prod (!cap "runtime.inputGet" "isProd"))
($ budget (!cap "runtime.inputGet" "budgetRemaining"))
($ cost (!cap "runtime.inputGet" "estCost"))
($ over (? (&& (!= budget null) (!= cost null)) (<= budget cost) false))
(!cap "runtime.publish" "exclude.allow" over)
(!cap "runtime.publish" "exclude.cap" over)
(!cap "runtime.publish" "exclude.approve" over)
(!cap "runtime.publish" "features.ctx" (+ (+ (+ (+ agent "|") cls) "|") prod))
(!cap "runtime.publish" "reason" (? over "budget rail: the call would exhaust the budget" "learned policy"))
"""
SRC = os.path.join(BASE, "governor.lycs")
with open(SRC, "w") as f:
    f.write(PROGRAM)
subprocess.run([LYCAN, "compile", SRC], check=True, capture_output=True)
LYC = SRC[:-len(".lycs")] + ".lyc"

# ── server lifecycle ─────────────────────────────────────────────────────
proc = None


def start_server():
    global proc
    log = open(os.path.join(BASE, "server.log"), "ab")
    proc = subprocess.Popen([SYNTRA, "serve", "--addr", ADDR, "--store", STORE,
                             "--admin-key", KEY], stdout=log, stderr=log)
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            c = http.client.HTTPConnection("127.0.0.1", PORT, timeout=1)
            c.request("GET", "/health")
            if c.getresponse().status == 200:
                c.close()
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("server did not become ready within 10s")


def stop_server():
    """SIGTERM: the server drains, flushes the log and snapshots models."""
    proc.terminate()
    proc.wait(timeout=30)


start_server()
conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)


def api(method, path, body=None, token=None, con=None):
    """One keep-alive HTTP call; returns (status, parsed-json-or-bytes)."""
    con = con or conn
    hdrs = {"Content-Type": "application/json",
            "Authorization": f"Bearer {token or KEY}"}
    if isinstance(body, (bytes, bytearray)):
        hdrs["Content-Type"] = "application/octet-stream"
    con.request(method, path, body=None if body is None else
                (bytes(body) if isinstance(body, (bytes, bytearray)) else json.dumps(body)),
                headers=hdrs)
    r = con.getresponse()
    data = r.read()
    try:
        return r.status, json.loads(data)
    except ValueError:
        return r.status, data


def capsule(tenant):
    return f"/v1/tenants/{tenant}/jobs/fleet/capsules/governor"


def decide(tenant, ctx, con=None):
    return api("POST", f"{capsule(tenant)}/decide", {"context": ctx}, con=con)


def reward(tenant, body, con=None):
    return api("POST", f"{capsule(tenant)}/reward", body, con=con)


# ── fleet model ──────────────────────────────────────────────────────────
TENANTS = {"alpha": ["scout", "coder", "rogue", "batch-exfil"],
           "beta": ["ops", "new-hire"]}
ROLE = {"scout": "read-only recon", "coder": "build & ship (mutate/exec)",
        "ops": "production operations (deploys)", "rogue": "compromised agent: reckless exec/deploy",
        "batch-exfil": "bulk high-blast network egress", "new-hire": "clumsy junior agent"}
PF = {  # true failure probability if the action is EXECUTED (ground-truth model)
    "scout":       {"read": .02, "mutate": .10, "exec": .10, "network": .05},
    "coder":       {"read": .02, "mutate": .10, "exec": .12, "network": .10},
    "ops":         {"read": .03, "mutate": .12, "exec": .15, "network": .10},
    "new-hire":    {"read": .05, "mutate": .25, "exec": .35, "network": .20},
    "rogue":       {"read": .40, "mutate": .60, "exec": .75, "network": .65},
    "batch-exfil": {"read": .70, "mutate": .80, "exec": .85, "network": .90}}
MIX = {  # toolClass traffic mix per agent
    "scout":       [("read", .70), ("network", .20), ("mutate", .10)],
    "coder":       [("mutate", .50), ("exec", .30), ("read", .15), ("network", .05)],
    "ops":         [("exec", .45), ("mutate", .30), ("read", .20), ("network", .05)],
    "new-hire":    [("read", .55), ("mutate", .25), ("exec", .15), ("network", .05)],
    "rogue":       [("exec", .70), ("network", .20), ("mutate", .10)],
    "batch-exfil": [("network", .85), ("read", .10), ("mutate", .05)]}
TOOL = {"read": ["fs.read", "db.select"], "mutate": ["git.push", "db.write"],
        "exec": ["deploy", "shell.exec", "vm.provision"], "network": ["http.post", "http.get"]}
BLAST = {  # blastRadius range per (agent, class)
    "scout": (.5, 3.0), "coder": (2.0, 6.0), "new-hire": (2.0, 6.0),
    "ops-read": (.5, 2.0), "ops-mutate": (3.0, 6.0), "ops-exec": (4.0, 8.0), "ops-network": (1.0, 4.0),
    "rogue-exec": (7.0, 10.0), "rogue-network": (6.0, 9.0), "rogue-mutate": (5.0, 8.0),
    "batch-exfil-network": (8.0, 10.0), "batch-exfil-read": (6.0, 9.0), "batch-exfil-mutate": (7.0, 9.0)}


def blast_range(a, cls):
    return BLAST.get(f"{a}-{cls}", BLAST.get(a, (.5, 5.0)))


COST = {"read": (2.0, 8.0), "mutate": (5.0, 20.0), "exec": (8.0, 28.0), "network": (4.0, 16.0)}
PROD_P = {"scout": .20, "coder": .25, "ops": .60, "new-hire": .10, "rogue": .80, "batch-exfil": .50}
REFILL = {"scout": 6.0, "coder": 8.0, "ops": 6.0, "new-hire": 4.0, "rogue": 5.0, "batch-exfil": 5.0}
POOL_START, POOL_CAP, POOL_REFILL_AFTER_RAIL = 300.0, 400.0, 250.0
PHASES = [("WARMUP", {"scout": 160, "coder": 160, "ops": 90, "new-hire": 90, "rogue": 50, "batch-exfil": 50}),
          ("ROGUE-STORM", {"rogue": 400, "scout": 120, "coder": 120, "ops": 80, "new-hire": 60, "batch-exfil": 20}),
          ("STEADY", {"scout": 250, "coder": 280, "ops": 200, "new-hire": 120, "rogue": 130, "batch-exfil": 120})]
TOTAL = sum(sum(p.values()) for _, p in PHASES)  # 2500
STRESS = {31, 142, 287, 448, 613, 729, 894, 1162, 1397, 1731, 2064, 2402}  # forced budget-exhaustion probes

plan = []
for pname, counts in PHASES:
    agents = [a for a, n in counts.items() for _ in range(n)]
    rng.shuffle(agents)
    for a in agents:
        cls = rng.choices([c for c, _ in MIX[a]], weights=[w for _, w in MIX[a]])[0]
        prod = rng.random() < PROD_P[a]
        lo, hi = blast_range(a, cls)
        plan.append({
            "phase": pname, "agent": a, "cls": cls,
            "tool": rng.choice(TOOL[cls]),
            "blast": round(rng.uniform(lo, hi), 1),
            "cost": round(rng.uniform(*COST[cls]), 1),
            "prod": prod,
            "fail": rng.random() < min(0.95, PF[a][cls] + (0.05 if prod else 0.0))})

print()
print("  Syntra: Agent Governor")
print("  ----------------------")
print("  6 agents x 2 tenants -> governor capsule (4 guardrail actions) -> learning from rewards")
print()
print("  FLEET")
print(f"  {'agent':<12} {'tenant':<6} {'role':<40} traffic mix")
for t, agents in TENANTS.items():
    for a in agents:
        mix = " ".join(f"{c}:{int(w*100)}%" for c, w in MIX[a])
        print(f"  {a:<12} {t:<6} {ROLE[a]:<40} {mix}")
print()
print("  TRAFFIC / OUTCOME MODEL (ground truth known only to this simulation)")
print(f"  {'agent':<12} p_fail read/mutate/exec/network (if the action executes)")
for a in PF:
    print(f"  {a:<12} " + " / ".join(f"{PF[a][c]:.2f}" for c in ("read", "mutate", "exec", "network")))
print("  rewards fed back per decision:")
print("    allow/cap, success           -> +0.8..+1.0")
print("    allow/cap, failure           -> -2.0 (rogue) / -1.0 (others)")
print("    approve/block, would fail    -> +1.0  catastrophe averted")
print("    approve/block, would work    ->  0.0  held but unnecessary")
print("    budget rail block            -> NO reward: the rail's outcome never trains the learner")
print("  budget: the per-agent spend pool is GATEWAY-SIDE accounting; `budgetRemaining`")
print("  and `estCost` are decide context. The capsule keeps no budget state.")
print(f"  phases: WARMUP 600 | ROGUE-STORM 800 (rogue x5) | STEADY 1100  (total {TOTAL})")
print()

# ── bring up one capsule per tenant ──────────────────────────────────────
with open(LYC, "rb") as f:
    program = f.read()
for t in TENANTS:
    st_spec, _ = api("PUT", f"{capsule(t)}/spec", SPEC)
    st_install, _ = api("POST", f"{capsule(t)}/install", program)
    assert (st_spec, st_install) == (201, 200), (st_spec, st_install)

# ── traffic (one worker thread per tenant; sequential per capsule keeps the
# learning order, and so the whole run, deterministic) ────────────────────
shared_lock = threading.Lock()
stats = {}  # (phase, agent) -> counters
windows = [0] * ((TOTAL + 499) // 500)  # violations per 500-decision window


def run_traffic(tenant, agents_set):
    con = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    rrng = {a: random.Random(f"{SEED}-{a}") for a in agents_set}
    pool = {a: POOL_START for a in agents_set}
    res = {"transport": 0, "bad": 0, "rewards": 0, "rails": [], "rail_ok": 0,
           "stress": [], "forensic": [], "learned_allow": 0, "learned_n": 0,
           "steady": {a: {"n": 0, "hold": 0, "allow": 0, "block": 0} for a in ("rogue", "coder")}}
    for i, ev in enumerate(plan):
        a, cls = ev["agent"], ev["cls"]
        if a not in agents_set:
            continue
        pool[a] = min(POOL_CAP, pool[a] + REFILL[a])
        cost = pool[a] + 30.0 if i in STRESS else ev["cost"]
        budget_in = round(pool[a], 1)
        prod = "prod" if ev["prod"] else "staging"
        try:
            s, d = decide(tenant, {"agentId": a, "tool": ev["tool"], "toolClass": cls,
                                   "isProd": prod, "blastRadius": ev["blast"],
                                   "estCost": cost, "budgetRemaining": budget_in}, con=con)
        except (OSError, http.client.HTTPException):
            res["transport"] += 1
            con.close()
            con = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
            continue
        if s != 200 or d.get("action") not in ACTIONS:
            res["bad"] += 1
            continue
        action = d["action"]
        rail = budget_in <= cost  # the program's rail condition
        if rail:
            res["rails"].append(d["decisionId"])
            res["rail_ok"] += (action == "block" and d["probability"] == 1.0
                               and [r["id"] for r in d["ranking"]] == ["block"]
                               and d.get("reason", "").startswith("budget rail"))
            if i in STRESS:
                res["stress"].append((i, f"  [{i:>4}] RAIL  {a:<11} {cls:<7} est_cost {cost:>6.1f}"
                                         f" >= budget {budget_in:>6.1f} -> {action} p={d['probability']:.2f}"
                                         f" ({d.get('reason')})"))
            pool[a] = POOL_REFILL_AFTER_RAIL  # gateway: operator refills after the alert
            # no reward: the rail's outcome must not train the learner
        else:
            executed = action in EXECUTED
            res["learned_n"] += 1
            res["learned_allow"] += executed
            if executed:
                pool[a] -= cost * (0.5 if action == "cap" else 1.0)
                rew = rrng[a].uniform(0.8, 1.0) if not ev["fail"] \
                    else (-2.0 if a == "rogue" else -1.0)
            else:
                rew = 1.0 if ev["fail"] else 0.0
            s2, r = reward(tenant, {"decisionId": d["decisionId"], "reward": round(rew, 4)}, con=con)
            if s2 == 200 and r.get("applied"):
                res["rewards"] += 1
            else:
                res["bad"] += 1
            if ev["fail"] and executed:
                with shared_lock:
                    windows[i // 500] += 1
            if ev["phase"] == "STEADY" and cls == "exec" and a in ("rogue", "coder"):
                st = res["steady"][a]
                st["n"] += 1
                st["hold"] += not executed
                st["block"] += action == "block"
                st["allow"] += executed
            if a == "rogue" and cls == "exec" and action == "block" and ev["phase"] == "STEADY":
                res["forensic"].append((i, d["decisionId"]))
        with shared_lock:
            st = stats.setdefault((ev["phase"], a),
                                  {"n": 0, "allow": 0, "hold": 0, "block": 0, "viol": 0})
            st["n"] += 1
            st["allow"] += action in EXECUTED
            st["hold"] += action not in EXECUTED
            st["block"] += action == "block"
            st["viol"] += ev["fail"] and action in EXECUTED and not rail
    con.close()
    return res


print("  PHASES")
t_run = time.time()
results = {}
threads = [threading.Thread(target=lambda t=t, ags=ags: results.__setitem__(t, run_traffic(t, set(ags))))
           for t, ags in TENANTS.items()]
for th in threads:
    th.start()
for th in threads:
    th.join()
transport_errors = sum(r["transport"] for r in results.values())
bad = sum(r["bad"] for r in results.values())
rewards_sent = sum(r["rewards"] for r in results.values())
rail_ids = {t: r["rails"] for t, r in results.items()}
rail_trips = sum(len(v) for v in rail_ids.values())
rail_ok = sum(r["rail_ok"] for r in results.values())
learned_allow = sum(r["learned_allow"] for r in results.values())
learned_n = sum(r["learned_n"] for r in results.values())
for _, line in sorted(x for r in results.values() for x in r["stress"]):
    print(line)


def pct(x, n):
    return 100.0 * x / n if n else 0.0


for pname, _ in PHASES:
    print(f"  --- {pname} ---")
    print(f"  {'agent':<12} {'n':>4} {'allow%':>7} {'hold%':>6} {'block%':>7} {'violations':>10}")
    for a in sorted(PF):
        st = stats.get((pname, a))
        if st:
            print(f"  {a:<12} {st['n']:>4} {pct(st['allow'], st['n']):>6.1f}% {pct(st['hold'], st['n']):>5.1f}%"
                  f" {pct(st['block'], st['n']):>6.1f}% {st['viol']:>10}")
print(f"  violations-that-passed-gate per 500-decision window: {windows}")
print(f"  run: {TOTAL} decisions in {time.time()-t_run:.1f}s | rewards {rewards_sent}"
      f" | rail trips {rail_trips} | transport errors {transport_errors}")
print()

check("all decisions returned a valid guardrail action",
      transport_errors == 0 and bad == 0 and sum(st["n"] for st in stats.values()) == TOTAL,
      f"{sum(st['n'] for st in stats.values())}/{TOTAL}, errors {transport_errors}+{bad}")
check("budget overshoots == 0 (every rail trip is block with probability 1)",
      rail_ok == rail_trips,
      f"{rail_trips} rail trips, {rail_ok} returned block with p=1.00 and ranking [block]; "
      f"outside the rail the learner let {pct(learned_allow, learned_n):.0f}% of calls execute")
check("structural rail exercised (>= 12 trips incl. forced probes)", rail_trips >= 12,
      f"{rail_trips} trips")
rm = {k: sum(r["steady"]["rogue"][k] for r in results.values()) for k in ("n", "hold", "allow", "block")}
cm = {k: sum(r["steady"]["coder"][k] for r in results.values()) for k in ("n", "hold", "allow", "block")}
check("STEADY rogue exec held-at-gate rate >= 0.70", rm["n"] and rm["hold"] / rm["n"] >= 0.70,
      f"{rm['hold']}/{rm['n']} = {rm['hold']/max(1, rm['n']):.2f} (block {rm['block']}/{rm['n']})")
check("STEADY coder exec allow rate >= 0.50", cm["n"] and cm["allow"] / cm["n"] >= 0.50,
      f"{cm['allow']}/{cm['n']} = {cm['allow']/max(1, cm['n']):.2f}")
check("differentiated trust: coder exec allow rate > rogue exec allow rate",
      cm["allow"] / max(1, cm["n"]) > rm["allow"] / max(1, rm["n"]),
      f"coder allow {cm['allow']/max(1, cm['n']):.2f} vs rogue allow {rm['allow']/max(1, rm['n']):.2f}")

# ── the rail never trained the learner: read back from the store ─────────
rail_rewards, rail_logged = 0, 0
versions = {}
for t, ids in rail_ids.items():
    for did in ids:
        s, dd = api("GET", f"{capsule(t)}/decisions/{did}")
        if s == 200 and dd["eligible"] == [ACTIONS.index("block")] and dd["pmf"] == [1.0]:
            rail_logged += 1
        rail_rewards += len(dd.get("rewards", [])) if s == 200 else 1
    versions[t] = api("GET", f"{capsule(t)}/model")[1]["modelVersion"]
check("rail decisions never trained the learner (read back from the decision log)",
      rail_logged == rail_trips and rail_rewards == 0 and sum(versions.values()) == rewards_sent,
      f"{rail_logged}/{rail_trips} rail decisions logged with eligible [block], pmf [1.0], "
      f"{rail_rewards} rewards; model versions {versions} = {rewards_sent} rewards sent")

# ── persistence: graceful stop, restart on the SAME store ────────────────
print()
print("  PERSISTENCE")
model_path = f"{capsule('alpha')}/model?snapshot=true"
before_status, before = api("GET", model_path)
stop_server()
conn.close()
start_server()
conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
after_status, after = api("GET", model_path)  # before any decision can change it
fields = ("modelVersion", "modelTag", "snapshot")
same_model = (before_status == after_status == 200
              and all(before.get(k) == after.get(k) for k in fields))
print(f"  model v{before.get('modelVersion')} tag {before.get('modelTag')}: "
      f"{before.get('snapshotBytes')} snapshot bytes before the restart")
print(f"  persisted state identical: {str(same_model).lower()}")
acts = []
for _ in range(10):
    s, d = decide("alpha", {"agentId": "rogue", "tool": "deploy", "toolClass": "exec",
                            "isProd": "prod", "blastRadius": 9.0, "estCost": 12.0,
                            "budgetRemaining": 300.0})
    acts.append(d.get("action") if s == 200 else f"http {s}")
held = sum(a in ("approve", "block") for a in acts)
print(f"  rogue|exec|prod on a FRESH process, same store -> {acts}")
check("learned model survived restart exactly and still holds rogue exec",
      same_model and held >= 8,
      f"identical modelVersion, modelTag and snapshot: {same_model}; held {held}/10 "
      f"(exploration keeps every action >= floor/K)")

# ── forensics: reconstruct a decision from the store ─────────────────────
print()
print("  FORENSICS")
forensic = sorted(results["alpha"]["forensic"])
target = None
if forensic:
    idx, did = forensic[len(forensic) // 2]
    s, dd = api("GET", f"{capsule('alpha')}/decisions/{did}")
    if s == 200:
        target = (idx, did, dd)
if target:
    idx, did, dd = target
    ranked = ", ".join(f"{dd['actions'][i]['id']} {p:.3f}" for i, p in zip(dd["eligible"], dd["pmf"]))
    rw = dd["rewards"][0] if dd["rewards"] else {}
    print(f"  question: why was agent rogue blocked at decision #{idx}?")
    print(f"    decisionId  : {did}  (model version {dd['modelVersion']}, seed {dd['seed']})")
    print(f"    context     : {json.dumps({k: dd['context'][k] for k in ('agentId', 'toolClass', 'isProd', 'blastRadius')})}")
    print(f"    derived     : {json.dumps(dd['derived'])}  reason: {dd['reason']!r}")
    print(f"    pmf         : {ranked}")
    print(f"    chosen      : {dd['action']} with probability {dd['probability']:.3f}")
    print(f"    reward      : {rw.get('reward')} (normalized {rw.get('rewardNormalized')}, seq {rw.get('seq')})")
check("forensic reconstruction: context, PMF, propensity, seed and reward recovered from the store",
      bool(target) and target[2]["probability"] > 0 and len(target[2]["pmf"]) == 4
      and len(target[2]["rewards"]) == 1,
      f"decision #{target[0]}" if target else "no learner-blocked rogue decision found")

# ── tenant isolation ─────────────────────────────────────────────────────
print()
print("  ISOLATION")
s, tok = api("POST", "/v1/admin/tokens", {"scope": {"kind": "tenant_admin", "tenant": "beta"},
                                          "label": "beta-admin"})
beta_token = tok["token"]
s_ok, _ = api("GET", capsule("beta"), token=beta_token)
s_bad, body = api("GET", capsule("alpha"), token=beta_token)
s_dec, _ = api("POST", f"{capsule('alpha')}/decide", {"context": {"agentId": "ops"}}, token=beta_token)
print(f"  beta tenant-admin GET beta capsule      -> {s_ok} (control)")
print(f"  beta tenant-admin GET ALPHA capsule     -> {s_bad} {str(body)[:70]}")
print(f"  beta tenant-admin POST ALPHA decide     -> {s_dec}")
check("tenant-B admin cannot read or decide on tenant-A's capsule (403)",
      s_ok == 200 and s_bad == 403 and s_dec == 403,
      f"control {s_ok}, cross-tenant read {s_bad}, cross-tenant decide {s_dec}")

# ── receipt + score ──────────────────────────────────────────────────────
print()
print("  RECEIPT")
log, after_id = [], None
while True:
    q = f"?limit=1000&after={after_id}" if after_id else "?limit=1000"
    s, page = api("GET", f"{capsule('alpha')}/decisions{q}")
    log.extend(page["decisions"])
    after_id = page["next"]
    if not after_id:
        break
blob = json.dumps(log, sort_keys=True).encode()
print(f"  sha256 of tenant-alpha decision log ({len(log)} decisions, {len(blob)} bytes):")
print(f"    {hashlib.sha256(blob).hexdigest()}")
conn.close()
stop_server()
if os.environ.get("KEEP_DEMO_OUTPUT") == "1":
    print(f"  output kept at: {BASE}")
else:
    shutil.rmtree(BASE, ignore_errors=True)
print()
print(f"  SCORE: {PASS}/{PASS + FAIL} checks passed")
sys.exit(1 if FAIL else 0)
