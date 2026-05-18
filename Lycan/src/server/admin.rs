use crate::graph::{Contract, NeuralGraph, OpCode};

use super::errors::{Resp, json_resp};
use super::state::SharedState;

pub(super) fn admin_html(service_name: &str) -> String {
    let service = escape_html(service_name);
    let initial = escape_html(&service_name.chars().next().unwrap_or('L').to_string());
    ADMIN_HTML
        .replace("__SERVICE_NAME__", &service)
        .replace("__SERVICE_INITIAL__", &initial)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Handler for `GET /admin/capsules`.
///
/// Walks every (tenant, job, capsule) tuple in the store and returns one
/// row per capsule containing:
///   * `path` — `{tenant}/{job}/{capsule}`
///   * `name` — friendly display name. Pulled from a `manifest.json`
///     `displayName`/`name` field if present, otherwise falls back to the
///     capsule directory name.
///   * `options` — option labels in graph order. The clean source is
///     `learning.json::sharedState.optionFeatures` (already a sorted
///     `BTreeMap`); otherwise we fall back to `option_0..option_{n-1}`
///     derived from the first AdaptiveChoice node's operand count in the
///     compiled `.lyc`. The `.lyc` binary does not preserve option labels,
///     so the placeholder is the honest answer for v1; the dashboard
///     work in Part 2c will overlay real labels when a sidecar ships them.
///   * `scoringMode` — `"shared-state-linucb"` when
///     `learning.json::sharedState.enabled` is true, else
///     `"meta-bandit"`.
///
/// Output is sorted by `path` for stable client-side rendering. A capsule
/// directory missing its `learning.json` is included with empty options
/// and the meta-bandit default — it does not 500 the endpoint.
pub(super) fn list_admin_capsules(state: &SharedState) -> Resp {
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for (tenant, job, capsule) in state.store.list_all_capsules() {
        let cfg = state.store.load_learning_config_in_job(&tenant, &job, &capsule);

        // Detect adaptive flavor. Order matters: hierarchical takes
        // precedence over shared-state which takes precedence over
        // flat meta-bandit. A capsule with a hierarchical_spec sidecar
        // has its option resolution come from the *tree*'s enumerated
        // leaves, not from the .lyc operand count.
        let hier_spec = state.store.load_hierarchical_spec_in_job(&tenant, &job, &capsule);

        let scoring_mode: &str = if hier_spec.is_some() {
            "hierarchical"
        } else if cfg.shared_state.enabled {
            "shared-state-linucb"
        } else {
            "meta-bandit"
        };

        // Option labels by flavor:
        //   * Hierarchical: leaf names from enumerate_paths().map(resolve_path).
        //     The tree carries real labels — no `option_0..N` fallback needed.
        //   * Shared-state: optionFeatures keys (sorted BTreeMap order).
        //   * Flat meta-bandit: option_0..option_{n-1} placeholder derived
        //     from the .lyc's first AdaptiveChoice operand count.
        //     `.lyc` doesn't preserve labels; the placeholder is honest.
        let options: Vec<String> = if let Some(ref h) = hier_spec {
            h.enumerate_paths()
                .iter()
                .filter_map(|p| h.resolve_path(p).map(|s| s.to_string()))
                .collect()
        } else if cfg.shared_state.enabled
            && !cfg.shared_state.option_features.is_empty()
        {
            cfg.shared_state.option_features.keys().cloned().collect()
        } else {
            match state.store.load_graph_in_job(&tenant, &job, &capsule) {
                Ok(bytes) => match NeuralGraph::from_bytes(&bytes) {
                    Ok(graph) => graph
                        .nodes
                        .iter()
                        .find(|n| matches!(n.op, OpCode::AdaptiveChoice))
                        .map(|node| {
                            let n_options = if node.contract == Contract::WithinTolerance
                                && node.weights.len() > 1
                            {
                                node.weights.len() - 1
                            } else {
                                node.weights.len()
                            };
                            (0..n_options).map(|i| format!("option_{i}")).collect()
                        })
                        .unwrap_or_default(),
                    Err(_) => Vec::new(),
                },
                Err(_) => Vec::new(),
            }
        };

        // Friendly name: prefer `displayName` in the install-time
        // manifest, then `name` (which install writes as the capsule
        // id today, but capsule authors may overwrite), then fall
        // back to the directory name.
        let manifest = state.store.read_manifest_in_job(&tenant, &job, &capsule);
        let friendly_name = manifest
            .as_ref()
            .and_then(|m| m.get("displayName").and_then(|v| v.as_str()).map(String::from))
            .or_else(|| {
                manifest
                    .as_ref()
                    .and_then(|m| m.get("name").and_then(|v| v.as_str()).map(String::from))
                    .filter(|n| n != &capsule)
            })
            .unwrap_or_else(|| capsule.clone());

        rows.push(serde_json::json!({
            "path": format!("{tenant}/{job}/{capsule}"),
            "name": friendly_name,
            "options": options,
            "scoringMode": scoring_mode,
        }));
    }

    rows.sort_by(|a, b| {
        a.get("path").and_then(|v| v.as_str()).unwrap_or("")
            .cmp(b.get("path").and_then(|v| v.as_str()).unwrap_or(""))
    });

    json_resp(200, &serde_json::json!({"capsules": rows}).to_string())
}

// ── Admin Console HTML ──

const ADMIN_HTML: &str = r##"
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>__SERVICE_NAME__ Console</title>
<style>
:root{
  --bg:#f5f6fa;--surface:#fff;--card:#fff;--card-hover:#f0f1f7;
  --border:#e0e2ee;--border-hover:#c8cade;
  --text:#1a1c2e;--muted:#6e7191;--dim:#a0a3bd;
  --peri:#6366f1;--peri-soft:#6366f114;--peri-glow:#6366f130;
  --green:#059669;--green-soft:#05966912;
  --red:#dc2626;--red-soft:#dc262612;
  --amber:#d97706;--amber-soft:#d9770612;
  --blue:#2563eb;
  --graph-bg:#0b0c10;
  --mono:'SF Mono','Cascadia Code','Fira Code','Consolas','Liberation Mono',monospace;
  --sans:-apple-system,BlinkMacSystemFont,'Segoe UI','Helvetica Neue',Arial,sans-serif;
  --radius:8px;--radius-lg:12px;
}
*{margin:0;padding:0;box-sizing:border-box}
html{font-size:13px}
body{font-family:var(--sans);background:var(--bg);color:var(--text);min-height:100vh;overflow:hidden}
button,input,select{font:inherit;color:inherit}
button{cursor:pointer;border:none;background:none}
::-webkit-scrollbar{width:5px}
::-webkit-scrollbar-track{background:transparent}
::-webkit-scrollbar-thumb{background:#c8cade;border-radius:3px}
::-webkit-scrollbar-thumb:hover{background:#a0a3bd}

/* Layout */
.app{display:grid;grid-template-columns:280px 1fr;height:100vh}

/* Sidebar */
.sidebar{background:#1a1c2e;border-right:1px solid #2a2d45;display:flex;flex-direction:column;overflow:hidden;color:#c8cade}
.sidebar-head{padding:20px 18px 16px;border-bottom:1px solid #2a2d45}
.logo-row{display:flex;align-items:center;gap:10px;margin-bottom:16px}
.logo-mark{width:32px;height:32px;background:var(--peri);border-radius:8px;display:flex;align-items:center;justify-content:center;font-weight:700;font-size:15px;color:#fff;font-family:var(--mono)}
.logo-text{font-size:17px;font-weight:700;letter-spacing:-.02em}
.logo-text span{color:var(--peri)}
.logo-sub{font-size:10px;color:var(--muted);letter-spacing:.04em;margin-top:1px}
.auth-box{display:flex;gap:6px}
.auth-box input{flex:1;background:#12131a;border:1px solid #2a2d45;border-radius:var(--radius);padding:8px 10px;font-family:var(--mono);font-size:11px;color:#e2e4f0;min-width:0}
.auth-box input:focus{outline:none;border-color:var(--peri);box-shadow:0 0 0 2px var(--peri-glow)}
.btn{padding:8px 14px;border-radius:var(--radius);font-size:12px;font-weight:600;border:1px solid var(--border);background:var(--card);transition:all .15s}
.btn:hover{background:var(--card-hover);border-color:var(--border-hover)}
.btn-peri{background:var(--peri);border-color:var(--peri);color:#fff}.btn-peri:hover{background:#6b72ee}
.btn-danger{background:var(--red);border-color:var(--red);color:#fff;font-size:11px}.btn-danger:hover{background:#e85555}
.btn-sm{padding:5px 10px;font-size:11px}
.status{display:flex;align-items:center;gap:6px;margin-top:10px;font-size:11px;color:var(--muted)}
.dot{width:7px;height:7px;border-radius:50%;flex-shrink:0}.dot.ok{background:var(--green)}.dot.err{background:var(--red)}

/* Tree */
.sidebar-body{flex:1;overflow-y:auto;padding:8px}
.search-box{padding:0 10px 8px}
.search-box input{width:100%;background:#12131a;border:1px solid #2a2d45;border-radius:var(--radius);padding:7px 10px;font-size:12px;color:#e2e4f0}
.search-box input:focus{outline:none;border-color:var(--peri)}
.tenant-block{margin-bottom:4px}
.tenant-name{padding:8px 12px 4px;font-size:10px;font-weight:700;letter-spacing:.08em;text-transform:uppercase;color:#8688a4;display:flex;justify-content:space-between;align-items:center}
.job-name{padding:4px 12px 2px;font-size:10px;color:#5a5d7a;font-weight:600;letter-spacing:.04em}
.cap-item{display:block;width:100%;text-align:left;padding:8px 12px;border-radius:var(--radius);margin:1px 0;transition:all .12s;border:1px solid transparent;color:#c8cade}
.cap-item:hover{background:#22243a;border-color:#2a2d45}
.cap-item.active{background:#6366f120;border-color:var(--peri);box-shadow:0 0 0 1px #6366f140}
.cap-label{font-size:12px;font-weight:600;color:#e2e4f0}
.cap-path{font-size:10px;color:#6e7191;font-family:var(--mono);margin-top:2px}

.sidebar-foot{border-top:1px solid #2a2d45;padding:12px 18px}
.sidebar-foot details{font-size:11px;color:#8688a4}
.sidebar-foot summary{cursor:pointer;font-weight:600;margin-bottom:6px}
.sidebar-foot input{width:100%;background:#12131a;border:1px solid #2a2d45;border-radius:var(--radius);padding:6px 8px;font-size:11px;margin-bottom:4px;color:#e2e4f0}

/* Main */
.main{display:flex;flex-direction:column;overflow:hidden}
.topbar{display:flex;align-items:center;justify-content:space-between;padding:14px 24px;border-bottom:1px solid var(--border);background:#fff;flex-shrink:0}
.topbar-left{display:flex;align-items:center;gap:12px}
.page-title{font-size:18px;font-weight:700;letter-spacing:-.01em}
.page-path{font-size:11px;color:var(--muted);font-family:var(--mono)}
.tabs{display:flex;gap:2px;background:#eef0f6;border-radius:var(--radius);padding:3px}
.tab{padding:6px 14px;border-radius:6px;font-size:12px;font-weight:600;color:var(--muted);transition:all .15s}
.tab:hover{color:var(--text)}
.tab.active{background:var(--peri);color:#fff;box-shadow:0 1px 3px #6366f140}

.content{flex:1;overflow-y:auto;padding:20px 24px 40px}
.error-box{margin-bottom:12px}
.error{background:var(--red-soft);border:1px solid var(--red);border-radius:var(--radius);padding:10px 14px;font-size:12px;color:var(--red)}

/* Stats */
.stats{display:grid;grid-template-columns:repeat(4,1fr);gap:10px;margin-bottom:20px}
.stat{background:#fff;border:1px solid var(--border);border-radius:var(--radius-lg);padding:16px;box-shadow:0 1px 3px rgba(0,0,0,.04)}
.stat-label{font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.06em;font-weight:600}
.stat-value{font-size:22px;font-weight:700;font-family:var(--mono);margin-top:4px}
.stat-value.peri{color:var(--peri)}
.stat-sub{font-size:10px;color:var(--dim);margin-top:2px}

/* Cards */
.card{background:#fff;border:1px solid var(--border);border-radius:var(--radius-lg);margin-bottom:14px;overflow:hidden;box-shadow:0 1px 3px rgba(0,0,0,.04)}
.card-head{padding:12px 16px;border-bottom:1px solid var(--border);display:flex;justify-content:space-between;align-items:center}
.card-title{font-size:12px;font-weight:700;text-transform:uppercase;letter-spacing:.06em;color:var(--muted)}
.card-body{padding:14px 16px}
.card-full{grid-column:1/-1}
.grid-2{display:grid;grid-template-columns:1fr 1fr;gap:14px}

/* Pill */
.pill{display:inline-block;padding:2px 8px;border-radius:20px;font-size:10px;font-weight:600;font-family:var(--mono);background:#eef0f6;color:var(--muted)}
.pill.ok{background:var(--green-soft);color:var(--green)}
.pill.warn{background:var(--amber-soft);color:var(--amber)}
.pill.bad{background:var(--red-soft);color:var(--red)}
.pill.peri{background:var(--peri-soft);color:var(--peri)}

/* Strategy */
.strategy{background:#f8f9fc;border:1px solid var(--border);border-radius:var(--radius);padding:14px;margin-bottom:10px}
.strategy-head{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.strategy-id{font-family:var(--mono);font-weight:700;color:var(--peri);font-size:13px}
.strategy-meta{font-size:11px;color:var(--muted)}
.option{display:grid;grid-template-columns:80px 1fr 70px 60px 70px;gap:8px;align-items:center;margin-bottom:5px;font-size:12px}
.option-name{font-family:var(--mono);font-weight:600;font-size:11px;color:var(--muted)}
.option-name.win{color:var(--green)}
.bar-bg{height:20px;background:#eef0f6;border-radius:4px;overflow:hidden}
.bar{height:100%;border-radius:4px;background:linear-gradient(90deg,var(--peri),#9BA1FF);transition:width .6s ease;min-width:2px}
.bar.win{background:linear-gradient(90deg,var(--green),#5eead4)}
.cell-label{font-size:9px;color:var(--dim);display:block;letter-spacing:.04em;text-transform:uppercase}
.cell-value{font-family:var(--mono);font-weight:600;font-size:12px}

/* Table */
.table{width:100%;border-collapse:collapse;font-size:12px}
.table th{text-align:left;font-size:10px;color:var(--muted);text-transform:uppercase;letter-spacing:.06em;font-weight:600;padding:6px 10px;border-bottom:1px solid var(--border)}
.table td{padding:6px 10px;border-bottom:1px solid var(--border);vertical-align:top}
.table tr:last-child td{border-bottom:none}
.mono{font-family:var(--mono)}

/* Timeline */
.event{padding:8px 0;border-bottom:1px solid var(--border)}
.event:last-child{border-bottom:none}
.event-main{display:flex;justify-content:space-between;align-items:center;gap:8px}
.event-main b{font-size:12px}
.event-meta{font-size:10px;color:var(--muted);margin-top:3px;font-family:var(--mono);word-break:break-all}

/* Graph */
.graph-shell{width:100%;height:440px;background:#080b14;border-radius:var(--radius-lg);overflow:hidden;position:relative;border:1px solid #252840;box-shadow:inset 0 0 0 1px rgba(255,255,255,.025)}
.graph-shell canvas{width:100%;height:100%}
.graph-legend{display:flex;gap:14px;flex-wrap:wrap;margin-top:8px;font-size:10px;color:var(--muted)}
.legend-dot{width:8px;height:8px;border-radius:50%;display:inline-block;margin-right:4px;vertical-align:middle}

.empty{color:var(--dim);font-size:12px;padding:16px 0;text-align:center}
.hide{display:none!important}

/* Caps panel */
.cap-group{margin-bottom:10px}
.cap-group b{font-size:11px;color:var(--peri)}
.cap-group div{font-size:11px;color:var(--muted);font-family:var(--mono);margin-top:4px;line-height:1.6}

@media(max-width:900px){.app{grid-template-columns:1fr}.sidebar{display:none}}
</style>
</head>
<body>
<div class="app">
  <!-- Sidebar -->
  <aside class="sidebar">
    <div class="sidebar-head">
      <div class="logo-row">
        <div class="logo-mark">__SERVICE_INITIAL__</div>
        <div><div class="logo-text"><span>__SERVICE_NAME__</span> Console</div><div class="logo-sub">Adaptive Runtime</div></div>
      </div>
      <div class="auth-box">
        <input id="key" type="password" placeholder="Admin key">
        <button class="btn btn-peri" id="connect">Connect</button>
      </div>
      <div class="status"><span class="dot" id="health-dot"></span><span id="health-text">Disconnected</span></div>
    </div>
    <div class="search-box" style="padding-top:10px"><input id="tree-filter" placeholder="Search capsules..."></div>
    <div class="sidebar-body" id="capsule-list"><div class="empty">Connect to load capsules</div></div>
    <div class="sidebar-foot">
      <details>
        <summary>Create Job</summary>
        <input id="job-tenant" placeholder="Tenant">
        <input id="job-id" placeholder="Job ID">
        <input id="job-name" placeholder="Name (optional)">
        <input id="job-desc" placeholder="Description (optional)">
        <button class="btn btn-sm btn-peri" id="create-job" style="margin-top:4px;width:100%">Create</button>
      </details>
    </div>
  </aside>

  <!-- Main -->
  <div class="main">
    <div class="topbar">
      <div class="topbar-left">
        <div>
          <div class="page-title" id="page-title">__SERVICE_NAME__ Console</div>
          <div class="page-path" id="page-sub">Select a capsule to begin</div>
        </div>
      </div>
      <div style="display:flex;align-items:center;gap:10px">
        <div class="tabs">
          <button class="tab active" data-tab="overview">Overview</button>
          <button class="tab" data-tab="decisions">Decisions</button>
          <button class="tab" data-tab="logs">Logs</button>
          <button class="tab" data-tab="system">System</button>
        </div>
        <button class="btn btn-sm" id="refresh-main" title="Refresh">&#x21bb;</button>
      </div>
    </div>

    <div class="content">
      <div id="error-box" class="error-box"></div>

      <!-- Overview tab -->
      <div data-panel="overview">
        <div class="stats">
          <div class="stat"><div class="stat-label">Strategies</div><div class="stat-value peri" id="k-strategies">-</div><div class="stat-sub" id="hash-pill">-</div></div>
          <div class="stat"><div class="stat-label">Confidence</div><div class="stat-value" id="k-confidence">-</div><div class="stat-sub" id="winner-pill">-</div></div>
          <div class="stat"><div class="stat-label">Decisions</div><div class="stat-value" id="k-decisions">-</div><div class="stat-sub" id="decision-count">-</div></div>
          <div class="stat"><div class="stat-label">Audits</div><div class="stat-value" id="audit-summary">-</div><div class="stat-sub" id="audit-count">-</div></div>
        </div>

        <div class="card">
          <div class="card-head"><span class="card-title">Strategy Weights</span><span class="pill peri" id="activation-pill">-</span></div>
          <div class="card-body" id="strategies"><div class="empty">No capsule selected</div></div>
        </div>

        <div class="card">
          <div class="card-head"><span class="card-title">Capsule Graph</span><span class="pill peri" id="graph-mode">-</span></div>
          <div class="card-body">
            <div style="display:none"><span id="g-nodes">-</span><span id="g-edges">-</span><span id="g-strategies">-</span><span id="g-contexts">-</span></div>
            <div class="graph-shell" id="graph-shell"><canvas id="capsule-graph"></canvas></div>
            <div class="graph-legend">
              <span><span class="legend-dot" style="background:#94a3b8"></span>input</span>
              <span><span class="legend-dot" style="background:#7dd3fc"></span>compute</span>
              <span><span class="legend-dot" style="background:#facc15"></span>strategy</span>
              <span><span class="legend-dot" style="background:#c084fc"></span>capability</span>
              <span><span class="legend-dot" style="background:#fb7185"></span>output</span>
              <span><span class="legend-dot" style="background:#34d399"></span>context</span>
            </div>
            <div style="margin-top:6px;font-size:11px;color:var(--muted)" id="graph-note"></div>
          </div>
        </div>

        <div class="grid-2">
          <div class="card">
            <div class="card-head"><span class="card-title">Policy</span><span class="pill" id="policy-mode">-</span></div>
            <div class="card-body" id="policy"><div class="empty">No policy loaded</div></div>
          </div>
          <div class="card">
            <div class="card-head"><span class="card-title">Capsule Detail</span></div>
            <div class="card-body" id="selected-detail"><div class="empty">No capsule selected</div></div>
          </div>
        </div>
      </div>

      <!-- Decisions tab -->
      <div data-panel="decisions" class="hide">
        <div class="card">
          <div class="card-head"><span class="card-title">Recent Decisions</span><span class="pill peri" id="k-last">-</span></div>
          <div class="card-body" id="decisions"><div class="empty">No decisions logged</div></div>
        </div>
      </div>

      <!-- Logs tab -->
      <div data-panel="logs" class="hide">
        <div class="grid-2">
          <div class="card">
            <div class="card-head"><span class="card-title">Audit Log</span><span class="pill" id="audit-count-side">0</span></div>
            <div class="card-body" style="max-height:400px;overflow-y:auto" id="audits"><div class="empty">No audits</div></div>
          </div>
          <div class="card">
            <div class="card-head"><span class="card-title">Evolution Log</span><span class="pill" id="evolution-count">0</span></div>
            <div class="card-body" style="max-height:400px;overflow-y:auto" id="evolution"><div class="empty">No evolution events</div></div>
          </div>
        </div>
      </div>

      <!-- System tab -->
      <div data-panel="system" class="hide">
        <div class="stats">
          <div class="stat"><div class="stat-label">Tenants</div><div class="stat-value" id="k-tenants">-</div></div>
          <div class="stat"><div class="stat-label">Jobs</div><div class="stat-value" id="k-jobs">-</div></div>
          <div class="stat"><div class="stat-label">Capsules</div><div class="stat-value peri" id="k-capsules">-</div></div>
          <div class="stat"><div class="stat-label">Capabilities</div><div class="stat-value" id="cap-count">-</div></div>
        </div>
        <div class="card">
          <div class="card-head"><span class="card-title">Capability Registry</span></div>
          <div class="card-body" id="capabilities"><div class="empty">Not loaded</div></div>
        </div>
      </div>
    </div>
  </div>
</div>

<script>
/* ── Hidden compat IDs for JS that references them ── */
void(document.getElementById("context-name")||document.body.insertAdjacentHTML("beforeend",'<span id="context-name" class="hide"></span><span id="context-path" class="hide"></span><span id="selected-pill" class="hide"></span><span id="clock" class="hide"></span><span id="rail-refresh" class="hide"></span><span id="refresh-side" class="hide"></span>'));

const state={key:"",inventory:[],capabilities:[],selected:null,report:null,inspect:null,policy:null,contexts:[],memory:null,decisions:[],audits:[],evolution:[],activeTab:"overview",graphFrame:null,graphSeed:1};
const $=id=>document.getElementById(id);
const esc=v=>String(v??"").replace(/[&<>"']/g,m=>({"&":"&amp;","<":"&lt;",">":"&gt;",'"':"&quot;","'":"&#39;"}[m]));
const pct=v=>Number.isFinite(+v)?((+v)*100).toFixed(1)+"%":"-";
const short=v=>v?String(v).slice(0,16):"-";
const auth=()=>({"Authorization":"Bearer "+state.key,"Content-Type":"application/json"});
const enc=encodeURIComponent;
function setError(msg){$("error-box").innerHTML=msg?'<div class="error">'+esc(msg)+'</div>':""}
function setHealth(ok,label){$("health-dot").className="dot "+(ok?"ok":"err");$("health-text").textContent=label}
async function json(path){const r=await fetch(path,{headers:auth()});if(!r.ok)throw new Error(path+" -> HTTP "+r.status);return await r.json()}
async function maybeJson(path){try{return await json(path)}catch(_){return null}}
async function text(path){const r=await fetch(path,{headers:auth()});if(!r.ok)throw new Error(path+" -> HTTP "+r.status);return await r.text()}
function parseLines(raw){return String(raw||"").split(/\n+/).map(s=>s.trim()).filter(Boolean).map(s=>{try{return JSON.parse(s)}catch(_){return{raw:s}}})}
function capsuleBase(sel){return "/tenants/"+enc(sel.tenant)+"/jobs/"+enc(sel.job||"default")+"/capsules/"+enc(sel.capsule)}

async function connect(){
  state.key=$("key").value.trim();
  sessionStorage.setItem("lycanKey",state.key);
  setError("");
  try{
    const h=await fetch("/health").then(r=>r.json());
    if(!h.ok)throw new Error("health check failed");
    setHealth(true,"Online");
    await Promise.all([loadInventory(),loadCapabilities()]);
    renderCapabilities();renderInventory();
    const first=state.inventory.flatMap(t=>t.jobs.flatMap(j=>j.capsules.map(c=>({tenant:t.tenant,job:j.id,capsule:c})))).shift();
    if(!state.selected&&first)state.selected=first;
    if(state.selected)await loadCapsule(state.selected);
  }catch(e){setHealth(false,"Auth failed");setError(e.message)}
}

async function loadInventory(){
  const tenants=(await json("/tenants")).tenants||[];
  const inventory=[];
  for(const tenant of tenants){
    const jobsResp=await maybeJson("/tenants/"+enc(tenant)+"/jobs");
    if(jobsResp&&Array.isArray(jobsResp.jobs)){
      const jobs=[];
      for(const j of jobsResp.jobs){
        const id=typeof j==="string"?j:j.id;if(!id)continue;
        let capsules=Array.isArray(j.capsules)?j.capsules:[];
        if(!capsules.length){
          const detail=await maybeJson("/tenants/"+enc(tenant)+"/jobs/"+enc(id));
          if(detail&&Array.isArray(detail.capsules))capsules=detail.capsules;
          else if(detail&&Array.isArray(detail.capsuleList))capsules=detail.capsuleList;
          else if(detail&&detail.job&&Array.isArray(detail.job.capsules))capsules=detail.job.capsules;
          else if(detail&&detail.job&&Array.isArray(detail.job.capsuleList))capsules=detail.job.capsuleList;
        }
        jobs.push({id,name:j.name||id,capsules});
      }
      inventory.push({tenant,jobs});
    }else{
      const caps=(await json("/tenants/"+enc(tenant)+"/capsules")).capsules||[];
      inventory.push({tenant,jobs:[{id:"default",name:"default",capsules:caps}]});
    }
  }
  state.inventory=inventory;
  $("k-tenants").textContent=inventory.length;
  $("k-jobs").textContent=inventory.reduce((n,t)=>n+t.jobs.length,0);
  $("k-capsules").textContent=inventory.reduce((n,t)=>n+t.jobs.reduce((m,j)=>m+j.capsules.length,0),0);
}

async function loadCapabilities(){
  const caps=await json("/capabilities");
  state.capabilities=Array.isArray(caps)?caps:[];
  $("cap-count").textContent=state.capabilities.length+" kernels";
}

async function loadCapsule(sel){
  state.selected=sel;renderInventory();
  $("page-title").textContent=sel.capsule;
  $("page-sub").textContent=sel.tenant+" / "+sel.job+" / "+sel.capsule;
  setError("");
  try{
    const base=capsuleBase(sel);
    const [report,inspect,policy,contexts,memory,decisions,audits,evolution]=await Promise.all([
      json(base+"/report"),json(base+"/inspect"),
      json(base+"/policy").catch(e=>({error:e.message})),
      json(base+"/contexts").catch(()=>({contexts:[]})),
      json(base+"/memory").catch(()=>null),
      text(base+"/decisions").catch(()=>""),
      text(base+"/audits").catch(()=>""),
      text(base+"/evolution").catch(()=>"")
    ]);
    state.report=report;state.inspect=inspect;state.policy=policy;
    state.contexts=contexts.contexts||[];state.memory=memory;
    state.decisions=parseLines(decisions);state.audits=parseLines(audits);
    state.evolution=parseLines(evolution);state.graphSeed++;
    renderAll();
  }catch(e){setError(e.message)}
}

function renderInventory(){
  const q=($("tree-filter").value||"").toLowerCase();
  if(!state.inventory.length){$("capsule-list").innerHTML='<div class="empty">Connect to load capsules</div>';return}
  let html="";
  for(const t of state.inventory){
    const jobs=t.jobs.map(j=>({...j,capsules:j.capsules.filter(c=>(t.tenant+"/"+j.id+"/"+c).toLowerCase().includes(q))})).filter(j=>j.capsules.length||!q);
    if(!jobs.length)continue;
    html+='<div class="tenant-block"><div class="tenant-name"><span>'+esc(t.tenant)+'</span><span class="pill">'+jobs.reduce((n,j)=>n+j.capsules.length,0)+'</span></div>';
    for(const job of jobs){
      html+='<div class="job-name">'+esc(job.name||job.id)+'</div>';
      for(const cap of job.capsules){
        const active=state.selected&&state.selected.tenant===t.tenant&&state.selected.job===job.id&&state.selected.capsule===cap;
        html+='<button class="cap-item '+(active?'active':'')+'" data-tenant="'+esc(t.tenant)+'" data-job="'+esc(job.id)+'" data-capsule="'+esc(cap)+'"><div class="cap-label">'+esc(cap)+'</div><div class="cap-path">'+esc(t.tenant)+'/'+esc(job.id)+'</div></button>';
      }
    }
    html+='</div>';
  }
  $("capsule-list").innerHTML=html||'<div class="empty">No matches</div>';
  document.querySelectorAll(".cap-item").forEach(b=>b.onclick=()=>loadCapsule({tenant:b.dataset.tenant,job:b.dataset.job,capsule:b.dataset.capsule}));
}

function renderAll(){renderOverview();renderStrategies();renderDecisions();renderPolicy();renderTimeline("audits",state.audits,"audit-count-side");renderTimeline("evolution",state.evolution,"evolution-count");renderSelected();if(state.activeTab==="overview")renderGraph();applyTab()}

function renderOverview(){
  const strategies=state.report?.strategies||[];
  $("k-strategies").textContent=strategies.length;
  $("hash-pill").textContent="hash "+short(state.report?.hash);
  let best=null,totalActivations=0;
  for(const st of strategies){totalActivations+=Number(st.activations||0);for(const o of st.options||[]){if(!best||o.weight>best.weight)best={...o,node_id:st.node_id}}}
  $("k-confidence").textContent=best?pct(best.weight):"-";
  $("winner-pill").textContent=best?"node "+best.node_id+" / opt "+best.option:"-";
  $("activation-pill").textContent=totalActivations+" activations";
  $("k-decisions").textContent=state.decisions.length;
  $("decision-count").textContent=state.decisions.length+" logged";
  $("audit-count").textContent=state.audits.length+" events";
  $("audit-summary").textContent=state.audits.length;
}

function renderStrategies(){
  const strategies=state.report?.strategies||[];
  if(!strategies.length){$("strategies").innerHTML='<div class="empty">No strategy nodes</div>';return}
  $("strategies").innerHTML=strategies.map(st=>{
    const opts=st.options||[];const winner=opts.reduce((a,o)=>!a||o.weight>a.weight?o:a,null);
    return '<div class="strategy"><div class="strategy-head"><div><div class="strategy-id">Strategy #'+esc(st.node_id)+'</div><div class="strategy-meta">'+esc(st.n_options||opts.length)+' options &middot; '+esc(st.activations||0)+' fires</div></div><span class="pill ok">winner '+esc(winner?.option??"-")+'</span></div>'
    +opts.map(o=>{const isWin=winner&&winner.option===o.option;return '<div class="option"><div class="option-name '+(isWin?'win':'')+'">Opt '+esc(o.option)+'</div><div class="bar-bg"><div class="bar '+(isWin?'win':'')+'" style="width:'+Math.max((+o.weight||0)*100,1)+'%"></div></div><div><span class="cell-label">Weight</span><span class="cell-value">'+pct(o.weight)+'</span></div><div><span class="cell-label">Tries</span><span class="cell-value">'+esc(o.tries??0)+'</span></div><div><span class="cell-label">Avg</span><span class="cell-value">'+Number(o.avg_ms||0).toFixed(3)+'ms</span></div></div>'}).join("")+'</div>';
  }).join("");
}

function renderDecisions(){
  const rows=state.decisions.slice(-20).reverse();
  if(!rows.length){$("decisions").innerHTML='<div class="empty">No decisions</div>';return}
  let html='<table class="table"><thead><tr><th>ID</th><th>Mode</th><th>Selected</th><th>Confidence</th><th>Context</th></tr></thead><tbody>';
  for(const ev of rows){const d=ev.decisions?.[0]||{};const learned=ev.learned===true||ev.learned==="true";
    html+='<tr><td class="mono">'+esc(short(ev.id))+'</td><td>'+(learned?'<span class="pill warn">learn</span>':'<span class="pill peri">read</span>')+'</td><td class="mono">#'+esc(d.node_id??"-")+' &rarr; '+esc(d.chosen_option??"-")+'</td><td>'+pct(d.confidence)+'</td><td class="mono">'+esc(ev.contextKey||"default")+'</td></tr>'}
  $("decisions").innerHTML=html+'</tbody></table>';
}

function renderPolicy(){
  const p=state.policy||{};
  const fields=[["stdout",p.allow_stdout],["stdin",p.allow_stdin],["file read",p.allow_file_read],["file write",p.allow_file_write],["network",p.allow_network]];
  $("policy").innerHTML=fields.map(([n,on])=>'<span class="pill '+(on?'ok':'bad')+'">'+esc(n)+' '+(on?'&#10003;':'&#10005;')+'</span>').join(" ")
    +(p.file_root?'<span class="pill peri" style="margin-left:4px">root: '+esc(p.file_root)+'</span>':'')
    +(Array.isArray(p.allowed_hosts)&&p.allowed_hosts.length?'<span class="pill peri" style="margin-left:4px">hosts: '+esc(p.allowed_hosts.join(", "))+'</span>':'');
  $("policy-mode").textContent=p.error?"error":"enforced";
}

function renderTimeline(id,items,countId){
  $(countId).textContent=items.length+" events";
  if(!items.length){$(id).innerHTML='<div class="empty">No events</div>';return}
  $(id).innerHTML=items.slice(-15).reverse().map(ev=>{
    const action=ev.event||ev.action||"event";const lower=action.toLowerCase();
    const tag=lower.includes("reject")?"bad":lower.includes("accept")||action==="feedback"?"ok":"peri";
    const bits=[ev.job?("job: "+ev.job):"",ev.decisionId,ev.nodeId?("node "+ev.nodeId):"",ev.option!=null?("opt "+ev.option):"",ev.reward!=null?("reward "+ev.reward):""].filter(Boolean).join(" &middot; ");
    return '<div class="event"><div class="event-main"><b>'+esc(action)+'</b><span class="pill '+tag+'">'+esc(ev.timestamp??"-")+'</span></div><div class="event-meta">'+esc(bits||ev.reason||ev.raw||JSON.stringify(ev))+'</div></div>'
  }).join("");
}

function renderCapabilities(){
  const groups={};for(const c of state.capabilities){const pkg=c.package||"other";(groups[pkg]||(groups[pkg]=[])).push(c)}
  $("capabilities").innerHTML=Object.keys(groups).sort().map(pkg=>'<div class="cap-group"><b>'+esc(pkg)+' <span class="mono">('+groups[pkg].length+')</span></b><div>'+groups[pkg].map(c=>esc(c.name)).join("<br>")+'</div></div>').join("")||'<div class="empty">No capabilities</div>';
}

function renderSelected(){
  const sel=state.selected||{};const r=state.report||{};const last=state.decisions[state.decisions.length-1]||{};
  $("selected-detail").innerHTML='<table class="table"><tbody>'
    +'<tr><th>Tenant</th><td class="mono">'+esc(sel.tenant||"-")+'</td></tr>'
    +'<tr><th>Job</th><td class="mono">'+esc(sel.job||"default")+'</td></tr>'
    +'<tr><th>Capsule</th><td class="mono">'+esc(sel.capsule||"-")+'</td></tr>'
    +'<tr><th>Graph hash</th><td class="mono">'+esc(r.hash||"-")+'</td></tr>'
    +'<tr><th>Last decision</th><td class="mono">'+esc(last.id||"-")+'</td></tr>'
    +'<tr><th>Learned</th><td>'+((last.learned===true||last.learned==="true")?'<span class="pill warn">yes</span>':'<span class="pill peri">no</span>')+'</td></tr>'
    +'</tbody></table>'
    +'<div style="display:flex;gap:8px;margin-top:14px">'
    +'<button class="btn btn-danger btn-sm" id="btn-purge-logs">Purge Logs</button>'
    +'<button class="btn btn-danger btn-sm" id="btn-delete-capsule">Delete Capsule</button>'
    +'</div>';
  const base=capsuleBase(sel);
  $("btn-purge-logs").onclick=async()=>{
    if(!confirm("Purge all logs for "+sel.capsule+"?"))return;
    try{const r=await fetch(base+"/logs",{method:"DELETE",headers:auth()});const j=await r.json();if(j.ok){setError("");await loadCapsule(sel)}else setError(j.error||"failed")}catch(e){setError(e.message)}
  };
  $("btn-delete-capsule").onclick=async()=>{
    if(!confirm("DELETE "+sel.tenant+"/"+sel.job+"/"+sel.capsule+"? This is permanent."))return;
    try{const r=await fetch(base,{method:"DELETE",headers:auth()});const j=await r.json();if(j.ok){state.selected=null;await loadInventory();renderInventory();$("selected-detail").innerHTML='<div class="empty">Deleted</div>'}else setError(j.error||"failed")}catch(e){setError(e.message)}
  };
}

/* Graph visualization */
function hashNum(v){let h=2166136261;const s=String(v);for(let i=0;i<s.length;i++){h^=s.charCodeAt(i);h=Math.imul(h,16777619)}return(h>>>0)/4294967295}
function graphKind(op,wk){if(op==="Strategy"||op==="AdaptiveChoice"||wk==="Strategy"||wk==="Adaptive")return"strategy";if(op==="Capability")return"capability";if(/^Const|LoadVar/.test(op))return"input";if(/Print|Return|Halt/.test(op))return"output";return"compute"}
function graphColor(k){return{input:"#94a3b8",compute:"#7dd3fc",strategy:"#facc15",capability:"#c084fc",output:"#fb7185",context:"#34d399"}[k]||"#7dd3fc"}
function graphNodeLabel(n){if(n.kind==="strategy")return"Strategy #"+n.id;if(n.kind==="capability")return n.op||"Capability";if(n.kind==="output")return n.op||"Output";return""}
function canvasRoundRect(ctx,x,y,w,h,r){if(ctx.roundRect){ctx.roundRect(x,y,w,h,r);return}ctx.moveTo(x+r,y);ctx.lineTo(x+w-r,y);ctx.quadraticCurveTo(x+w,y,x+w,y+r);ctx.lineTo(x+w,y+h-r);ctx.quadraticCurveTo(x+w,y+h,x+w-r,y+h);ctx.lineTo(x+r,y+h);ctx.quadraticCurveTo(x,y+h,x,y+h-r);ctx.lineTo(x,y+r);ctx.quadraticCurveTo(x,y,x+r,y)}
function makeGraphModel(w,h){
  const inspect=state.inspect||{};const raw=inspect.nodeList||[];const report=state.report||{};const strategies=report.strategies||[];
  const strategyIds=new Set(strategies.map(s=>Number(s.node_id)));const hotIds=new Set(raw.filter(n=>Number(n.activationCount||0)>0).map(n=>Number(n.id)));
  const keepMap=new Map();
  const add=n=>{if(n&&Number.isFinite(Number(n.id)))keepMap.set(Number(n.id),n)};
  raw.forEach((n,i)=>{const id=Number(n.id);const kind=graphKind(n.op,n.weightKind);if(strategyIds.has(id)||hotIds.has(id)||kind==="capability"||kind==="output"||kind==="input"||i%Math.max(1,Math.ceil(raw.length/92))===0)add(n)});
  raw.forEach(n=>{const id=Number(n.id);if(strategyIds.has(id)){for(const ref of n.operandRefs||[]){const found=raw.find(x=>Number(x.id)===Number(ref));add(found)}}});
  if(!keepMap.size)raw.slice(0,92).forEach(add);
  const keep=[...keepMap.values()].sort((a,b)=>Number(a.id)-Number(b.id));
  const ids=new Set(keep.map(n=>Number(n.id)));
  const px=52,py=54;const lane={input:.10,compute:.34,capability:.54,strategy:.72,output:.90};
  const groups={input:[],compute:[],capability:[],strategy:[],output:[]};
  for(const n of keep){const kind=graphKind(n.op,n.weightKind);(groups[kind]||groups.compute).push(n)}
  const nodes=[];
  for(const kind of ["input","compute","capability","strategy","output"]){
    const arr=groups[kind]||[];const cols=Math.max(1,Math.ceil(arr.length/13));const colGap=kind==="compute"?34:26;
    arr.forEach((n,i)=>{
      const id=Number(n.id);const col=i%cols;const row=Math.floor(i/cols);const rows=Math.max(1,Math.ceil(arr.length/cols));
      const x=w*lane[kind]+(col-(cols-1)/2)*colGap+(hashNum("x"+id)-.5)*10;
      const y=py+(h-py*2)*(row+1)/(rows+1)+(hashNum("y"+id)-.5)*12;
      const hot=Number(n.activationCount||0)>0;const strat=strategies.find(s=>Number(s.node_id)===id);
      const winner=strat?.options?.reduce((a,o)=>!a||o.weight>a.weight?o:a,null);
      const r=kind==="strategy"?15:kind==="capability"?10:kind==="output"?9:kind==="input"?7:hot?7:5.5;
      nodes.push({id,op:n.op,kind,x,y,r,hot,strategy:!!strat,winner,weights:n.weights||[],activation:Number(n.activationCount||0)});
    });
  }
  const byId=new Map(nodes.map(n=>[n.id,n]));let edges=[];
  for(const n of keep){const to=Number(n.id);for(const from of n.operandRefs||[]){if(ids.has(Number(from))&&ids.has(to))edges.push({from:Number(from),to,kind:"operand",weight:.45})}}
  for(const e of inspect.edgeList||[]){if(ids.has(Number(e.from))&&ids.has(Number(e.to)))edges.push({from:Number(e.from),to:Number(e.to),kind:"edge",weight:Number(e.weight||.35)})}
  edges=edges.slice(0,220);
  if(edges.length<Math.max(6,nodes.length*.22)){const ordered=[...nodes].sort((a,b)=>a.x-b.x||a.y-b.y);for(let i=1;i<ordered.length;i++){if(hashNum("link"+i+state.graphSeed)>.42)edges.push({from:ordered[i-1].id,to:ordered[i].id,kind:"flow",weight:.18})}}
  const contexts=(state.contexts||[]).slice(0,18).map((c,i)=>{
    const anchor=byId.get(Number(c.nodeId))||nodes.find(n=>n.strategy)||nodes[0];const angle=(Math.PI*2*i)/Math.max(1,state.contexts.length);
    const weights=c.weights||[];const best=weights.length?Math.max(...weights):0;
    return{id:"ctx-"+i,kind:"context",label:c.contextKey||"context",x:(anchor?.x||w*.5)+Math.cos(angle)*(52+best*38),y:(anchor?.y||h*.5)+Math.sin(angle)*(38+best*28),r:5+best*7,anchor,best,tries:c.totalTries||0};
  });
  return{nodes,edges,contexts,byId,started:performance.now()};
}
function drawGraph(model,t){
  const canvas=$("capsule-graph");const shell=$("graph-shell");if(!canvas||!shell)return;
  const dpr=window.devicePixelRatio||1;const rect=shell.getBoundingClientRect();const w=Math.max(320,rect.width),h=Math.max(300,rect.height);
  if(canvas.width!==Math.floor(w*dpr)||canvas.height!==Math.floor(h*dpr)){canvas.width=Math.floor(w*dpr);canvas.height=Math.floor(h*dpr);canvas.style.width=w+"px";canvas.style.height=h+"px"}
  const ctx=canvas.getContext("2d");ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,w,h);
  const bg=ctx.createLinearGradient(0,0,w,h);bg.addColorStop(0,"#090d18");bg.addColorStop(.55,"#0b1020");bg.addColorStop(1,"#06080f");ctx.fillStyle=bg;ctx.fillRect(0,0,w,h);
  const lanes=[["input",.10],["compute",.34],["capability",.54],["strategy",.72],["output",.90]];
  ctx.font="700 10px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif";ctx.textAlign="center";ctx.textBaseline="top";
  for(const [name,frac] of lanes){const x=w*frac;ctx.fillStyle="rgba(255,255,255,.035)";ctx.fillRect(x-44,14,88,h-28);ctx.strokeStyle="rgba(255,255,255,.055)";ctx.lineWidth=1;ctx.strokeRect(x-44,14,88,h-28);ctx.fillStyle=graphColor(name);ctx.fillText(name.toUpperCase(),x,24)}
  const pulse=(Math.sin(t/620)+1)/2;
  for(const e of model.edges){const a=model.byId.get(e.from),b=model.byId.get(e.to);if(!a||!b)continue;const hot=a.hot||b.hot||a.kind==="strategy"||b.kind==="strategy";ctx.beginPath();ctx.moveTo(a.x,a.y);const mx=(a.x+b.x)/2,my=(a.y+b.y)/2-24*Math.sin((a.id+b.id+t/1200)%6);ctx.quadraticCurveTo(mx,my,b.x,b.y);ctx.strokeStyle=hot?"rgba(124,131,255,.50)":"rgba(148,163,184,.18)";ctx.lineWidth=hot?1.8:.9;ctx.stroke()}
  for(const c of model.contexts){if(c.anchor){ctx.beginPath();ctx.moveTo(c.anchor.x,c.anchor.y);ctx.lineTo(c.x,c.y);ctx.strokeStyle="rgba(52,211,153,.24)";ctx.lineWidth=1;ctx.stroke()}}
  for(const c of model.contexts){ctx.beginPath();ctx.arc(c.x,c.y,c.r+pulse*1.8,0,Math.PI*2);ctx.fillStyle="rgba(52,211,153,.78)";ctx.shadowColor="#34d399";ctx.shadowBlur=12;ctx.fill();ctx.shadowBlur=0}
  for(const n of model.nodes){const color=graphColor(n.kind);ctx.beginPath();ctx.arc(n.x,n.y,n.r+(n.hot?pulse*1.6:0),0,Math.PI*2);ctx.fillStyle=color;ctx.shadowColor=n.kind==="strategy"?"#facc15":color;ctx.shadowBlur=n.kind==="strategy"?22:n.hot?12:5;ctx.fill();ctx.shadowBlur=0;ctx.lineWidth=n.kind==="strategy"?2.5:1;ctx.strokeStyle=n.kind==="strategy"?"rgba(255,255,255,.82)":"rgba(255,255,255,.35)";ctx.stroke();
    if(n.kind==="strategy"){if(n.weights?.length){let start=-Math.PI/2;for(const weight of n.weights){const end=start+Math.PI*2*Number(weight||0);ctx.beginPath();ctx.arc(n.x,n.y,n.r+8,start,end);ctx.strokeStyle=weight>.5?"#34d399":"rgba(124,131,255,.80)";ctx.lineWidth=4;ctx.stroke();start=end}}}}
  ctx.font="700 12px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif";ctx.textBaseline="middle";ctx.textAlign="left";
  const labelNodes=[...model.nodes.filter(n=>n.kind==="strategy"),...model.nodes.filter(n=>n.kind==="capability"||n.kind==="output")].filter((n,i,a)=>a.findIndex(x=>x.id===n.id)===i).slice(0,12);
  for(const n of labelNodes){const label=graphNodeLabel(n);if(!label)continue;const x=n.x+n.r+9,y=n.y;ctx.font=n.kind==="strategy"?"800 13px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif":"700 11px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif";const text=label;const pad=5,tw=ctx.measureText(text).width;ctx.fillStyle="rgba(6,8,15,.78)";ctx.strokeStyle="rgba(255,255,255,.12)";ctx.lineWidth=1;ctx.beginPath();canvasRoundRect(ctx,x-1,y-10,tw+pad*2,20,6);ctx.fill();ctx.stroke();ctx.fillStyle=n.kind==="strategy"?"#fff7b8":"#e5eefc";ctx.fillText(text,x+pad,y)}
}
function renderGraph(){
  if(state.graphFrame)cancelAnimationFrame(state.graphFrame);
  const inspect=state.inspect||{};const strategies=state.report?.strategies||[];
  $("g-nodes").textContent=inspect.nodes??"-";$("g-edges").textContent=inspect.edges??"-";$("g-strategies").textContent=strategies.length;$("g-contexts").textContent=(state.contexts||[]).length;
  $("graph-mode").textContent=(inspect.nodes||0)+" nodes";
  $("graph-note").textContent=strategies.length?"Layered runtime graph: "+strategies.length+" strategy node"+(strategies.length===1?"":"s")+", "+(state.contexts||[]).length+" context"+(((state.contexts||[]).length===1)?"":"s")+". Yellow nodes are learnable decisions.":"Layered runtime graph — connect a capsule with strategy nodes to see learned decision points.";
  const shell=$("graph-shell");if(!shell||!state.inspect)return;
  const rect=shell.getBoundingClientRect();const model=makeGraphModel(Math.max(320,rect.width),Math.max(300,rect.height));
  const frame=t=>{drawGraph(model,t);state.graphFrame=requestAnimationFrame(frame)};
  state.graphFrame=requestAnimationFrame(frame);
}

async function createJob(){
  const tenant=$("job-tenant").value.trim();const id=$("job-id").value.trim();const name=$("job-name").value.trim();const desc=$("job-desc").value.trim();
  if(!tenant||!id){setError("Tenant and job id required");return}
  try{
    const r=await fetch("/tenants/"+enc(tenant)+"/jobs",{method:"POST",headers:auth(),body:JSON.stringify({id,name,description:desc,metadata:{source:"console"}})});
    if(!r.ok)throw new Error("create job -> HTTP "+r.status);
    await loadInventory();renderInventory();setError("");
  }catch(e){setError(e.message)}
}

function applyTab(){
  document.querySelectorAll(".tab").forEach(b=>b.classList.toggle("active",b.dataset.tab===state.activeTab));
  document.querySelectorAll("[data-panel]").forEach(p=>p.classList.toggle("hide",p.dataset.panel!==state.activeTab));
  if(state.activeTab==="overview"&&state.inspect)renderGraph();
  else if(state.graphFrame){cancelAnimationFrame(state.graphFrame);state.graphFrame=null}
}

/* Wire up */
$("connect").onclick=connect;
$("refresh-main").onclick=async()=>{if(state.selected)await loadCapsule(state.selected)};
$("create-job").onclick=createJob;
$("tree-filter").oninput=renderInventory;
document.querySelectorAll(".tab").forEach(b=>b.onclick=()=>{state.activeTab=b.dataset.tab;applyTab()});
$("key").addEventListener("keydown",e=>{if(e.key==="Enter")connect()});
const saved=sessionStorage.getItem("lycanKey");if(saved){$("key").value=saved;connect()}
applyTab();
window.addEventListener("resize",()=>{if(state.activeTab==="overview"&&state.inspect)renderGraph()});
</script>
</body>
</html>
"##;
