#!/usr/bin/env python3
"""Syntra: TLS Gateway — the appliance behind a REAL TLS reverse proxy.

A plain-HTTP lycan backend (fresh temp store) sits behind a stdlib TLS-
terminating reverse proxy on 127.0.0.1. A self-signed CA (leaf acting as
its own CA, SAN DNS:localhost + IP:127.0.0.1) is generated with the
openssl CLI. The demo proves, over the network: real certificate
verification (right CA passes, wrong CA is rejected, hostname mismatch is
rejected), TLS-only exposure (plain HTTP to the proxy port dies at the
handshake), header forwarding (missing admin key -> 401 THROUGH the proxy,
Bearer key -> 200), and a full decide+feedback round-trip through the
terminator.

Run:  python3 scripts/demo-tls-gateway.py
Env:  KEEP_DEMO_OUTPUT=1 keeps the temp dir and prints its path.
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
LYCAN = os.path.join(ROOT, "target", "release", "lycan")
if not os.path.exists(LYCAN):
    print("  FAIL: target/release/lycan not found — build it first")
    sys.exit(1)

OPENSSL = shutil.which("openssl")
if not OPENSSL:
    print("  FAIL: openssl CLI not found on PATH")
    sys.exit(1)

BASE = tempfile.mkdtemp(prefix="syntra-tls-gateway.", dir=os.environ.get("TMPDIR", "/tmp"))
STORE = os.path.join(BASE, "store")
KEY = "tls-demo-key"
TENANT, JOB, CAPSULE_ID = "t1", "fleet", "tlsgw"

PASS, FAIL = 0, 0
def check(label, cond, detail=""):
    global PASS, FAIL
    mark = "PASS" if cond else "FAIL"
    PASS, FAIL = PASS + bool(cond), FAIL + (not cond)
    print(f"  {mark}: {label}" + (f"  [{detail}]" if detail else ""))
    return cond

def pick_port_pair():
    """Backend on p, proxy on p+1; 9700..10000 to stay clear of the other
    demos (governor 12000-12499, sandbox 9300-9399). Bind-probe first."""
    for _ in range(64):
        base = 9700 + int.from_bytes(os.urandom(2), "big") % 300
        probes = []
        try:
            for p in (base, base + 1):
                s = socket.socket()
                s.bind(("127.0.0.1", p))
                probes.append(s)
            return base
        except OSError:
            continue
        finally:
            for s in probes:
                s.close()
    raise RuntimeError("no free port pair in 9700..10000")

PORT = pick_port_pair()
PROXY_PORT = PORT + 1
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

# ── capsule: tiny 3-option classifier on the governor's policy surface ───
CAPSULE = r"""
;; TLS-GATEWAY DEMO — request classifier. Options: 0 allow | 1 review | 2 block
($ raw_cls (!cap "runtime.inputGet" "cls"))
($ cls (? (!= raw_cls null) raw_cls "read"))
($ w (? (== cls "write") 1.0 0.0))
($ x (? (== cls "exec") 1.0 0.0))
(F s_allow () (- 90.0 (+ (* w 30.0) (* x 65.0))))
(F s_review () (- 70.0 (* x 25.0)))
(F s_block () (+ 10.0 (+ (* w 30.0) (* x 75.0))))
($ static_best (? (> (s_block) (s_allow)) (? (> (s_block) (s_review)) 2 (? (> (s_review) (s_allow)) 1 0)) (? (> (s_review) (s_allow)) 1 0)))
($ decision (choice 0 1 2))
(!p "TLSGW cls:" cls "static_best:" static_best "ACTION:" decision)
decision
"""
SRC = os.path.join(BASE, "tlsgw.lycs")
with open(SRC, "w") as f:
    f.write(CAPSULE)
subprocess.run([LYCAN, "compile", SRC], check=True, capture_output=True)
LYC = SRC[:-len(".lycs")] + ".lyc"

# ── backend: same serve command as the governor demo ────────────────────
proc = None
def start_backend():
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
            pass
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

    # ── bootstrap tenant/capsule THROUGH the proxy (also proves POST forwarding)
    api("POST", f"/v1/tenants/{TENANT}/jobs",
        {"id": JOB, "name": "TLS Demo Fleet", "description": "decide/feedback over TLS"})
    with open(LYC, "rb") as f:
        s_install, _ = api("POST", f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}/install",
                           f.read())
    # capsule detail GET is not a route on this runtime; /memory is (as in the
    # governor demo) and proves the install landed and is queryable.
    s_install_ctl = api("GET", f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}/memory",
                        token=KEY)[0]

    # ── b. TLS decide+feedback round-trip with a real decision pipeline ───
    CLS = ["read", "read", "write", "exec"]
    n = 24
    ok_dec = ok_fb = 0
    actions = set()
    con_b = https_conn()
    for i in range(n):
        cls = CLS[i % len(CLS)]
        s, d = api("POST",
                   f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}/decide?learn=true",
                   {"contextKey": cls, "input": {"cls": cls}}, con=con_b)
        if s == 200 and d.get("ok") and d.get("decisionId") and d.get("decisions"):
            ok_dec += 1
            actions.add(int(d["result"]))
            # reward: hold/review on write|exec, allow on read — honest outcome model
            want_hold = cls in ("write", "exec")
            rew = 1.0 if (int(d["result"]) >= 1) == want_hold else -0.5
            s2, _ = api("POST", f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}/feedback",
                        {"decisionId": d["decisionId"], "reward": rew}, con=con_b)
            ok_fb += s2 == 200
    con_b.close()
    check("(b) decide+feedback round-trip over TLS through the terminator",
          ok_dec == n and ok_fb == n and s_install == 200 and s_install_ctl == 200
          and actions and actions <= {0, 1, 2},
          f"{ok_dec}/{n} decisions, {ok_fb}/{n} feedbacks, actions {sorted(actions)}")

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
    s_noauth, _ = api("GET", f"/v1/tenants", token=None)
    s_auth, _ = api("GET", f"/v1/tenants", token=KEY)
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

    # ── h. decisions/audits log reachable through the proxy ──────────────
    s_dec, dec_raw = api("GET", f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}/decisions")
    dec_lines = [l for l in dec_raw.decode(errors="replace").splitlines() if l.strip()] \
        if isinstance(dec_raw, (bytes, bytearray)) else []
    s_aud, _ = api("GET", f"/v1/tenants/{TENANT}/jobs/{JOB}/capsules/{CAPSULE_ID}/audits")
    check("(h) decisions + audits log routes reachable through the proxy",
          s_dec == 200 and len(dec_lines) >= n and s_aud == 200,
          f"decisions {s_dec} ({len(dec_lines)} lines), audits {s_aud}")

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
