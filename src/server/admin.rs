//! The admin console: one static page. It holds no data itself; every
//! call goes through the authenticated API with the key the operator
//! enters, kept in the tab's session storage.

pub fn console_html(service_name: &str) -> String {
    CONSOLE.replace("{{SERVICE}}", &html_escape(service_name))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const CONSOLE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{SERVICE}} admin console</title>
<style>
:root { --bg:#fff; --fg:#1b1d22; --muted:#5d6470; --line:#e3e6eb; --accent:#2f5bd3; --bad:#b42318; }
@media (prefers-color-scheme: dark) { :root { --bg:#111317; --fg:#e8eaee; --muted:#9aa3b1; --line:#2a2e36; --accent:#8fb0ff; --bad:#ff8a80; } }
* { box-sizing: border-box; }
body { margin:0; font:14px/1.45 system-ui, sans-serif; background:var(--bg); color:var(--fg); }
header { display:flex; gap:12px; align-items:center; padding:12px 16px; border-bottom:1px solid var(--line); flex-wrap:wrap; }
header h1 { font-size:16px; margin:0 12px 0 0; }
input, button, select { font:inherit; padding:6px 8px; border:1px solid var(--line); border-radius:6px; background:var(--bg); color:var(--fg); }
button { cursor:pointer; }
main { display:grid; grid-template-columns: minmax(200px, 280px) 1fr; min-height: calc(100vh - 58px); }
@media (max-width: 720px) { main { grid-template-columns: 1fr; } }
nav { border-right:1px solid var(--line); padding:12px; overflow:auto; }
nav a { display:block; padding:6px 8px; border-radius:6px; color:var(--fg); text-decoration:none; word-break:break-all; }
nav a.active, nav a:hover { background:var(--line); }
section { padding:16px; overflow:auto; }
h2 { font-size:15px; margin:18px 0 8px; }
pre { background:var(--line); padding:10px; border-radius:6px; overflow:auto; max-height:320px; }
table { border-collapse:collapse; width:100%; }
th, td { text-align:left; padding:6px 8px; border-bottom:1px solid var(--line); font-variant-numeric: tabular-nums; }
.muted { color:var(--muted); } .err { color:var(--bad); }
</style>
</head>
<body>
<header>
  <h1>{{SERVICE}}</h1>
  <input id="key" type="password" placeholder="API key" autocomplete="off" size="28">
  <button id="connect">Connect</button>
  <span id="status" class="muted"></span>
</header>
<main>
  <nav id="capsules"><p class="muted">Enter a key to list capsules.</p></nav>
  <section id="detail"><p class="muted">Select a capsule.</p></section>
</main>
<script>
const $ = (id) => document.getElementById(id);
let key = sessionStorage.getItem("syntra-key") || "";
$("key").value = key;
async function api(path) {
  const r = await fetch("/v1" + path, { headers: { Authorization: "Bearer " + key } });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error || r.status);
  return body;
}
function esc(v) { return String(v).replace(/[&<>"]/g, c => ({ "&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;" }[c])); }
async function loadCapsules() {
  try {
    const { capsules } = await api("/admin/capsules");
    $("status").textContent = capsules.length + " capsule(s)";
    $("capsules").innerHTML = capsules.length ? "" : '<p class="muted">No capsules yet.</p>';
    for (const c of capsules) {
      const a = document.createElement("a");
      a.href = "#"; a.textContent = `${c.tenant}/${c.job}/${c.capsule}`;
      a.onclick = (e) => { e.preventDefault(); document.querySelectorAll("nav a").forEach(x => x.classList.remove("active")); a.classList.add("active"); show(c); };
      $("capsules").appendChild(a);
    }
  } catch (e) { $("status").innerHTML = '<span class="err">' + esc(e.message) + "</span>"; }
}
async function show(c) {
  const base = `/tenants/${c.tenant}/jobs/${c.job}/capsules/${c.capsule}`;
  $("detail").innerHTML = '<p class="muted">Loading...</p>';
  try {
    const [cap, dec, aud] = await Promise.all([api(base), api(base + "/decisions?limit=50"), api(base + "/audits?limit=20")]);
    const rows = dec.decisions.slice().reverse().map(d =>
      `<tr><td>${esc(new Date(d.tsMs).toISOString())}</td><td>${esc(d.action)}</td><td>${Number(d.probability).toFixed(3)}</td><td>${esc(d.mode)}</td><td>${esc(d.modelVersion)}</td></tr>`).join("");
    $("detail").innerHTML = `
      <h2>${esc(c.tenant)}/${esc(c.job)}/${esc(c.capsule)}</h2>
      <p>Model version <b>${esc(cap.modelVersion)}</b> · mode <b>${esc(cap.spec.mode)}</b>${cap.program ? " · feature program " + esc(cap.program.programSha256.slice(0, 12)) : ""}</p>
      <h2>Recent decisions</h2>
      <table><tr><th>Time</th><th>Action</th><th>Probability</th><th>Mode</th><th>Model</th></tr>${rows || '<tr><td colspan="5" class="muted">None yet.</td></tr>'}</table>
      <h2>Spec</h2><pre>${esc(JSON.stringify(cap.spec, null, 2))}</pre>
      <h2>Audit</h2><pre>${esc(JSON.stringify(aud.audits, null, 2))}</pre>`;
  } catch (e) { $("detail").innerHTML = '<p class="err">' + esc(e.message) + "</p>"; }
}
$("connect").onclick = () => { key = $("key").value.trim(); sessionStorage.setItem("syntra-key", key); loadCapsules(); };
if (key) loadCapsules();
</script>
</body>
</html>
"##;
