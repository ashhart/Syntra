#!/usr/bin/env python3
"""Syntra: TLS Gateway — the appliance behind a REAL TLS reverse proxy.

A plain-HTTP syntra backend (fresh temp store) sits behind a stdlib TLS-
terminating reverse proxy on 127.0.0.1. A self-signed CA (leaf acting as
its own CA, SAN DNS:localhost + IP:127.0.0.1) is generated with the
openssl CLI. The demo proves, over the network: real certificate
verification (right CA passes, wrong CA is rejected, hostname mismatch is
rejected), TLS-only exposure (plain HTTP to the proxy port dies at the
handshake), header forwarding (missing admin key -> 401 THROUGH the proxy,
Bearer key -> 200), and a full decide + reward round-trip through the
terminator, with a feature program installed over TLS.

Run:  python3 scripts/demo-tls-gateway.py
Env:  SYNTRA_BIN / LYCAN_BIN pick the binaries (default: the release build,
      built if missing). KEEP_DEMO_OUTPUT=1 keeps the temp dir.
"""
import http.client
import json
import os
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

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

OPENSSL = shutil.which("openssl")
if not OPENSSL:
    print("  FAIL: openssl CLI not found on PATH")
    sys.exit(1)

BASE = tempfile.mkdtemp(prefix="syntra-tls-gateway.")
STORE = os.path.join(BASE, "store")
KEY = "tls-demo-key"
TENANT, JOB, CAPSULE_ID = "t1", "fleet", "tlsgw"
CAPSULE_PATH = f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}"

PASS, FAIL = 0, 0


def check(label, cond, detail=""):
    global PASS, FAIL
    mark = "PASS" if cond else "FAIL"
    PASS, FAIL = PASS + bool(cond), FAIL + (not cond)
    print(f"  {mark}: {label}" + (f"  [{detail}]" if detail else ""))
    return cond


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


PORT = free_port()
PROXY_PORT = free_port()
while PROXY_PORT == PORT:
    PROXY_PORT = free_port()
ADDR = f"127.0.0.1:{PORT}"


# ── certificates: openssl CLI, self-signed leaf acting as its own CA ─────
def openssl_run(args):
    # capture_output: openssl stdout/stderr (which can echo subject lines,
    # never key material) is never printed. Private keys are only ever
    # written to files by openssl itself.
    subprocess.run([OPENSSL, *args], check=True, capture_output=True)


def make_identity(name, cn):
    key = os.path.join(BASE, f"{name}.key")
    crt = os.path.join(BASE, f"{name}.crt")
    openssl_run(["genrsa", "-out", key, "2048"])
    openssl_run(["req", "-x509", "-new", "-key", key, "-sha256", "-days", "2",
                 "-subj", f"/CN={cn}",
                 "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
                 "-addext", "basicConstraints=critical,CA:TRUE",
                 "-out", crt])
    return key, crt


t0 = time.time()
CA_KEY, CA_CRT = make_identity("gateway", "localhost")          # trusted demo CA
BAD_KEY, BAD_CRT = make_identity("rogue-ca", "rogue.invalid")   # wrong CA, never installed
CERT_SECONDS = time.time() - t0

# ── capsule: a request classifier with three actions ────────────────────
# The spec declares the actions; Syntra learns which one each request class
# deserves. The feature program only derives features and a reason.
SPEC = {
    "actions": [{"id": "allow"}, {"id": "review"}, {"id": "block"}],
    "reward": {"range": [-0.5, 1.0]},
}
PROGRAM = r"""
;; TLS-GATEWAY DEMO feature program: derived risk features for the request class.
($ raw_cls (!cap "runtime.inputGet" "cls"))
($ cls (? (!= raw_cls null) raw_cls "read"))
(!cap "runtime.publish" "features.write" (? (== cls "write") 1.0 0.0))
(!cap "runtime.publish" "features.exec" (? (== cls "exec") 1.0 0.0))
(!cap "runtime.publish" "reason" (+ "class " cls))
"""
SRC = os.path.join(BASE, "tlsgw.lycs")
with open(SRC, "w") as f:
    f.write(PROGRAM)
subprocess.run([LYCAN, "compile", SRC], check=True, capture_output=True)
LYC = SRC[:-len(".lycs")] + ".lyc"

# ── backend ─────────────────────────────────────────────────────────────
proc = None


def start_backend():
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
    raise RuntimeError("backend did not become ready within 10s")


# ── TLS-terminating reverse proxy (stdlib only) ─────────────────────────
_STRIP_REQ = {"connection", "keep-alive", "proxy-authenticate",
              "proxy-authorization", "te", "trailer", "trailers",
              "transfer-encoding", "upgrade", "host", "content-length"}
_STRIP_RES = _STRIP_REQ - {"host"}


class TLSTerminator(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _forward(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else None
        headers = {k: v for k, v in self.headers.items()
                   if k.lower() not in _STRIP_REQ}
        try:
            up = http.client.HTTPConnection("127.0.0.1", PORT, timeout=30)
            up.request(self.command, self.path, body=body, headers=headers)
            res = up.getresponse()
            payload = res.read()
            status, reason = res.status, res.reason
            resp_headers = res.getheaders()
            up.close()
        except OSError as e:
            payload = json.dumps({"error": f"upstream unavailable: {e}"}).encode()
            status, reason, resp_headers = 502, "Bad Gateway", []
        self.send_response(status, reason)
        for k, v in resp_headers:
            if k.lower() in _STRIP_RES:
                continue
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = do_HEAD = _forward

    def log_message(self, fmt, *args):  # keep the demo transcript clean
        pass

    def handle_error(self, request, client_address):  # expected: failed TLS handshakes
        pass


proxy = None


def start_proxy():
    global proxy
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(certfile=CA_CRT, keyfile=CA_KEY)
    proxy = ThreadingHTTPServer(("127.0.0.1", PROXY_PORT), TLSTerminator)
    proxy.daemon_threads = True
    proxy.socket = ctx.wrap_socket(proxy.socket, server_side=True)
    threading.Thread(target=proxy.serve_forever, kwargs={"poll_interval": 0.2},
                     daemon=True).start()


def https_conn(host="localhost", cafile=CA_CRT):
    ctx = ssl.create_default_context(cafile=cafile)
    return http.client.HTTPSConnection(host, PROXY_PORT, context=ctx, timeout=15)


def api(method, path, body=None, token=KEY, con=None):
    """One call through the TLS proxy; returns (status, parsed-json-or-bytes)."""
    own = con is None
    con = con or https_conn()
    hdrs = {"Content-Type": "application/json"}
    if token is not None:
        hdrs["Authorization"] = f"Bearer {token}"
    if isinstance(body, (bytes, bytearray)):
        hdrs["Content-Type"] = "application/octet-stream"
    con.request(method, path, body=None if body is None else
                (bytes(body) if isinstance(body, (bytes, bytearray)) else json.dumps(body)),
                headers=hdrs)
    r = con.getresponse()
    data = r.read()
    if own:
        con.close()
    try:
        return r.status, json.loads(data)
    except ValueError:
        return r.status, data


def cleanup():
    if proxy is not None:
        try:
            proxy.shutdown()
            proxy.server_close()
        except OSError:
            pass
    if proc is not None:
        try:
            proc.terminate()
            proc.wait(timeout=10)
        except Exception:
            proc.kill()
    if os.environ.get("KEEP_DEMO_OUTPUT") == "1":
        print(f"  output kept at: {BASE}")
    else:
        shutil.rmtree(BASE, ignore_errors=True)


T0 = time.time()
try:
    print()
    print("  Syntra: TLS Gateway")
    print("  -------------------")
    print(f"  plain-HTTP appliance behind a stdlib TLS reverse proxy on "
          f"127.0.0.1:{PROXY_PORT} (backend :{PORT})")
    print(f"  certs: RSA-2048/SHA-256 self-signed CA, SAN DNS:localhost,IP:127.0.0.1 "
          f"(made in {CERT_SECONDS:.2f}s; key material never printed)")
    print()

    start_backend()
    start_proxy()

    # ── a. backend healthy over plain loopback ────────────────────────────
    plain = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
    plain.request("GET", "/health")
    a_status = plain.getresponse().status
    plain.close()
    check("(a) backend healthy over plain loopback", a_status == 200,
          f"GET :{PORT}/health -> {a_status}")

    # ── bootstrap THROUGH the proxy (proves PUT/POST and binary forwarding)
    s_job, _ = api("POST", f"/v1/tenants/{TENANT}/jobs", {"id": JOB, "name": "TLS Demo Fleet"})
    s_spec, _ = api("PUT", f"{CAPSULE_PATH}/spec", SPEC)
    with open(LYC, "rb") as f:
        s_install, installed = api("POST", f"{CAPSULE_PATH}/install", f.read())
    s_capsule, capsule = api("GET", CAPSULE_PATH)
    installed_ok = (s_job == 201 and s_spec == 201 and s_install == 200 and s_capsule == 200
                    and isinstance(capsule, dict)
                    and (capsule.get("program") or {}).get("programSha256")
                    == installed.get("programSha256"))

    # ── b. TLS decide + reward round-trip through the real decision path ──
    CLS = ["read", "read", "write", "exec"]
    n = 24
    ok_dec = ok_rew = 0
    actions, reasons = set(), set()
    con_b = https_conn()
    for i in range(n):
        cls = CLS[i % len(CLS)]
        s, d = api("POST", f"{CAPSULE_PATH}/decide", {"context": {"cls": cls}}, con=con_b)
        if s == 200 and d.get("decisionId") and d.get("action") and 0 < d.get("probability", 0) <= 1:
            ok_dec += 1
            actions.add(d["action"])
            reasons.add(d.get("reason"))
            # reward: hold (review/block) on write|exec, allow on read
            want_hold = cls in ("write", "exec")
            rew = 1.0 if (d["action"] != "allow") == want_hold else -0.5
            # The last reward waits for the commit, so (h) reads a complete log.
            s2, r = api("POST", f"{CAPSULE_PATH}/reward",
                        {"decisionId": d["decisionId"], "reward": rew,
                         "durable": i == n - 1}, con=con_b)
            ok_rew += s2 == 200 and r.get("applied") is True
    con_b.close()
    check("(b) decide + reward round-trip over TLS through the terminator",
          installed_ok and ok_dec == n and ok_rew == n
          and actions <= {"allow", "review", "block"}
          and reasons == {"class read", "class write", "class exec"},
          f"{ok_dec}/{n} decisions, {ok_rew}/{n} rewards applied, actions {sorted(actions)}, "
          f"feature program installed over TLS")

    # ── c. verification SUCCEEDS with the demo CA; TLS >= 1.2, cipher reported
    con_c = https_conn()
    con_c.request("GET", "/health", headers={"Authorization": f"Bearer {KEY}"})
    r_c = con_c.getresponse()
    c_status = r_c.status
    r_c.read()
    ver = con_c.sock.version()
    ver_num = float(ver.replace("TLSv", "").replace("SSLv", "")) if ver else 0.0
    cipher = con_c.sock.cipher()
    con_c.close()
    check("(c) demo CA verifies; negotiated TLS >= 1.2 with a non-null cipher",
          c_status == 200 and ver_num >= 1.2 and cipher and cipher[0],
          f"{ver}, cipher {cipher[0] if cipher else None}, /health via TLS -> {c_status}")

    # ── d. WRONG CA: handshake must fail with a cert-verification error ───
    d_err = None
    try:
        raw = socket.create_connection(("127.0.0.1", PROXY_PORT), timeout=10)
        ssl.create_default_context(cafile=BAD_CRT).wrap_socket(
            raw, server_hostname="localhost")
    except ssl.SSLError as e:
        d_err = e
    check("(d) a second, unrelated CA is REJECTED — verification is real",
          d_err is not None and isinstance(d_err, ssl.SSLCertVerificationError)
          and "certificate verify failed" in str(d_err).lower(),
          type(d_err).__name__ if d_err else "handshake unexpectedly succeeded")

    # ── e. hostname mismatch: trusted CA, bogus server_hostname ───────────
    e_err = None
    try:
        ctx_e = ssl.create_default_context(cafile=CA_CRT)
        ctx_e.check_hostname = True
        raw = socket.create_connection(("127.0.0.1", PROXY_PORT), timeout=10)
        ctx_e.wrap_socket(raw, server_hostname="evil.test")
    except ssl.SSLError as e:
        e_err = e
    check("(e) hostname mismatch rejected (same CA, server_hostname='evil.test')",
          e_err is not None and "mismatch" in str(e_err).lower(),
          f"{type(e_err).__name__}: {str(e_err)[:70]}" if e_err
          else "hostname not enforced")

    # ── f. missing admin key -> 401 THROUGH the proxy (header forwarding) ─
    s_noauth, _ = api("GET", "/v1/tenants", token=None)
    s_auth, _ = api("GET", "/v1/tenants", token=KEY)
    check("(f) missing admin key -> 401 through the proxy; Bearer key -> 200",
          s_noauth == 401 and s_auth == 200,
          f"no-auth {s_noauth}, with Bearer key {s_auth}")

    # ── g. plain HTTP to the TLS port must not get an HTTP answer ────────
    g_err, g_status = None, None
    try:
        cg = http.client.HTTPConnection("127.0.0.1", PROXY_PORT, timeout=5)
        cg.request("GET", "/health")
        g_status = cg.getresponse().status
        cg.close()
    except Exception as e:
        g_err = e
    check("(g) plain HTTP to the proxy port dies at the TLS handshake (TLS-only)",
          g_status is None and isinstance(g_err, Exception),
          f"{type(g_err).__name__}: {str(g_err)[:60]}" if g_err
          else f"plaintext got HTTP {g_status}")

    # ── h. decision log and audit trail reachable through the proxy ──────
    s_dec, dec = api("GET", f"{CAPSULE_PATH}/decisions?limit=1000")
    logged = dec.get("decisions", []) if isinstance(dec, dict) else []
    rewarded = 0
    if logged:
        s_one, one = api("GET", f"{CAPSULE_PATH}/decisions/{logged[0]['decisionId']}")
        rewarded = len(one.get("rewards", [])) if s_one == 200 else 0
    s_aud, aud = api("GET", f"{CAPSULE_PATH}/audits")
    events = [a.get("event") for a in aud.get("audits", [])] if isinstance(aud, dict) else []
    check("(h) decision log (with propensities and rewards) + audit trail through the proxy",
          s_dec == 200 and len(logged) == n and all(0 < x["probability"] <= 1 for x in logged)
          and rewarded == 1 and s_aud == 200
          and {"capsule_created", "program_installed"} <= set(events),
          f"decisions {s_dec} ({len(logged)} logged, first has {rewarded} reward), "
          f"audits {s_aud} {events}")

    print()
    print("  NOTE: demo-grade stdlib TLS terminator; production uses")
    print("  nginx/envoy/stunnel — the point proven is end-to-end certificate")
    print("  verification, header forwarding, and TLS-only exposure.")
    print(f"  (wall time {time.time() - T0:.1f}s)")
    print()
    print(f"  SCORE: {PASS}/{PASS + FAIL} checks passed")
    sys.exit(1 if FAIL else 0)
finally:
    cleanup()
