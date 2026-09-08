#!/usr/bin/env python3
"""Live adaptation dashboard for the Syntra live demo.

Serves a same-origin page and proxies /api/* to the Syntra instance with
the admin key injected server-side — the key never reaches the browser.

The page drives a real learning loop:
  decide -> delayed feedback (reward 1.0 if the chosen arm matches the
  current best, else 0.0) -> watch the graph weights converge.
  "Flip best arm" is the regime change: the policy has to re-adapt.

Usage:
  demo-live.py --api http://127.0.0.1:PORT --key KEY [--port 8901]
"""
import argparse
import json
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGE = """<!doctype html>
<html><head><meta charset="utf-8"><title>Syntra — Live Adaptation</title>
<style>
  :root { --bg:#0b0e14; --panel:#11151f; --line:#1e2533; --txt:#d7e0ee;
          --dim:#5c6b84; --a:#4fc3f7; --b:#ffb74d; --c:#81c784; --acc:#ff5252; }
  * { box-sizing:border-box; margin:0; padding:0; }
  body { background:var(--bg); color:var(--txt); font:14px/1.5 "SF Mono",Menlo,monospace;
         padding:24px; }
  h1 { font-size:18px; letter-spacing:2px; color:var(--txt); }
  h1 span { color:var(--dim); font-weight:400; }
  .row { display:flex; gap:16px; margin-top:16px; flex-wrap:wrap; }
  .card { background:var(--panel); border:1px solid var(--line); border-radius:8px;
          padding:14px 16px; }
  .chart-card { flex:1 1 640px; min-width:320px; }
  .stats { flex:0 0 280px; display:flex; flex-direction:column; gap:10px; }
  .stat { display:flex; justify-content:space-between; border-bottom:1px solid var(--line);
          padding:6px 0; }
  .stat b { font-size:16px; color:var(--txt); }
  .stat span { color:var(--dim); }
  canvas { width:100%; height:300px; display:block; }
  .legend { display:flex; gap:18px; margin-top:8px; color:var(--dim); font-size:12px; }
  .dot { display:inline-block; width:10px; height:10px; border-radius:50%; margin-right:6px; }
  button { background:#1a2233; color:var(--txt); border:1px solid var(--line);
           border-radius:6px; padding:10px 14px; font:inherit; cursor:pointer;
           letter-spacing:1px; }
  button:hover { border-color:var(--acc); color:var(--acc); }
  .badge { display:inline-block; padding:2px 8px; border-radius:10px; font-size:12px;
           background:#1a2233; color:var(--dim); }
  .badge.hot { background:#3a1a1a; color:var(--acc); }
  #status { color:var(--dim); font-size:12px; margin-top:10px; }
  .arm { display:inline-block; width:14px; height:14px; border-radius:3px;
         vertical-align:middle; margin-right:6px; }
</style></head><body>
<h1>SYNTRA <span>— LIVE ADAPTATION · watch the policy learn</span></h1>
<div class="row">
  <div class="card chart-card">
    <canvas id="cv" width="900" height="300"></canvas>
    <div class="legend">
      <span><span class="dot" style="background:var(--a)"></span>Arm A</span>
      <span><span class="dot" style="background:var(--b)"></span>Arm B</span>
      <span><span class="dot" style="background:var(--c)"></span>Arm C</span>
      <span style="margin-left:auto">chosen: <span id="chosen" class="badge">—</span></span>
      <span>best: <span id="best" class="badge hot">—</span></span>
    </div>
  </div>
  <div class="stats">
    <div class="card">
      <div class="stat"><span>decisions</span><b id="n">0</b></div>
      <div class="stat"><span>accuracy</span><b id="acc">—</b></div>
      <div class="stat"><span>warmup</span><b id="warm">—</b></div>
      <div class="stat"><span>best arm</span><b id="best2">—</b></div>
      <div class="stat"><span>last reward</span><b id="rw">—</b></div>
    </div>
    <div class="card">
      <button id="flip">FLIP BEST ARM (regime change)</button>
      <div style="margin-top:10px;color:var(--dim);font-size:12px">
        The runtime gets delayed feedback only — it never sees the ground
        truth. Flip the best arm and watch the persisted policy pivot.
      </div>
    </div>
  </div>
</div>
<div id="status">connecting…</div>
<script>
const API = "/api/tenants/__TENANT__/jobs/__JOB__/capsules/__CAPSULE__";
const N = 3, MAXP = 140;
const colors = ["var(--a)","var(--b)","var(--c)"];
const names = ["A","B","C"];
let series = [[],[],[]];
let best = 2, n = 0, correct = 0, lastReward = null;
const cv = document.getElementById("cv"), ctx = cv.getContext("2d");

async function api(method, path, body) {
  const r = await fetch(API + path, { method,
    headers: {"Content-Type":"application/json"},
    body: body ? JSON.stringify(body) : undefined });
  return r.json();
}
function css(v){ return getComputedStyle(document.documentElement).getPropertyValue(v).trim(); }

async function tick() {
  try {
    const d = await api("POST", "/decide", {});
    const chosen = d.decisions[0].chosen_option;
    const reward = chosen === best ? 1.0 : 0.0;
    await api("POST", "/feedback", { decisionId: d.decisionId, reward });
    n++; if (reward === 1.0) correct++;
    lastReward = reward;
    document.getElementById("chosen").textContent = names[chosen];
    document.getElementById("chosen").className = "badge" +
      (chosen === best ? " hot" : "");
    document.getElementById("n").textContent = n;
    document.getElementById("acc").textContent =
      n ? (100*correct/n).toFixed(1)+"%" : "—";
    document.getElementById("rw").textContent = reward;
    document.getElementById("warm").textContent =
      (d.warmup && d.warmup.state) || "active";
  } catch (e) { /* keep looping */ }
}

async function refresh() {
  try {
    const rep = await api("GET", "/report");
    const w = (rep.strategies[0].graphWeights) || [];
    for (let i = 0; i < N; i++) {
      series[i].push(w[i] ?? 0);
      if (series[i].length > MAXP) series[i].shift();
    }
    draw();
    document.getElementById("best2").textContent = names[best];
    document.getElementById("best").textContent = names[best];
    document.getElementById("status").textContent =
      "live · polling /decide + /feedback · policy weights from /report";
  } catch (e) {
    document.getElementById("status").textContent = "waiting for server…";
  }
}

function draw() {
  const W = cv.width, H = cv.height, pad = 34;
  ctx.clearRect(0,0,W,H);
  // grid
  ctx.strokeStyle = css("--line"); ctx.fillStyle = css("--dim");
  ctx.font = "11px monospace"; ctx.lineWidth = 1;
  for (let g = 0; g <= 4; g++) {
    const y = pad + (H-2*pad) * (1 - g/4);
    ctx.beginPath(); ctx.moveTo(pad,y); ctx.lineTo(W-8,y); ctx.stroke();
    ctx.fillText((g/4).toFixed(2), 4, y+4);
  }
  const npts = Math.max(...series.map(s=>s.length));
  if (npts < 2) return;
  const x0 = pad, x1 = W-8;
  for (let i = 0; i < N; i++) {
    ctx.strokeStyle = css(colors[i]); ctx.lineWidth = 2.5;
    ctx.beginPath();
    series[i].forEach((v, k) => {
      const x = x0 + (x1-x0) * k/(MAXP-1);
      const y = pad + (H-2*pad) * (1 - v);
      k ? ctx.lineTo(x,y) : ctx.moveTo(x,y);
    });
    ctx.stroke();
  }
  // last chosen marker
  const last = series.map(s=>s[s.length-1] ?? 0);
  const bestIdx = last.indexOf(Math.max(...last));
  ctx.fillStyle = css(colors[bestIdx]);
  ctx.beginPath();
  ctx.arc(x1, pad + (H-2*pad)*(1-last[bestIdx]), 6, 0, 7);
  ctx.fill();
}

document.getElementById("flip").onclick = () => {
  best = best === 2 ? 0 : 2;   // regime change: A <-> C
  document.getElementById("best").textContent = names[best];
  document.getElementById("best2").textContent = names[best];
};

setInterval(tick, 900);
setInterval(refresh, 1000);
refresh(); tick();
</script></body></html>
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--api", required=True, help="Syntra base URL, e.g. http://127.0.0.1:9600")
    ap.add_argument("--key", required=True, help="admin key (never sent to the browser)")
    ap.add_argument("--tenant", default="demo")
    ap.add_argument("--job", default="live")
    ap.add_argument("--capsule", default="bandit")
    ap.add_argument("--port", type=int, default=8901)
    args = ap.parse_args()

    page = (PAGE.replace("__TENANT__", args.tenant)
                .replace("__JOB__", args.job)
                .replace("__CAPSULE__", args.capsule))

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
                return self._send(200, "text/html", page.encode())
            if self.path.startswith("/api/"):
                return self.proxy("GET", self.path[5:], None)
            return self._send(404, "application/json", b'{"error":"not found"}')

        def do_POST(self):
            if self.path.startswith("/api/"):
                n = int(self.headers.get("Content-Length", 0) or 0)
                body = self.rfile.read(n) if n else b"{}"
                return self.proxy("POST", self.path[5:], body)
            return self._send(404, "application/json", b'{"error":"not found"}')

        def proxy(self, method, path, body):
            req = urllib.request.Request(
                f"{args.api.rstrip('/')}/{path}", data=body, method=method)
            req.add_header("Authorization", f"Bearer {args.key}")
            req.add_header("Content-Type", "application/json")
            try:
                with urllib.request.urlopen(req, timeout=10) as r:
                    self._send(r.status,
                               r.headers.get("Content-Type", "application/json"),
                               r.read())
            except urllib.error.HTTPError as e:
                self._send(e.code, "application/json", e.read())
            except Exception as e:  # noqa: BLE001 - surface proxy errors to the page
                self._send(502, "application/json",
                           json.dumps({"error": str(e)}).encode())

    srv = ThreadingHTTPServer(("127.0.0.1", args.port), H)
    print(f"live dashboard: http://127.0.0.1:{args.port}")
    srv.serve_forever()


if __name__ == "__main__":
    main()
