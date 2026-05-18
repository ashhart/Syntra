#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LYCAN="$ROOT/target/release/lycan"
BASE="${LYCAN_API:-http://localhost:8787}"
KEY="${LYCAN_ADMIN_KEY:-syntra-demo-key}"

[[ -x "$LYCAN" ]] || (cd "$ROOT" && cargo build --release --quiet)

"$LYCAN" compile "$ROOT/examples/demo_mars_horizons_api.lycs" >/dev/null
"$LYCAN" compile "$ROOT/examples/demo_planetary_defense.lycs" >/dev/null
"$LYCAN" compile "$ROOT/examples/demo_grid_blackout_prevention.lycs" >/dev/null
"$LYCAN" compile "$ROOT/examples/demo_evolve_target.lycs" >/dev/null

python3 - "$ROOT" "$BASE" "$KEY" <<'PY'
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import textwrap
import time
import urllib.error
import urllib.request

root = pathlib.Path(sys.argv[1])
base = sys.argv[2].rstrip("/")
key = sys.argv[3]
tenant = f"boundary{int(time.time())}"

headers_json = {"Authorization": f"Bearer {key}", "Content-Type": "application/json"}
headers_bin = {"Authorization": f"Bearer {key}", "Content-Type": "application/octet-stream"}

passed = 0
failed = 0

def request(method, path, body=None, headers=None, timeout=120, parse_json=True):
    data = None
    if body is not None:
        if isinstance(body, (dict, list)):
            data = json.dumps(body).encode()
        elif isinstance(body, str):
            data = body.encode()
        else:
            data = body
    req = urllib.request.Request(base + path, data=data, method=method, headers=headers or {"Authorization": f"Bearer {key}"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            raw = resp.read().decode()
            if parse_json and raw.strip().startswith("{"):
                return resp.status, raw, json.loads(raw)
            return resp.status, raw, raw
    except urllib.error.HTTPError as err:
        raw = err.read().decode()
        try:
            parsed = json.loads(raw)
        except Exception:
            parsed = raw
        return err.code, raw, parsed

def check(condition, message, detail=""):
    global passed, failed
    if condition:
        passed += 1
        print(f"  PASS: {message}")
        if detail:
            for line in detail.splitlines():
                print(f"        {line}")
    else:
        failed += 1
        print(f"  FAIL: {message}")
        if detail:
            for line in detail.splitlines():
                print(f"        {line}")

def create_job(job, name):
    status, raw, _ = request("POST", f"/tenants/{tenant}/jobs", {"id": job, "name": name}, headers_json)
    check(status in (200, 409), f"job exists/created: {job}", raw[:180])

def install(job, capsule, lyc_path):
    data = pathlib.Path(lyc_path).read_bytes()
    status, raw, parsed = request("POST", f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/install", data, headers_bin)
    check(status == 200 and parsed.get("job") == job, f"installed {capsule} into {job}", raw[:180])

def decide(job, capsule, payload=None, timeout=120):
    return request("POST", f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/decide", payload or {}, headers_json, timeout=timeout)

def feedback(job, capsule, payload):
    return request("POST", f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/feedback", payload, headers_json)

def report(job, capsule):
    status, raw, parsed = request("GET", f"/tenants/{tenant}/jobs/{job}/capsules/{capsule}/report")
    if status != 200:
        raise RuntimeError(f"report failed for {job}/{capsule}: {status} {raw}")
    return parsed

def weights(job, capsule):
    rep = report(job, capsule)
    strat = rep["strategies"][0]
    return strat["node_id"], [opt["weight"] for opt in strat["options"]]

def stdout_text(parsed):
    return "\n".join(str(x) for x in parsed.get("stdout", []))

print("================================================================")
print("              LYCAN BOUNDARY API TEST SUITE")
print("================================================================")
print(f"API:    {base}")
print(f"tenant: {tenant}")
print()

status, raw, _ = request("GET", "/health", headers={})
check(status == 200, "server is healthy", raw)

print()
print("1. MISSION CONTROL: Earth-Mars porkchop via API")
print("   Live NASA/JPL Horizons HTTPS + native Lambert solver + runtime.input + stats")
create_job("mission-control", "Mission Control")
install("mission-control", "mars", root / "examples/demo_mars_horizons_api.lyc")
mission_policy = {
    "allow_stdout": True,
    "allow_stdin": False,
    "allow_file_read": False,
    "allow_file_write": False,
    "allow_network": True,
    "allowed_hosts": ["ssd.jpl.nasa.gov"],
    "deny_private_networks": True,
}
status, raw, _ = request("PUT", f"/tenants/{tenant}/jobs/mission-control/capsules/mars/policy", mission_policy, headers_json)
check(status == 200, "mission capsule policy allows only NASA/JPL Horizons network egress", raw[:180])
mission = {
    "horizons": {"start": "2026-Jan-01", "stop": "2028-Jan-01", "step_days": 5.0},
    "max_c3": 12.0,
    "min_tof": 220.0,
    "max_tof": 330.0,
    "search_window_days": 500.0,
    "objective": "minimize_c3",
}
status, raw, parsed = decide("mission-control", "mars", mission, timeout=180)
mars_out = stdout_text(parsed) if isinstance(parsed, dict) else raw
check(status == 200 and "Live NASA/JPL Horizons API" in mars_out and "Fetched vectors:" in mars_out and "C3:" in mars_out and parsed.get("decisions"),
      "Mars mission fetched Horizons vectors and produced structured science decision",
      "\n".join([line for line in mars_out.splitlines() if "Earth records:" in line or "Mars records:" in line or "Date:" in line or "TOF:" in line or "C3:" in line][:6]))
mars_decision = parsed["decisionId"]
mars_node = parsed["decisions"][0]["node_id"]
mars_before = parsed["decisions"][0]["weights"]
for _ in range(8):
    feedback("mission-control", "mars", {"decisionId": mars_decision, "reward": 1.0})
_, mars_after = weights("mission-control", "mars")
check(max(mars_after) > max(mars_before), f"Mars search strategy learned from mission outcome (node {mars_node})",
      f"before={mars_before}\nafter={mars_after}")

print()
print("2. PLANETARY DEFENSE: robust asteroid deflection")
print("   100 uncertainty cases, five interventions, delayed feedback teaches robust policy")
create_job("planetary-defense", "Planetary Defense")
install("planetary-defense", "deflect", root / "examples/demo_planetary_defense.lyc")
status, raw, parsed = decide("planetary-defense", "deflect", {}, timeout=120)
defense_out = stdout_text(parsed) if isinstance(parsed, dict) else raw
check(status == 200 and "PLANETARY DEFENSE" in defense_out and "Best strategy by robust score:" in defense_out,
      "defense model scored intervention policies",
      "\n".join([line for line in defense_out.splitlines() if "Best strategy" in line or "AdaptiveChoice" in line]))
def_node, defense_before = weights("planetary-defense", "deflect")
for _ in range(18):
    feedback("planetary-defense", "deflect", {"strategyId": def_node, "option": 4, "reward": 1.0})
_, defense_after = weights("planetary-defense", "deflect")
check(defense_after.index(max(defense_after)) == 4 and defense_after[4] > 0.70,
      "feedback converged toward hybrid tracking/trim policy",
      f"before={defense_before}\nafter={defense_after}")

print()
print("3. AUTONOMOUS EVOLUTION: proposal accepted/rejected through HTTP")
print("   Candidate-first graft, fresh benchmark gate, external evolution journal")
create_job("evolution-lab", "Evolution Lab")
install("evolution-lab", "sum", root / "examples/demo_evolve_target.lyc")
good = json.loads((root / "examples/proposals/good_strategy.json").read_text())
bad = json.loads((root / "examples/proposals/wrong_output.json").read_text())
status, raw, parsed = request("POST", f"/tenants/{tenant}/jobs/evolution-lab/capsules/sum/evolve",
                              {"proposal": good, "minImprovement": 0.0}, headers_json, timeout=180)
check(status == 200 and parsed.get("accepted") == 1,
      "good evolution proposal accepted after benchmark",
      raw[:260])
status, raw, parsed = request("POST", f"/tenants/{tenant}/jobs/evolution-lab/capsules/sum/evolve",
                              {"proposal": bad, "minImprovement": 0.0}, headers_json, timeout=180)
check(status == 200 and parsed.get("rejected", 0) >= 1,
      "wrong-output evolution proposal rejected",
      raw[:260])
status, journal, _ = request("GET", f"/tenants/{tenant}/jobs/evolution-lab/capsules/sum/evolution", parse_json=False)
valid_journal = True
events = []
for line in journal.splitlines():
    if line.strip():
        try:
            events.append(json.loads(line)["event"])
        except Exception:
            valid_journal = False
check(status == 200 and valid_journal and "ProposalAccepted" in events and "ProposalRejected" in events,
      "evolution journal is valid JSONL and records both outcomes",
      ", ".join(events[-8:]))

print()
print("4. HOSTILE CAPSULES: filesystem and SSRF sandbox")
print("   Allow the effect, then prove the sandbox still blocks dangerous targets")
with tempfile.TemporaryDirectory(prefix="lycan-boundary-") as td:
    td = pathlib.Path(td)
    file_src = td / "file_escape.lycs"
    file_lyc = td / "file_escape.lyc"
    file_src.write_text('(!p (!cap "file.readText" "/etc/passwd"))\n')
    subprocess.run([str(root / "target/release/lycan"), "compile", str(file_src)], check=True, stdout=subprocess.DEVNULL)
    net_src = td / "ssrf.lycs"
    net_lyc = td / "ssrf.lyc"
    net_src.write_text('(!p (!cap "http.get" "http://169.254.169.254/latest/meta-data/"))\n')
    subprocess.run([str(root / "target/release/lycan"), "compile", str(net_src)], check=True, stdout=subprocess.DEVNULL)
    create_job("red-team", "Red Team")
    install("red-team", "file-escape", file_lyc)
    policy_file = {
        "allow_stdout": True, "allow_stdin": False,
        "allow_file_read": True, "allow_file_write": False,
        "allow_network": False, "file_root": ".",
        "allowed_hosts": [], "deny_private_networks": True,
    }
    request("PUT", f"/tenants/{tenant}/jobs/red-team/capsules/file-escape/policy", policy_file, headers_json)
    status, raw, _ = decide("red-team", "file-escape", {}, timeout=30)
    check(status >= 400 and ("absolute" in raw.lower() or "sandbox" in raw.lower() or "denied" in raw.lower()),
          "absolute file read blocked even when file_read is granted",
          raw[:260])
    install("red-team", "ssrf", net_lyc)
    policy_net = {
        "allow_stdout": True, "allow_stdin": False,
        "allow_file_read": False, "allow_file_write": False,
        "allow_network": True, "file_root": ".",
        "allowed_hosts": ["169.254.169.254"], "deny_private_networks": True,
    }
    request("PUT", f"/tenants/{tenant}/jobs/red-team/capsules/ssrf/policy", policy_net, headers_json)
    status, raw, _ = decide("red-team", "ssrf", {}, timeout=30)
    check(status >= 400 and ("private" in raw.lower() or "denied" in raw.lower() or "sandbox" in raw.lower()),
          "metadata-service SSRF blocked despite explicit host allowlist",
          raw[:260])

print()
print("5. INFRASTRUCTURE MEMORY FORKS: one grid brain, three jobs")
print("   Same capsule, separate learned operational policy per deployment")
grid_jobs = [("grid-storage", 2), ("grid-peaker", 3), ("grid-coordinator", 4)]
for job, target in grid_jobs:
    create_job(job, job.replace("-", " ").title())
    install(job, "blackout", root / "examples/demo_grid_blackout_prevention.lyc")
    node, before = weights(job, "blackout")
    for _ in range(16):
        feedback(job, "blackout", {"strategyId": node, "option": target, "reward": 1.0})
    _, after = weights(job, "blackout")
    check(after.index(max(after)) == target and after[target] > 0.70,
          f"{job} independently learned option {target}",
          f"before={before}\nafter={after}")

print()
print("================================================================")
print(f"BOUNDARY TESTS COMPLETE: PASS={passed} FAIL={failed}")
print("================================================================")
if failed:
    raise SystemExit(1)
PY
