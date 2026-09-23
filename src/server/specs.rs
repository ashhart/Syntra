//! Specs from files at startup (`syntra serve --specs <dir>`), for
//! deployments that keep capsule configuration in version control (a Helm
//! values file, a ConfigMap, a GitOps repository).
//!
//! Every `*.json`, `*.yaml` or `*.yml` file in the directory holds one or
//! more documents (YAML documents separated by `---`, or a JSON array):
//!
//! ```yaml
//! tenant: acme
//! job: prod
//! capsule: router
//! spec:
//!   actions: [{id: small}, {id: large}]
//!   reward: {default: 0, waitSeconds: 600}
//! ```
//!
//! Each spec replaces the capsule's stored spec (fields it leaves out take
//! their defaults, as with `PUT .../spec?replace=true`), but only when it
//! differs, so restarts do not churn snapshots or the audit trail. Applied
//! specs are audited as `spec_applied` with the file name. Any invalid
//! file stops the server from starting: a bad deployment should not serve.

use std::path::Path;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::decision::DecisionSpec;

use super::capsules::{SpecChange, change_spec};
use super::state::State;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpecDoc {
    tenant: String,
    job: String,
    capsule: String,
    spec: Value,
}

/// What `apply_dir` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub files: usize,
    pub applied: Vec<String>,
    pub unchanged: Vec<String>,
}

fn documents(path: &Path) -> Result<Vec<SpecDoc>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.display();
    let values: Vec<Value> = if path.extension().is_some_and(|e| e == "json") {
        match serde_json::from_str::<Value>(&text).map_err(|e| format!("{name}: {e}"))? {
            Value::Array(items) => items,
            one => vec![one],
        }
    } else {
        let mut out = Vec::new();
        for doc in serde_norway::Deserializer::from_str(&text) {
            let v = Value::deserialize(doc).map_err(|e| format!("{name}: {e}"))?;
            if !v.is_null() {
                out.push(v);
            }
        }
        out
    };
    values
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            serde_json::from_value::<SpecDoc>(v)
                .map_err(|e| format!("{name}, document {}: {e}", i + 1))
        })
        .collect()
}

/// Apply every spec document under `dir`. Validates everything before
/// changing anything.
pub fn apply_dir(state: &State, dir: &Path) -> Result<Applied, String> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("--specs {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e, "json" | "yaml" | "yml"))
        })
        .collect();
    paths.sort();

    let mut plan = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in &paths {
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for doc in documents(path)? {
            for name in [&doc.tenant, &doc.job, &doc.capsule] {
                crate::store::validate_name(name).map_err(|e| format!("{file}: {e}"))?;
            }
            let address = format!("{}/{}/{}", doc.tenant, doc.job, doc.capsule);
            if !seen.insert(address.clone()) {
                return Err(format!("{file}: {address} is specified more than once"));
            }
            let spec = DecisionSpec::default()
                .merge_patch(&doc.spec)
                .map_err(|e| format!("{file}: {address}: {e}"))?;
            plan.push((file.clone(), doc, spec, address));
        }
    }

    let mut out = Applied {
        files: paths.len(),
        ..Applied::default()
    };
    for (file, doc, spec, address) in plan {
        let stored = state
            .store
            .load_spec(&doc.tenant, &doc.job, &doc.capsule)
            .ok()
            .flatten();
        if stored.as_ref() == Some(&spec) {
            out.unchanged.push(address);
            continue;
        }
        change_spec(
            state,
            &doc.tenant,
            &doc.job,
            &doc.capsule,
            SpecChange {
                patch: &doc.spec,
                event: "spec_applied",
                expect_base: None,
                audit_extra: json!({ "file": file }),
                replace: true,
            },
        )
        .map_err(|r| format!("{file}: {address}: {}", String::from_utf8_lossy(&r.body)))?;
        out.applied.push(address);
    }
    Ok(out)
}
