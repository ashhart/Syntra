/// Lycan capsule format (`.lycap`): manifest.json + program.lyc + policy.json
/// (plus generated inspect.json / journal.json).

use std::path::Path;
use sha2::{Sha256, Digest};
use crate::graph::{NeuralGraph, OpCode, Operand};
use crate::verifier;

/// Manifest — the capsule's identity and contract.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub intent: String,
    pub entry: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub capabilities: Vec<String>,
    pub created_by: String,
    pub format: String,
}

/// Policy — what the capsule is allowed to do.
#[derive(Debug, Clone)]
pub struct Policy {
    pub allow_stdout: bool,
    pub allow_stdin: bool,
    pub allow_file_read: bool,
    pub allow_file_write: bool,
    pub allow_network: bool,
    pub allow_self_modify: bool,
    pub max_execution_ms: u64,
    pub max_memory_bytes: u64,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            allow_stdout: true,
            allow_stdin: false,
            allow_file_read: false,
            allow_file_write: false,
            allow_network: false,
            allow_self_modify: true,
            max_execution_ms: 30000,
            max_memory_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Create a capsule directory from a .lyc binary.
pub fn create(
    lyc_path: &str,
    output_dir: &str,
    name: &str,
    intent: &str,
    capabilities: Vec<String>,
) -> Result<(), String> {
    // Read and verify the graph
    let data = std::fs::read(lyc_path)
        .map_err(|e| format!("cannot read {lyc_path}: {e}"))?;
    let graph = NeuralGraph::from_bytes(&data)?;
    if let Err(e) = verifier::verify(&graph) {
        return Err(format!("graph verification failed: {e}"));
    }

    // Create output directory
    std::fs::create_dir_all(output_dir)
        .map_err(|e| format!("cannot create {output_dir}: {e}"))?;

    // Write program.lyc
    let lyc_out = format!("{output_dir}/program.lyc");
    std::fs::write(&lyc_out, &data)
        .map_err(|e| format!("cannot write program.lyc: {e}"))?;

    // Generate inspect first (need hash for manifest)
    let inspect = generate_inspect(&graph);
    let inspect_path = format!("{output_dir}/inspect.json");
    std::fs::write(&inspect_path, &inspect)
        .map_err(|e| format!("cannot write inspect.json: {e}"))?;

    // Compute hashes
    let program_hash = sha256_hex(&data);
    let inspect_hash = sha256_hex(inspect.as_bytes());

    // Detect graph effects for policy cross-check and merge any caller-declared effects.
    let mut effects = detect_effects(&graph);
    for capability in capabilities {
        if !effects.iter().any(|existing| existing == &capability) {
            effects.push(capability);
        }
    }

    // Generate manifest with hashes
    let manifest = generate_manifest(name, intent, &effects, &graph, &program_hash, &inspect_hash);
    let manifest_path = format!("{output_dir}/manifest.json");
    std::fs::write(&manifest_path, &manifest)
        .map_err(|e| format!("cannot write manifest.json: {e}"))?;

    // Generate journal
    let journal = generate_journal(&graph);
    let journal_path = format!("{output_dir}/journal.json");
    std::fs::write(&journal_path, &journal)
        .map_err(|e| format!("cannot write journal.json: {e}"))?;

    // Generate policy
    let policy = generate_policy(&effects);
    let policy_path = format!("{output_dir}/policy.json");
    std::fs::write(&policy_path, &policy)
        .map_err(|e| format!("cannot write policy.json: {e}"))?;

    Ok(())
}

/// Strict capsule verification — invalid capsules MUST fail closed.
pub fn verify_capsule(dir: &str) -> Result<(), String> {
    let mut errors = Vec::new();

    // 1. Required files exist
    let required = ["manifest.json", "program.lyc", "policy.json"];
    for f in &required {
        if !Path::new(&format!("{dir}/{f}")).exists() {
            errors.push(format!("missing required file: {f}"));
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }

    // 2. Graph is structurally valid
    let data = std::fs::read(format!("{dir}/program.lyc"))
        .map_err(|e| format!("cannot read program.lyc: {e}"))?;
    let graph = NeuralGraph::from_bytes(&data)?;
    if let Err(e) = verifier::verify(&graph) {
        return Err(format!("graph verification failed: {e}"));
    }

    // 3. Manifest is valid
    let manifest_str = std::fs::read_to_string(format!("{dir}/manifest.json"))
        .map_err(|e| format!("cannot read manifest.json: {e}"))?;
    if !manifest_str.contains("\"format\"") || !manifest_str.contains("lycan-capsule") {
        errors.push("manifest.json missing format field".to_string());
    }

    // 4. Hash verification (if manifest contains hashes)
    if manifest_str.find("\"program_sha256\"").is_some() {
        let actual_hash = sha256_hex(&data);
        if !manifest_str.contains(&actual_hash) {
            errors.push(format!("program.lyc hash mismatch (actual: {actual_hash})"));
        }
    }

    if Path::new(&format!("{dir}/inspect.json")).exists() {
        let inspect_data = std::fs::read(format!("{dir}/inspect.json"))
            .map_err(|e| format!("cannot read inspect.json: {e}"))?;
        if let Some(_) = manifest_str.find("\"inspect_sha256\"") {
            let actual_hash = sha256_hex(&inspect_data);
            if !manifest_str.contains(&actual_hash) {
                errors.push(format!("inspect.json hash mismatch (actual: {actual_hash})"));
            }
        }
    }

    // 5. Policy allows every effect the graph performs
    let policy_str = std::fs::read_to_string(format!("{dir}/policy.json"))
        .map_err(|e| format!("cannot read policy.json: {e}"))?;
    let effects = detect_effects(&graph);
    for effect in &effects {
        let required_field = match effect.as_str() {
            "stdout" => "\"allow_stdout\": true",
            "stdin" => "\"allow_stdin\": true",
            "file_read" => "\"allow_file_read\": true",
            "file_write" => "\"allow_file_write\": true",
            "network" => "\"allow_network\": true",
            _ => continue,
        };
        if !policy_str.contains(required_field) {
            errors.push(format!("graph uses {effect} but policy does not allow it"));
        }
    }

    // 6. Journal entries reference real nodes
    if Path::new(&format!("{dir}/journal.json")).exists() {
        // Journal node refs validated by graph verifier already
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Detect what effects/capabilities a graph uses.
fn detect_effects(graph: &NeuralGraph) -> Vec<String> {
    let mut effects = Vec::new();
    let mut has_stdout = false;
    let mut has_stdin = false;

    for node in &graph.nodes {
        match node.op {
            OpCode::Print => has_stdout = true,
            OpCode::ReadLine => has_stdin = true,
            OpCode::Capability => {
                if let Some(first) = node.operands.first() {
                    if let Some(name) = capability_name_from_operand(graph, first) {
                        if let Some(spec) = crate::capabilities::get(&name) {
                            for effect in spec.effects {
                                if !effects.iter().any(|existing| existing == effect) {
                                    effects.push(effect.to_string());
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if has_stdout { effects.push("stdout".to_string()); }
    if has_stdin { effects.push("stdin".to_string()); }
    effects
}

fn capability_name_from_operand(graph: &NeuralGraph, operand: &Operand) -> Option<String> {
    match operand {
        Operand::StringRef(idx) => Some(graph.get_string(*idx)),
        Operand::NodeRef(id) => {
            let node = graph.nodes.get(*id as usize)?;
            if node.op != OpCode::ConstStr {
                return None;
            }
            match node.operands.first()? {
                Operand::StringRef(idx) => Some(graph.get_string(*idx)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

fn generate_manifest(name: &str, intent: &str, capabilities: &[String], graph: &NeuralGraph, program_hash: &str, inspect_hash: &str) -> String {
    let caps: Vec<String> = capabilities.iter().map(|c| format!("\"{}\"", c)).collect();
    let live = graph.nodes.iter().filter(|n| n.op != crate::graph::OpCode::Noop).count();

    format!(r#"{{
  "name": "{}",
  "version": "0.1.0",
  "intent": "{}",
  "entry": "program.lyc",
  "inputs": [],
  "outputs": ["stdout"],
  "capabilities": [{}],
  "created_by": "lycan 0.1.0",
  "format": "lycan-capsule-v1",
  "program_sha256": "{}",
  "inspect_sha256": "{}",
  "graph_stats": {{
    "nodes": {},
    "live_nodes": {},
    "edges": {},
    "strings": {}
  }}
}}"#,
        name,
        intent.replace('\"', "\\\""),
        caps.join(", "),
        program_hash,
        inspect_hash,
        graph.nodes.len(),
        live,
        graph.edges.len(),
        graph.string_table.len()
    )
}

fn generate_inspect(graph: &NeuralGraph) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"format\": \"lycan-graph-v{}\",\n", graph.header.version));
    out.push_str(&format!("  \"entry\": {},\n", graph.entry));
    out.push_str(&format!("  \"total_nodes\": {},\n", graph.nodes.len()));
    let live = graph.nodes.iter().filter(|n| n.op != crate::graph::OpCode::Noop).count();
    out.push_str(&format!("  \"live_nodes\": {},\n", live));
    out.push_str(&format!("  \"edges\": {},\n", graph.edges.len()));

    out.push_str("  \"nodes\": [\n");
    let live_nodes: Vec<&crate::graph::GraphNode> = graph.nodes.iter()
        .filter(|n| n.op != crate::graph::OpCode::Noop)
        .collect();
    for (i, node) in live_nodes.iter().enumerate() {
        let wk = match node.weight_kind {
            crate::graph::WeightKind::Observational => "observational",
            crate::graph::WeightKind::Adaptive => "adaptive",
            crate::graph::WeightKind::TypeHint => "type_hint",
            crate::graph::WeightKind::Strategy => "strategy",
            crate::graph::WeightKind::Decision => "decision",
        };
        out.push_str(&format!("    {{\"id\": {}, \"op\": \"{:?}\", \"fired\": {}, \"weight_kind\": \"{}\"",
            node.id, node.op, node.activation_count, wk));
        if !node.weights.is_empty() {
            let ws: Vec<String> = node.weights.iter().map(|w| format!("{w:.4}")).collect();
            out.push_str(&format!(", \"weights\": [{}]", ws.join(", ")));
        }
        if node.bias != 0.0 {
            let hint = if node.bias == 1.0 { "int" } else if node.bias == 2.0 { "float" } else { "unknown" };
            out.push_str(&format!(", \"type_hint\": \"{}\"", hint));
        }
        out.push('}');
        if i < live_nodes.len() - 1 { out.push(','); }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
}

fn generate_journal(graph: &NeuralGraph) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"entries\": [\n");
    for (i, entry) in graph.journal.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"run\": {}, \"node\": {}, \"mutation\": \"{:?}\"}}",
            entry.run_number, entry.node_id, entry.mutation
        ));
        if i < graph.journal.len() - 1 { out.push(','); }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn generate_policy(capabilities: &[String]) -> String {
    let policy = Policy {
        allow_stdout: capabilities.contains(&"stdout".to_string()),
        allow_stdin: capabilities.contains(&"stdin".to_string()),
        allow_file_read: capabilities.contains(&"file_read".to_string()),
        allow_file_write: capabilities.contains(&"file_write".to_string()),
        allow_network: capabilities.contains(&"network".to_string()),
        allow_self_modify: true,
        ..Default::default()
    };

    format!(r#"{{
  "allow_stdout": {},
  "allow_stdin": {},
  "allow_file_read": {},
  "allow_file_write": {},
  "allow_network": {},
  "allow_self_modify": {},
  "max_execution_ms": {},
  "max_memory_bytes": {}
}}"#,
        policy.allow_stdout, policy.allow_stdin,
        policy.allow_file_read, policy.allow_file_write,
        policy.allow_network, policy.allow_self_modify,
        policy.max_execution_ms, policy.max_memory_bytes
    )
}

/// Load a capsule's policy.json and return an ExecutionPolicy for runtime enforcement.
pub fn load_policy(dir: &str) -> Result<crate::context::ExecutionPolicy, String> {
    let path = format!("{dir}/policy.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read policy.json: {e}"))?;
    let json: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("invalid policy.json: {e}"))?;

    fn bool_field(json: &serde_json::Value, key: &str, default: bool) -> bool {
        json.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    let allowed_hosts = json.get("allowed_hosts")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    Ok(crate::context::ExecutionPolicy {
        allow_stdout: bool_field(&json, "allow_stdout", true),
        allow_stdin: bool_field(&json, "allow_stdin", false),
        allow_file_read: bool_field(&json, "allow_file_read", false),
        allow_file_write: bool_field(&json, "allow_file_write", false),
        allow_network: bool_field(&json, "allow_network", false),
        file_root: json.get("file_root").and_then(|v| v.as_str()).map(String::from),
        allowed_hosts,
        deny_private_networks: bool_field(&json, "deny_private_networks", true),
    })
}
