#!/usr/bin/env python3
"""Syntra: Agent Governor — a compiled, learning policy plane between AI
agents and their tools.

A fleet of 6 agents across 2 tenants proposes tool calls. Every proposal is
routed through a Syntra capsule whose guardrail node learns from delayed
rewards which guardrail action (allow / allow-with-cap / require-human-
approval / block) a context deserves. A structural budget rail sits UNDER
the learner: when the (gateway-side) spend signal says the budget is spent,
the graph returns BLOCK regardless of what the bandit's weights would have
picked — learning cannot trade safety away.

Run:  python3 scripts/demo-agent-governor.py
Env:  KEEP_DEMO_OUTPUT=1 keeps the temp store and prints its path.
"""
import hashlib
import http.client
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LYCAN = os.path.join(ROOT, "target", "release", "lycan")
if not os.path.exists(LYCAN):
    subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)

BASE = tempfile.mkdtemp(prefix="syntra-agent-governor.", dir=os.environ.get("TMPDIR", "/tmp"))
STORE = os.path.join(BASE, "store")
KEY = "governor-demo-key"
PORT = 12000 + (int.from_bytes(os.urandom(2), "big") % 500)
ADDR = f"127.0.0.1:{PORT}"
SEED = 20260908
rng = random.Random(SEED)

PASS, FAIL = 0, 0
def check(label, cond, detail=""):
    global PASS, FAIL
    mark = "PASS" if cond else "FAIL"
    PASS, FAIL = PASS + bool(cond), FAIL + (not cond)
    print(f"  {mark}: {label}" + (f"  [{detail}]" if detail else ""))
    return cond

# ── capsule ──────────────────────────────────────────────────────────────
CAPSULE = r"""
;; AGENT FLEET GOVERNOR — a compiled guardrail policy plane for agent tool calls.
;; Guardrail options: 0 allow | 1 allow-with-cap | 2 require-human-approval | 3 block
;;
;; TWO LAYERS:
;;  (a) STRUCTURAL GUARDRAIL UNDER THE LEARNER. `budgetRemaining` arrives as a
;;      decide input (gateway-side spend accounting — the capsule is stateless
;;      per decide and tracks no budget itself). When the budget rail trips the
;;      action the graph RETURNS is 3 (block) no matter what the bandit's
;;      weights would have picked — the response shows this: the returned
;;      action is 3 even when the choice node's own recorded pick is 0/1.
;;      The demo deliberately sends NO feedback for rail decisions, so the
;;      rail's outcome can never train the learner — learning cannot trade a
;;      spent-down budget for an action.
;;  (b) CONTEXT-AWARE STATIC RISK PRIORS. The F functions below score each
;;      guardrail option for the request context (high blast + prod + exec
;;      leans risky). The demo seeds these scores into the per-context choice
;;      weights via /feedback, and online learning then refines them from real
;;      outcomes. The contextKey is `agentId|toolClass|isProd`.
($ raw_agent (!cap "runtime.inputGet" "agentId"))
($ agent (? (!= raw_agent null) raw_agent "anon"))
($ raw_tool (!cap "runtime.inputGet" "tool"))
($ tool (? (!= raw_tool null) raw_tool "unknown"))
($ raw_class (!cap "runtime.inputGet" "toolClass"))
($ tool_class (? (!= raw_class null) raw_class "read"))
($ raw_prod (!cap "runtime.inputGet" "isProd"))
($ is_prod (? (!= raw_prod null) raw_prod "staging"))
($ raw_blast (!cap "runtime.inputGet" "blastRadius"))
($ blast (? (!= raw_blast null) raw_blast 1.0))
($ raw_cost (!cap "runtime.inputGet" "estCost"))
($ est_cost (? (!= raw_cost null) raw_cost 1.0))
($ raw_budget (!cap "runtime.inputGet" "budgetRemaining"))
($ budget (? (!= raw_budget null) raw_budget 100000.0))

($ exec (? (== tool_class "exec") 1.0 0.0))
($ net (? (== tool_class "network") 1.0 0.0))
($ mut (? (== tool_class "mutate") 1.0 0.0))
($ prod (? (== is_prod "prod") 1.0 0.0))
($ big (? (> blast 6.0) 1.0 0.0))

(F allow_score () (- 90.0 (+ (* exec 20.0) (+ (* prod 18.0) (+ (* big 28.0) (+ (* net 10.0) (* mut 5.0)))))))
(F cap_score () (- 72.0 (+ (* big 24.0) (+ (* exec 8.0) (* prod 6.0)))))
(F approve_score () (+ 34.0 (+ (* exec 9.0) (+ (* prod 8.0) (+ (* big 14.0) (* net 6.0))))))
(F block_score () (+ 6.0 (+ (* exec 12.0) (+ (* prod 10.0) (+ (* big 22.0) (+ (* net 8.0) (* (* exec prod) 20.0)))))))

(F static_best ()
  (? (&& (> (block_score) (approve_score)) (&& (> (block_score) (allow_score)) (> (block_score) (cap_score)))) 3
    (? (&& (> (approve_score) (allow_score)) (> (approve_score) (cap_score))) 2
      (? (> (cap_score) (allow_score)) 1 0))))

;; (a) the rail: spent budget forces option 3 BEFORE the choice node
($ over_budget (? (<= budget est_cost) 1 0))
($ decision (? (> over_budget 0) 3 (choice 0 1 2 3)))

(!p "GOV agent:" agent "tool:" tool "class:" tool_class "prod:" is_prod "blast:" blast "cost:" est_cost "budget:" budget)
(!p "SCORES allow" (allow_score) "cap" (cap_score) "approve" (approve_score) "block" (block_score) "static_best" (static_best))
(!p "RAIL budget_exhausted:" over_budget "ACTION:" decision)
decision
"""
SRC = os.path.join(BASE, "governor.lycs")
with open(SRC, "w") as f:
    f.write(CAPSULE)
subprocess.run([LYCAN, "compile", SRC], check=True, capture_output=True)
LYC = SRC[:-len(".lycs")] + ".lyc"

# ── server lifecycle ─────────────────────────────────────────────────────
proc = None
def start_server():
    global proc
    log = open(os.path.join(BASE, "server.log"), "ab")
    proc = subprocess.Popen([LYCAN, "serve", "--addr", ADDR, "--store", STORE,
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
            time.sleep(0.2)
    raise RuntimeError("server did not become ready within 10s")

def stop_server():
    proc.terminate()
    proc.wait(timeout=10)

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

def decide(tenant, ctx, inp, learn=False, con=None):
    q = "?learn=true" if learn else ""
    return api("POST", f"/tenants/{tenant}/jobs/fleet/capsules/governor/decide{q}",
               {"contextKey": ctx, "input": inp}, con=con)

def feedback(tenant, body, con=None):
    return api("POST", f"/tenants/{tenant}/jobs/fleet/capsules/governor/feedback", body, con=con)
# ── fleet model ──────────────────────────────────────────────────────────
TENANTS = {"alpha": ["scout", "coder", "rogue", "batch-exfil"],
           "beta": ["ops", "new-hire"]}
ROLE = {"scout": "read-only recon", "coder": "build & ship (mutate/exec)",
        "ops": "production operations (deploys)", "rogue": "compromised agent — reckless exec/deploy",
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
    assert len(agents) == sum(counts.values())
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
print("  6 agents x 2 tenants -> compiled guardrail capsule -> delayed-reward learning")
print()
print("  FLEET")
print(f"  {'agent':<12} {'tenant':<6} {'role':<38} traffic mix")
for t, agents in TENANTS.items():
    for a in agents:
        mix = " ".join(f"{c}:{int(w*100)}%" for c, w in MIX[a])
        print(f"  {a:<12} {t:<6} {ROLE[a]:<38} {mix}")
print()
print("  TRAFFIC / OUTCOME MODEL (ground truth known only to this simulation)")
print(f"  {'agent':<12} p_fail read/mutate/exec/network (if the action executes)")
for a in PF:
    print(f"  {a:<12} " + " / ".join(f"{PF[a][c]:.2f}" for c in ("read", "mutate", "exec", "network")))
print("  rewards fed back per decision:")
print("    executed, success      -> +0.8..+1.0")
print("    executed, failure      -> -2.0 (rogue) / -1.0 (others)")
print("    held (2/3), would fail -> +1.0  catastrophe averted")
print("    held (2/3), would work ->  0.0  blocked-but-unnecessary (no signal)")
print("    structural rail block  -> NO feedback: the rail outcome is never credited to the learner")
print("  budget: per-agent spend pool is GATEWAY-SIDE accounting; `budgetRemaining` is")
print("  passed as a decide input. The capsule is stateless per decide — it never")
print("  tracks budgets itself. A real gateway would do this accounting upstream.")
print(f"  phases: WARMUP 600 | ROGUE-STORM 800 (rogue x5) | STEADY 1100  (total {TOTAL})")
print()

# ── bring up tenants ─────────────────────────────────────────────────────
api("POST", "/admin/rng/seed", {"seed": SEED})
for t in TENANTS:
    api("POST", f"/tenants/{t}/jobs", {"id": "fleet", "name": "Agent Fleet",
                                       "description": "Governed agent tool-call fleet"})
    with open(LYC, "rb") as f:
        api("POST", f"/tenants/{t}/jobs/fleet/capsules/governor/install", f.read())

# ── priors: seed each context's choice weights from the capsule's own scores ──
CLASSES = ["read", "mutate", "exec", "network"]
node_id = {}
seeded = 0
t_prior = time.time()
for t, agents in TENANTS.items():
    for a in agents:
        for cls in CLASSES:
            if not any(c == cls for c, _ in MIX[a]):
                continue
            for prod in ("prod", "staging"):
                lo, hi = blast_range(a, cls)
                rep_blast = hi if a in ("rogue", "batch-exfil") else round((lo + hi) / 2, 1)
                s, d = decide(t, f"{a}|{cls}|{prod}", {
                    "agentId": a, "tool": "probe", "toolClass": cls, "isProd": prod,
                    "blastRadius": rep_blast, "estCost": 16.0, "budgetRemaining": 9999.0})
                line = next(l for l in d["stdout"] if l.startswith("SCORES"))
                sc = {k: float(v) for k, v in re.findall(r"(allow|cap|approve|block) ([-\d.]+)", line)}
                nid = d["decisions"][0]["node_id"]
                node_id[t] = nid
                order = sorted(sc, key=sc.get)  # worst..best
                feedback(t, {"strategyId": nid, "option": order[-1], "reward": 1.5,
                             "contextKey": f"{a}|{cls}|{prod}"})
                feedback(t, {"strategyId": nid, "option": order[0], "reward": -0.8,
                             "contextKey": f"{a}|{cls}|{prod}"})
                seeded += 1
print("  PRIORS")
print(f"  seeded {seeded} contexts (agent|toolClass|isProd) from the capsule's static risk")
print(f"  scores via /feedback in {time.time()-t_prior:.1f}s — learning starts from the")
print("  static risk policy and is then free to be corrected by real outcomes")
check("priors seeded for every traffic context", seeded == sum(
    2 * len([c for c in CLASSES if any(cc == c for cc, _ in MIX[a])]) for agents in TENANTS.values() for a in agents),
      f"{seeded} contexts")

# ── traffic (one worker thread per tenant; sequential per capsule keeps the
# per-context learning order deterministic, tenants are independent) ──────
shared_lock = threading.Lock()
stats = {}  # (phase, agent) -> counters
windows = [0] * ((TOTAL + 499) // 500)  # violations per 500-decision window
forensic_ids = []
steady_metrics = {"rogue": {"n": 0, "hold": 0, "allow": 0, "block3": 0},
                  "coder": {"n": 0, "hold": 0, "allow": 0, "block3": 0}}

def run_traffic(tenant, agents_set):
    con = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    rrng = {a: random.Random(f"{SEED}-{a}") for a in agents_set}
    pool = {a: POOL_START for a in agents_set}
    res = {"transport": 0, "bad": 0, "fb": 0, "rails": 0, "over": 0,
           "overrode": 0, "stress": []}
    steady = {a: {"n": 0, "hold": 0, "allow": 0, "block3": 0} for a in ("rogue", "coder")}
    fails = 0
    for i, ev in enumerate(plan):
        a, cls = ev["agent"], ev["cls"]
        if a not in agents_set:
            continue
        pool[a] = min(POOL_CAP, pool[a] + REFILL[a])
        cost = pool[a] + 30.0 if i in STRESS else ev["cost"]
        budget_in = round(pool[a], 1)
        prod = "prod" if ev["prod"] else "staging"
        ctx = f"{a}|{cls}|{prod}"
        try:
            s, d = decide(tenant, ctx, {"agentId": a, "tool": ev["tool"], "toolClass": cls,
                                        "isProd": prod, "blastRadius": ev["blast"],
                                        "estCost": cost, "budgetRemaining": budget_in},
                          learn=True, con=con)
            if s != 200 or not d.get("ok"):
                res["transport"] += 1
                continue
            action = int(d["result"])
            rail = cost >= budget_in           # capsule rail: budget <= estCost
            if rail:
                res["rails"] += 1
                res["over"] += action != 3
                if d["decisions"]:
                    lp = d["decisions"][0]["chosen_option"]
                    res["overrode"] += (lp < 3 and action == 3)
                if i in STRESS:
                    pick = d["decisions"][0]["chosen_option"] if d["decisions"] else None
                    res["stress"].append((i, f"  [{i:>4}] RAIL  {a:<11} {cls:<7} "
                                            f"est_cost {cost:>6.1f} >= budget {budget_in:>6.1f}"
                                            f" -> returned action {action}"
                                            + (f" (learner's own pick was {pick})" if pick is not None else "")))
                pool[a] = POOL_REFILL_AFTER_RAIL  # gateway: operator refills after alert
                # no feedback: the rail's outcome must not train the learner
            else:
                executed = action <= 1
                if executed:
                    pool[a] -= cost * (0.5 if action == 1 else 1.0)
                if executed:
                    rew = rrng[a].uniform(0.8, 1.0) if not ev["fail"] \
                        else (-2.0 if a == "rogue" else -1.0)
                else:
                    rew = 1.0 if ev["fail"] else 0.0
                if d["decisions"]:
                    s2, _ = feedback(tenant, {"decisionId": d["decisionId"], "reward": rew},
                                     con=con)
                    if s2 == 200:
                        res["fb"] += 1
                    else:
                        res["transport"] += 1
                else:
                    res["bad"] += 1
                if ev["fail"] and action <= 1:
                    with shared_lock:
                        windows[i // 500] += 1
                if ev["phase"] == "STEADY" and cls == "exec" and a in ("rogue", "coder"):
                    steady[a]["n"] += 1
                    steady[a]["hold"] += action >= 2
                    steady[a]["block3"] += action == 3
                    steady[a]["allow"] += action <= 1
                if (a == "rogue" and cls == "exec" and action == 3
                        and ev["phase"] == "STEADY" and d["decisions"]):
                    with shared_lock:
                        forensic_ids.append((i, tenant, d["decisionId"]))
            with shared_lock:
                st = stats.setdefault((ev["phase"], a),
                                      {"n": 0, "allow": 0, "hold": 0, "block3": 0, "viol": 0,
                                       "rail": 0})
                st["n"] += 1
                st["allow"] += action <= 1
                st["hold"] += action >= 2
                st["block3"] += action == 3
                st["viol"] += ev["fail"] and action <= 1
                st["rail"] += rail
            fails = 0
        except (OSError, ValueError) as e:
            fails += 1
            res["transport"] += 1
            if fails > 20:
                print(f"  traffic worker {tenant} aborted: {e}")
                break
            try:
                con.close()
            except OSError:
                pass
            con = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
    con.close()
    return res, steady

print()
print("  PHASES")
t_run = time.time()
results = {}
threads = {t: threading.Thread(target=lambda t=t, ags=ags: results.__setitem__(
    t, run_traffic(t, set(ags)))) for t, ags in TENANTS.items()}
for th in threads.values():
    th.start()
for th in threads.values():
    th.join()
transport_errors = sum(r["transport"] for r, _ in results.values())
bad_decisions = sum(r["bad"] for r, _ in results.values())
feedbacks_sent = sum(r["fb"] for r, _ in results.values())
struct_blocks = sum(r["rails"] for r, _ in results.values())
overshoots = sum(r["over"] for r, _ in results.values())
rail_overrode = sum(r["overrode"] for r, _ in results.values())
for _, line in sorted((i, ln) for r, _ in results.values() for i, ln in r["stress"]):
    print(line)
for _r, steady in results.values():
    for a in ("rogue", "coder"):
        for k, v in steady[a].items():
            steady_metrics[a][k] += v
forensic_ids.sort()

def pct(x, n):
    return 100.0 * x / n if n else 0.0

def phase_table(pname):
    print(f"  --- {pname} ---")
    print(f"  {'agent':<12} {'n':>4} {'allow%':>7} {'hold%':>6} {'block%':>7} {'violations':>10}")
    for a in sorted(PF):
        st = stats.get((pname, a))
        if not st:
            continue
        print(f"  {a:<12} {st['n']:>4} {pct(st['allow'], st['n']):>6.1f}% {pct(st['hold'], st['n']):>5.1f}%"
              f" {pct(st['block3'], st['n']):>6.1f}% {st['viol']:>10}")

for pname, _ in PHASES:
    phase_table(pname)
print(f"  violations-that-passed-gate per 500-decision window: {windows}")
print(f"  run: {TOTAL} decisions in {time.time()-t_run:.0f}s | feedbacks {feedbacks_sent}"
      f" | rail trips {struct_blocks} | transport errors {transport_errors}")
print()

check("all decisions returned a valid guardrail action", transport_errors == 0 and bad_decisions == 0
      and sum(st["n"] for st in stats.values()) == TOTAL,
      f"{sum(st['n'] for st in stats.values())}/{TOTAL}, errors {transport_errors}+{bad_decisions}")
check("budget overshoots == 0 across the run (rail decides; learner cannot trade it)",
      overshoots == 0,
      f"{struct_blocks} rail trips, all returned action 3; rail beat the learner's "
      f"more permissive pick {rail_overrode} times")
check("structural rail exercised (>= 12 trips incl. forced probes)", struct_blocks >= 12,
      f"{struct_blocks} trips")
rm, cm = steady_metrics["rogue"], steady_metrics["coder"]
check("STEADY rogue exec held-at-gate rate >= 0.70", rm["n"] and rm["hold"] / rm["n"] >= 0.70,
      f"{rm['hold']}/{rm['n']} = {rm['hold']/max(1,rm['n']):.2f} (strict block {(rm['block3'])}/{rm['n']})")
check("STEADY coder exec allow rate >= 0.50", cm["n"] and cm["allow"] / cm["n"] >= 0.50,
      f"{cm['allow']}/{cm['n']} = {cm['allow']/max(1,cm['n']):.2f}")
check("differentiated trust: coder exec allow rate > rogue exec allow rate",
      cm["allow"] / max(1, cm["n"]) > rm["allow"] / max(1, rm["n"]),
      f"coder allow {cm['allow']/max(1,cm['n']):.2f} vs rogue allow {rm['allow']/max(1,rm['n']):.2f}")

# ── persistence: kill server, restart on the SAME store ─────────────────
print()
print("  PERSISTENCE")
memory_path = "/tenants/alpha/jobs/fleet/capsules/governor/memory"
before_status, before_memory = api("GET", memory_path)
stop_server()
conn.close()
start_server()
conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
# Inspect before any decision can change exploration or bookkeeping state.
after_status, after_memory = api("GET", memory_path)
same_memory = (before_status == after_status == 200
               and isinstance(before_memory, dict)
               and before_memory == after_memory)
print(f"  persisted state identical: {str(same_memory).lower()}")
acts = []
valid_probes = True
for _ in range(5):
    s, d = decide("alpha", "rogue|exec|prod", {"agentId": "rogue", "tool": "deploy",
                                               "toolClass": "exec", "isProd": "prod",
                                               "blastRadius": 9.0, "estCost": 12.0,
                                               "budgetRemaining": 300.0})
    acts.append(int(d["result"]))
    valid_probes = valid_probes and s == 200 and 0 <= acts[-1] <= 3
held = sum(a >= 2 for a in acts)
# the persisted memory bucket (ground truth) — not just the response echo
mem_raw = after_memory
w = []
for stv in (mem_raw if isinstance(mem_raw, dict) else {}).get("strategies", {}).values():
    b = stv.get("contexts", {}).get("rogue|exec|prod")
    if b:
        w = b["weights"]
print(f"  rogue|exec|prod on a FRESH process, same store -> first decision action {acts[0]},"
      f" 5-decision actions {acts} (min_exploration can occasionally probe)")
print(f"    persisted memory weights [{', '.join(f'{x:.3f}' for x in w)}]"
      f"  (0 allow / 1 cap / 2 approve / 3 block)")
check("learned memory survived kill+restart exactly and still favors holding rogue exec",
      same_memory and valid_probes and len(w) == 4 and sum(w[2:]) > sum(w[:2]),
      f"identical state {same_memory}, first action {acts[0]}, held {held}/5; exploration remains enabled")

# ── forensics: reconstruct from the store, not from the simulation ───────
print()
print("  FORENSICS")
s, dec_raw = api("GET", "/tenants/alpha/jobs/fleet/capsules/governor/decisions")
s2, aud_raw = api("GET", "/tenants/alpha/jobs/fleet/capsules/governor/audits")
dec_lines = [l for l in dec_raw.decode().splitlines() if l.strip()]
aud_lines = [l for l in aud_raw.decode().splitlines() if l.strip()]
# a mid-run decision where the LEARNER itself blocked rogue (STEADY, learner consulted)
aud = None
target = None
for idx, tenant, did in forensic_ids:
    for l in dec_lines:
        try:
            ev = json.loads(l)
        except ValueError:
            continue
        if ev.get("id") == did and ev.get("decisions") and ev["decisions"][0].get("chosen_option") == 3:
            target = (idx, did, ev)
            break
    if target:
        break
if target:
    idx, did, ev = target
    dd = ev["decisions"][0]
    ws = ", ".join(f"{x:.3f}" for x in dd["weights"])
    means = [round(o.get("rewardMean", 0.0), 2) for o in dd.get("contextStats", [])]
    aud = next((json.loads(l) for l in aud_lines if f'"{did}"' in l), None)
    print(f"  question: why was agent rogue blocked at decision #{idx}?")
    print(f"    decisionId  : {did}")
    print(f"    contextKey  : {ev['contextKey']}  (from the store's decision log)")
    print(f"    chosen      : option {dd['chosen_option']} (block), {dd.get('activations')} activations")
    print(f"    weights     : [{ws}]")
    print(f"    mean reward per option (context memory at that point): {means}")
    if aud:
        print(f"    audit line  : action={aud['action']} tenant={aud['tenant']} job={aud['job']}"
              f" beforeHash={aud.get('beforeHash','')[:12]}.. afterHash={aud.get('afterHash','')[:12]}.."
              f" t={aud.get('timestamp')}")
    print(f"    -> block weight dominated after repeated catastrophe-averted rewards;")
    print(f"       options 0/1 carry negative mean reward in this context.")
check("forensic reconstruction: decision + audit line recovered from the store", bool(target) and aud is not None,
      f"decision #{target[0]}" if target else "no learner-blocked rogue decision found")

# ── tenant isolation ─────────────────────────────────────────────────────
print()
print("  ISOLATION")
s, tok = api("POST", "/admin/tokens", {"scope": {"kind": "tenant_admin", "tenant": "beta"},
                                       "label": "beta-admin"})
beta_token = tok["token"]
s_ok, _ = api("GET", "/tenants/beta/jobs/fleet/capsules/governor/report", token=beta_token)
s_bad, body = 0, ""
try:
    s_bad, body = api("GET", "/tenants/alpha/jobs/fleet/capsules/governor/report", token=beta_token)
except http.client.HTTPException as e:
    s_bad, body = 0, str(e)
print(f"  beta tenant-admin GET beta report  -> {s_ok} (control)")
print(f"  beta tenant-admin GET ALPHA report -> {s_bad} {str(body)[:80]}")
check("tenant-B admin cannot read tenant-A capsule report (401/403)",
      s_ok == 200 and s_bad in (401, 403), f"control {s_ok}, cross-tenant {s_bad}")

# ── receipt + score ──────────────────────────────────────────────────────
print()
print("  RECEIPT")
s, dec_raw = api("GET", "/tenants/alpha/jobs/fleet/capsules/governor/decisions")
digest = hashlib.sha256(dec_raw).hexdigest()
nlines = dec_raw.decode().count("\n")
print(f"  sha256 of tenant-alpha decision log ({len(dec_raw)} bytes, {nlines} decisions):")
print(f"    {digest}")
conn.close()
if os.environ.get("KEEP_DEMO_OUTPUT") == "1":
    print(f"  output kept at: {BASE}")
    try:
        stop_server()
    except Exception:
        pass
else:
    stop_server()
    shutil.rmtree(BASE, ignore_errors=True)
print()
print(f"  SCORE: {PASS}/{PASS + FAIL} checks passed")
sys.exit(1 if FAIL else 0)
