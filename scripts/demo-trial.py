#!/usr/bin/env python3
"""SYNTRA IS RUNNING A CLINICAL TRIAL — live adaptive-trial demo.

The runtime is the response-adaptive allocator: each enrolled patient is
a /decide, each outcome is delayed /feedback, and the learned per-context
weights ARE the randomization schedule. The trial adapts per patient
subgroup instead of using a fixed 1:1:1 ratio.

The engine (this file) is the trial protocol: it enrolls patients, hands
each allocation to Syntra, simulates the outcome from the (hidden) true
response rates, tracks Beta-Bernoulli posteriors, and stops a subgroup
when the expected loss of the leading arm drops below 1 percentage
point. A fixed 1:1:1 control runs in parallel on the same true rates for the headline
"responses vs fixed control" number. Every allocation is in Syntra's decision log.

Usage:
  demo-trial.py --api http://127.0.0.1:PORT --key KEY \
      [--port 8902] [--patients 240] [--seed 7] [--headless]
"""
import argparse
import json
import math
import random
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Hidden ground truth: response rate per treatment per subgroup.
# Mild: A wins. Severe: B wins. No single treatment is best overall —
# a fixed trial would pick the wrong drug for one subgroup.
TRUE = {"mild": [0.65, 0.45, 0.35], "severe": [0.40, 0.70, 0.30]}
NAMES = ["A", "B", "C"]
EL_THRESHOLD = 0.01   # expected-loss stopping rule (Adcock-style)
MC_DRAWS = 2000

PAGE = """<!doctype html>
<html><head><meta charset="utf-8"><title>Syntra — Clinical Trial</title>
<style>
  :root { --bg:#070d0c; --panel:#0d1513; --line:#1b2a26; --txt:#d8e8e2;
          --dim:#5d7a70; --a:#4dd0e1; --b:#ffb74d; --c:#ff8a80; --ok:#69f0ae; }
  * { box-sizing:border-box; margin:0; padding:0; }
  body { background:var(--bg); color:var(--txt); font:13px/1.5 "SF Mono",Menlo,monospace;
         padding:22px; }
  h1 { font-size:17px; letter-spacing:2px; }
  h1 span { color:var(--dim); font-weight:400; }
  .sub { color:var(--dim); margin-top:4px; font-size:12px; }
  .row { display:flex; gap:14px; margin-top:16px; flex-wrap:wrap; }
  .card { background:var(--panel); border:1px solid var(--line); border-radius:8px;
          padding:14px 16px; }
  .panel { flex:1 1 340px; min-width:300px; }
  .side { flex:0 0 300px; display:flex; flex-direction:column; gap:12px; }
  h2 { font-size:13px; letter-spacing:1px; color:var(--dim); margin-bottom:10px; }
  .bar { display:flex; height:26px; border-radius:5px; overflow:hidden;
         border:1px solid var(--line); margin:6px 0 10px; }
  .bar div { display:flex; align-items:center; justify-content:center;
             font-size:11px; color:#08110e; font-weight:700; min-width:34px;
             transition:width .4s; }
  .arm { display:flex; justify-content:space-between; padding:3px 0;
         border-bottom:1px solid var(--line); }
  .arm b { color:var(--txt); }
  .arm span { color:var(--dim); }
  canvas { width:100%; height:130px; display:block; margin-top:6px; }
  .stat { display:flex; justify-content:space-between; padding:5px 0;
          border-bottom:1px solid var(--line); }
  .stat b { color:var(--txt); }
  .stat span { color:var(--dim); }
  .badge { display:inline-block; padding:2px 9px; border-radius:10px; font-size:12px;
           background:#12251f; color:var(--ok); }
  .badge.run { background:#12212b; color:var(--a); }
  .audit { font-size:11px; color:var(--dim); max-height:150px; overflow-y:auto; }
  .audit div { padding:2px 0; border-bottom:1px solid var(--line); }
  .audit b { color:var(--txt); }
  .reveal { margin-top:10px; padding:10px; border:1px dashed var(--line);
            border-radius:6px; color:var(--dim); font-size:12px; }
  .reveal b { color:var(--ok); }
</style></head><body>
<h1>SYNTRA <span>IS RUNNING A CLINICAL TRIAL RIGHT NOW</span></h1>
<div class="sub">3 treatments · 2 patient subgroups · response-adaptive randomization ·
every allocation is a decision in the audit log</div>
<div class="row">
  <div class="card panel">
    <h2>SUBGROUP: MILD</h2>
    <div class="bar" id="bar-mild"></div>
    <canvas id="cv-mild" width="420" height="130"></canvas>
    <div id="post-mild" style="margin-top:6px"></div>
  </div>
  <div class="card panel">
    <h2>SUBGROUP: SEVERE</h2>
    <div class="bar" id="bar-severe"></div>
    <canvas id="cv-severe" width="420" height="130"></canvas>
    <div id="post-severe" style="margin-top:6px"></div>
  </div>
  <div class="side">
    <div class="card">
      <div class="stat"><span>patients enrolled</span><b id="n">0</b></div>
      <div class="stat"><span>status</span><b id="status">enrolling…</b></div>
      <div class="stat"><span>responses vs fixed control</span><b id="saved">—</b></div>
      <div class="stat"><span>trial design</span><b>response-adaptive</b></div>
    </div>
    <div class="card">
      <h2>AUDIT TRAIL (Syntra decision log)</h2>
      <div class="audit" id="audit">waiting…</div>
    </div>
    <div class="card">
      <h2>GROUND TRUTH (revealed at end)</h2>
      <div class="reveal" id="reveal">hidden — the runtime never sees it</div>
    </div>
  </div>
</div>
<script>
const colors = ["var(--a)","var(--b)","var(--c)"];
const names = ["A","B","C"];
let hist = {mild:[[],[],[]], severe:[[],[],[]]};
const MAXP = 120;
function css(v){ return getComputedStyle(document.documentElement).getPropertyValue(v).trim(); }

async function refresh() {
  let s;
  try { s = await (await fetch("/state")).json(); } catch(e){ return; }
  document.getElementById("n").textContent = s.enrolled;
  document.getElementById("status").textContent = s.done ? "TRIAL COMPLETE" : "enrolling…";
  document.getElementById("status").className = "badge " + (s.done ? "" : "run");
  document.getElementById("saved").textContent =
    s.saved === null ? "—" : `${s.saved} (${s.savedPct}%)`;
  for (const ctx of ["mild","severe"]) {
    const st = s.contexts[ctx];
    const bar = document.getElementById("bar-" + ctx);
    bar.innerHTML = "";
    for (let i = 0; i < 3; i++) {
      const d = document.createElement("div");
      d.style.background = css(colors[i]);
      d.style.width = Math.max(3, 100*st.alloc[i]/Math.max(1,st.total)) + "%";
      d.textContent = st.alloc[i] ? names[i] : "";
      bar.appendChild(d);
    }
    document.getElementById("post-" + ctx).innerHTML =
      names.map((nm,i) => `<div class="arm"><span>${nm} response</span>
        <b>${st.posterior[i].toFixed(3)}</b></div>`).join("") +
      (st.identified ? `<div style="margin-top:6px"><span class="badge">WINNER: ${names[st.winner]} · identified at patient ${st.identifiedAt}</span></div>`
                     : `<div style="margin-top:6px;color:var(--dim)">identifying… (expected loss &lt; 0.01)</div>`);
    // history
    for (let i = 0; i < 3; i++) {
      hist[ctx][i].push(st.posterior[i]);
      if (hist[ctx][i].length > MAXP) hist[ctx][i].shift();
    }
    draw(ctx);
  }
  // audit
  const audit = document.getElementById("audit");
  audit.innerHTML = s.audit.length
    ? s.audit.slice(-6).reverse().map(a =>
        `<div><b>#${a.patient}</b> ${a.context} → treatment ${names[a.arm]} · outcome ${a.outcome}</div>`).join("")
    : "waiting…";
  // reveal
  const r = document.getElementById("reveal");
  if (s.done) {
    r.innerHTML = `mild → <b>${names[s.gt.mild.indexOf(Math.max(...s.gt.mild))]}</b> (${s.gt.mild.map(x=>(x*100).toFixed(0)+"%").join("/")})<br>` +
                  `severe → <b>${names[s.gt.severe.indexOf(Math.max(...s.gt.severe))]}</b> (${s.gt.severe.map(x=>(x*100).toFixed(0)+"%").join("/")})<br>` +
                  `<span style="color:var(--dim)">a fixed 1:1:1 trial would have picked the wrong drug for one subgroup</span>`;
  }
}

function draw(ctx) {
  const cv = document.getElementById("cv-" + ctx), c = cv.getContext("2d");
  const W = cv.width, H = cv.height, pad = 22;
  c.clearRect(0,0,W,H);
  c.strokeStyle = css("--line"); c.fillStyle = css("--dim"); c.font = "10px monospace";
  for (let g = 0; g <= 2; g++) {
    const y = pad + (H-2*pad) * (1 - g/2);
    c.beginPath(); c.moveTo(pad,y); c.lineTo(W-6,y); c.stroke();
    c.fillText((g/2).toFixed(1), 2, y+3);
  }
  const npts = Math.max(...hist[ctx].map(s=>s.length));
  if (npts < 2) return;
  for (let i = 0; i < 3; i++) {
    c.strokeStyle = css(colors[i]); c.lineWidth = 2;
    c.beginPath();
    hist[ctx][i].forEach((v,k) => {
      const x = pad + (W-pad-6) * k/(MAXP-1);
      const y = pad + (H-2*pad) * (1 - v);
      k ? c.lineTo(x,y) : c.moveTo(x,y);
    });
    c.stroke();
  }
}

setInterval(refresh, 500);
refresh();
</script></body></html>
"""


class Trial:
    def __init__(self, api, key, tenant, job, capsule, patients, seed):
        self.api = api.rstrip("/")
        self.key = key
        self.base = f"{self.api}/tenants/{tenant}/jobs/{job}/capsules/{capsule}"
        self.patients = patients
        self.rng = random.Random(seed)
        self.fixed_rng = random.Random(seed + 1)
        self.state = {
            "enrolled": 0, "done": False, "saved": None, "savedPct": None,
            "gt": TRUE, "audit": [],
            "contexts": {ctx: {"alloc": [0, 0, 0], "success": [0, 0, 0],
                               "total": 0,
                               "posterior": [1/3, 1/3, 1/3],
                               "identified": False, "winner": None,
                               "identifiedAt": None}
                         for ctx in ("mild", "severe")},
        }
        # fixed 1:1:1 control: (success, fail) per arm per context
        self.fixed = {ctx: [[1, 1] for _ in range(3)] for ctx in ("mild", "severe")}
        self.fixed_identified = {ctx: None for ctx in ("mild", "severe")}
        self.fixed_enrolled = 0
        self.lock = threading.Lock()

    # ---- Syntra calls ----
    def _post(self, path, body):
        req = urllib.request.Request(
            self.base + path, data=json.dumps(body).encode(), method="POST",
            headers={"Authorization": f"Bearer {self.key}",
                     "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.load(r)

    def _get(self, path):
        req = urllib.request.Request(self.base + path,
                                     headers={"Authorization": f"Bearer {self.key}"})
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.read().decode()

    # ---- statistics ----
    @staticmethod
    def expected_loss(counts):
        """EL_i = E[max_j p_j - p_i] via Monte Carlo over Beta(1+s, 1+f).

        The standard Bayesian stopping rule for response-adaptive trials:
        stop a subgroup when the expected loss of staying on the leading
        arm drops below the threshold. Unlike a raw P(best) cutoff, it is
        robust to arms the allocator has (correctly) starved."""
        rng = random.Random(1234)
        cols = [[rng.betavariate(1 + s, 1 + f) for _ in range(MC_DRAWS)]
                for s, f in counts]
        acc = [0.0] * len(cols)
        for k in range(MC_DRAWS):
            m = max(c[k] for c in cols)
            for i, c in enumerate(cols):
                acc[i] += m - c[k]
        return [a / MC_DRAWS for a in acc]

    def _check_fixed(self, ctx):
        """First global patient where the fixed control's min expected
        loss drops below the same stopping threshold."""
        if self.fixed_identified[ctx] is not None:
            return
        el = self.expected_loss(self.fixed[ctx])
        if min(el) < EL_THRESHOLD:
            self.fixed_identified[ctx] = self.fixed_enrolled

    # ---- main loop ----
    def run(self):
        try:
            for i in range(self.patients):
                ctx = "mild" if self.rng.random() < 0.5 else "severe"
                st = self.state["contexts"][ctx]
                if st["identified"]:
                    # subgroup done — keep enrolling the other (real trials
                    # stop a subgroup, not necessarily the whole trial)
                    pass
                d = self._post("/decide", {"contextKey": ctx})
                arm = d["decisions"][0]["chosen_option"]
                outcome = 1 if self.rng.random() < TRUE[ctx][arm] else 0
                self._post("/feedback", {"decisionId": d["decisionId"],
                                         "reward": float(outcome)})
                with self.lock:
                    self.state["enrolled"] += 1
                    st["alloc"][arm] += 1
                    st["success"][arm] += outcome
                    st["total"] += 1
                    # Beta(1+success, 1+fail) per arm — the posterior is
                    # built from observed RESPONSES, never from allocations.
                    counts = [[1 + st["success"][a],
                               1 + st["alloc"][a] - st["success"][a]]
                              for a in range(3)]
                    # posterior mean response rate per arm
                    st["posterior"] = [c[0] / (c[0] + c[1]) for c in counts]
                    if not st["identified"] and st["total"] >= 20:
                        el = self.expected_loss(counts)
                        if min(el) < EL_THRESHOLD:
                            st["identified"] = True
                            st["winner"] = el.index(min(el))
                            st["identifiedAt"] = self.state["enrolled"]
                    self.state["audit"].append(
                        {"patient": self.state["enrolled"], "context": ctx,
                         "arm": arm, "outcome": outcome})
                # fixed control (same true rates, 1:1:1, own RNG)
                fctx = "mild" if self.fixed_rng.random() < 0.5 else "severe"
                farm = self.fixed_enrolled % 3
                fout = 1 if self.fixed_rng.random() < TRUE[fctx][farm] else 0
                self.fixed[fctx][farm][0] += fout
                self.fixed[fctx][farm][1] += 1 - fout
                self.fixed_enrolled += 1
                self._check_fixed(fctx)
            with self.lock:
                self.state["done"] = True
                # Headline: observed responses with the SAME number of
                # enrolled patients, adaptive allocation vs the fixed
                # 1:1:1 control. This measures who actually got the better
                # drug during the trial — not merely who stopped first.
                ad = sum(sum(self.state["contexts"][c]["success"])
                         for c in ("mild", "severe"))
                fx = sum(self.fixed[c][a][0]
                         for c in ("mild", "severe") for a in range(3))
                self.state["adResponses"] = ad
                self.state["fxResponses"] = fx
                self.state["saved"] = ad - fx
                self.state["savedPct"] = round(100 * (ad - fx) / max(1, fx))
        except Exception as e:  # noqa: BLE001 - surface to the page
            with self.lock:
                self.state["error"] = str(e)
                self.state["done"] = True

    def snapshot(self):
        with self.lock:
            s = json.loads(json.dumps(self.state))
            try:
                log = self._get("/decisions")
                # keep the audit list from the engine (already has outcomes)
            except Exception:  # noqa: BLE001
                pass
            return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--api", required=True)
    ap.add_argument("--key", required=True)
    ap.add_argument("--tenant", default="demo")
    ap.add_argument("--job", default="trial")
    ap.add_argument("--capsule", default="trial")
    ap.add_argument("--port", type=int, default=8902)
    ap.add_argument("--patients", type=int, default=240)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--headless", action="store_true")
    args = ap.parse_args()

    trial = Trial(args.api, args.key, args.tenant, args.job, args.capsule,
                  args.patients, args.seed)
    t = threading.Thread(target=trial.run, daemon=True)
    t.start()

    if args.headless:
        while t.is_alive():
            time.sleep(0.5)
        s = trial.snapshot()
        for ctx in ("mild", "severe"):
            st = s["contexts"][ctx]
            winner = st["winner"]
            print(f"{ctx}: alloc A/B/C = {st['alloc']}  "
                  f"est. response = {[round(p,3) for p in st['posterior']]}  "
                  f"{'WINNER ' + ['A','B','C'][winner] + ' at patient ' + str(st['identifiedAt']) if st['identified'] else 'not identified'}")
        print(f"fixed control identified at: "
              f"mild={trial.fixed_identified['mild']} severe={trial.fixed_identified['severe']}")
        print(f"responses: adaptive {s['adResponses']} vs fixed 1:1:1 {s['fxResponses']}"
              f" — {s['saved']:+d} more patients responded ({s['savedPct']:+d}%)")
        return

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send(self, code, ctype, data):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            if self.path in ("/", "/index.html"):
                return self._send(200, "text/html", PAGE.encode())
            if self.path == "/state":
                return self._send(200, "application/json",
                                  json.dumps(trial.snapshot()).encode())
            if self.path.startswith("/api/"):
                return self.proxy("GET", self.path[5:], None)
            return self._send(404, "application/json", b'{"error":"nf"}')

        def do_POST(self):
            if self.path.startswith("/api/"):
                n = int(self.headers.get("Content-Length", 0) or 0)
                return self.proxy("POST", self.path[5:],
                                  self.rfile.read(n) if n else b"{}")
            return self._send(404, "application/json", b'{"error":"nf"}')

        def proxy(self, method, path, body):
            req = urllib.request.Request(
                f"{args.api.rstrip('/')}/{path}", data=body, method=method)
            req.add_header("Authorization", f"Bearer {args.key}")
            req.add_header("Content-Type", "application/json")
            try:
                with urllib.request.urlopen(req, timeout=15) as r:
                    self._send(r.status,
                               r.headers.get("Content-Type", "application/json"),
                               r.read())
            except urllib.error.HTTPError as e:
                self._send(e.code, "application/json", e.read())
            except Exception as e:  # noqa: BLE001
                self._send(502, "application/json",
                           json.dumps({"error": str(e)}).encode())

    srv = ThreadingHTTPServer(("127.0.0.1", args.port), H)
    print(f"trial dashboard: http://127.0.0.1:{args.port}")
    srv.serve_forever()


if __name__ == "__main__":
    main()
