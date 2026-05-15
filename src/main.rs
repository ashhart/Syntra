use lycan::*;
use std::io::{self, Write};

fn main() {
    // Large stack for deep graph recursion (fib(20) = ~21K recursive calls)
    let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
    let handler = builder.spawn(|| { main_inner(); }).unwrap();
    handler.join().unwrap();
}

fn main_inner() {
    let args: Vec<String> = std::env::args().collect();

    // Variable-arity commands — check before length-based dispatch
    if args.len() >= 2 && args[1] == "serve" {
        cli_serve(&args[2..]);
        return;
    }
    if args.len() >= 3 && args[1] == "evolve" {
        cli_evolve(&args[2..]);
        return;
    }
    if args.len() >= 5 && args[1] == "feedback" {
        cli_feedback(&args[2..]);
        return;
    }

    match args.len() {
        1 => repl(),
        2 => {
            match args[1].as_str() {
                "--help" | "-h" => print_usage(),
                "capabilities" => list_capabilities(),
                _ => run_file(&args[1]),
            }
        }
        3 => {
            match args[1].as_str() {
                "compile" => compile_to_neural(&args[2]),
                "explain" => explain_file(&args[2]),
                "inspect" => inspect_json(&args[2]),
                "dump" => dump_graph(&args[2]),
                "stats" => show_stats(&args[2]),
                "learn-report" => learn_report(&args[2]),
                "decision-report" => decision_report(&args[2]),
                "improve-report" => cli_improve_report(&args[2]),
                "decide" => cli_decide(&args[2]),
                _ => {
                    eprintln!("unknown command '{}'", args[1]);
                    print_usage();
                }
            }
        }
        4 => {
            match args[1].as_str() {
                "transfer-weights" => evolve_program(&args[2], &args[3]),
                "capsule" => {
                    match args[2].as_str() {
                        "verify" => capsule_verify(&args[3]),
                        "inspect" => capsule_inspect(&args[3]),
                        "run" => capsule_run(&args[3]),
                        "improve" => capsule_improve(&args[3]),
                        _ => print_usage(),
                    }
                }
                _ => print_usage(),
            }
        }
        5 => {
            if args[1] == "capsule" && args[2] == "apply-proposal" {
                capsule_apply_proposal(&args[3], &args[4]);
            } else if args[1] == "capsule" && args[2] == "create" {
                capsule_create(&args[3], &args[4], "no intent specified");
            } else if args[1] == "decide" && args[3] == "--input" {
                cli_decide_with_input(&args[2], &args[4]);
            } else {
                print_usage();
            }
        }
        _ => {
            if args.len() >= 6 && args[1] == "capsule" && args[2] == "create" {
                capsule_create(&args[3], &args[4], &args[5]);
            } else {
                print_usage();
            }
        }
    }
}

fn print_usage() {
    eprintln!("Lycan — AI-native graph runtime");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  lycan                     Interactive REPL");
    eprintln!("  lycan <file.lycs>         Run source directly");
    eprintln!("  lycan <file.lyc>          Execute graph binary");
    eprintln!("  lycan compile <file.lycs> Compile to .lyc graph binary");
    eprintln!("  lycan explain <file.lyc>  Translate binary to text");
    eprintln!("  lycan inspect <file.lyc>  AI-readable JSON graph view");
    eprintln!("  lycan capabilities        List native capability registry");
    eprintln!("  lycan dump <file.lyc>     Dump graph binary hex");
    eprintln!("  lycan stats <file.lyc>    Show evolution statistics");
    eprintln!("  lycan learn-report <f.lyc> Show strategy learning report");
    eprintln!("  lycan transfer-weights <a.lyc> <b.lyc>  Transfer learned weights");
    eprintln!("  lycan evolve <path> [flags]   Autonomous evolution loop");
    eprintln!("    --proposal <file>           Apply local proposal JSON");
    eprintln!("    --agent-command <cmd>       Send brief to agent subprocess");
    eprintln!("    --no-agent                  Generate brief only");
    eprintln!("    --iterations <n>            Max iterations (default 1)");
    eprintln!("    --min-improvement <f>       Minimum improvement threshold (default 0.05)");
    eprintln!("    --budget-ms <n>             Time budget in ms (default 60000)");
    eprintln!("    --dry-run                   Verify but never mutate");
    eprintln!("  lycan serve [--addr 127.0.0.1:8787] [--store ./lycan-store] [--admin-key <key>]");
    eprintln!("  lycan decide <f.lyc> --input <request.json>  Decide with injected JSON");
    eprintln!("  lycan feedback <f.lyc> <node> --option <n> --reward <f> [--success <bool>]");
    eprintln!("  lycan capsule create <file.lyc> <name> <intent>");
    eprintln!("  lycan capsule verify <dir>");
    eprintln!("  lycan capsule inspect <dir>");
    eprintln!("  lycan capsule run <dir>");
    eprintln!("  lycan capsule improve <file.lyc>     Emit improvement brief");
    eprintln!("  lycan capsule apply-proposal <file.lyc> <proposal.json>");
}

fn cli_serve(args: &[String]) {
    let mut addr = "127.0.0.1:8787".to_string();
    let mut store_path = "./lycan-store".to_string();
    let mut admin_key: Option<String> = std::env::var("LYCAN_ADMIN_KEY").ok();
    let mut dev_mode = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => { i += 1; if let Some(v) = args.get(i) { addr = v.clone(); } }
            "--store" => { i += 1; if let Some(v) = args.get(i) { store_path = v.clone(); } }
            "--admin-key" => { i += 1; if let Some(v) = args.get(i) { admin_key = Some(v.clone()); } }
            "--dev-mode" => { dev_mode = true; }
            _ => {}
        }
        i += 1;
    }

    if admin_key.is_none() && !dev_mode {
        eprintln!("ERROR: no admin key set. Set LYCAN_ADMIN_KEY or use --admin-key.");
        eprintln!("  For unauthenticated development, use --dev-mode (binds localhost only).");
        std::process::exit(1);
    }

    if dev_mode && admin_key.is_none() {
        eprintln!("WARNING: running in dev mode — all routes unauthenticated");
        if !addr.starts_with("127.0.0.1") && !addr.starts_with("localhost") {
            eprintln!("WARNING: dev mode on non-loopback address {addr} — this is unsafe");
        }
    }

    server::run_server(server::ServerConfig {
        addr,
        store_path,
        admin_key,
        service_name: Some("Lycan".to_string()),
    });
}

fn list_capabilities() {
    println!("{}", capabilities::json_catalog());
}

fn run_file(path: &str) {
    if path.ends_with(".lyc") {
        run_binary(path);
    } else {
        run_source(path);
    }
}

fn run_source(path: &str) {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    match execute_source(&src) {
        Ok(_) => {}
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}

fn run_binary(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };

    // Compiled graph format (v2)
    if data.len() >= 4 && data[0] == 0x4C && data[1] == 0x59 && data[2] == 0x43 && data[3] == 0x4E {
        let ng = match graph::NeuralGraph::from_bytes(&data) {
            Ok(g) => g,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        // Verify before execution — invalid graphs must fail closed
        if let Err(e) = verifier::verify(&ng) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        let mut executor = graph_executor::GraphExecutor::new(ng);
        match executor.run() {
            Ok(_) => {}
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        }
        // Self-optimization: weight tracking, specialization, constant folding
        // Pruning is disabled by default — safe for programs with varying input
        let mut updated = executor.into_graph();
        let stats = optimizer::optimize(&mut updated);
        if stats.nodes_specialized > 0 || stats.nodes_cached > 0 {
            optimizer::print_stats(&stats);
        }
        // Save evolved program back
        let updated_bytes = updated.to_bytes();
        if let Err(e) = std::fs::write(path, &updated_bytes) {
            eprintln!("warning: could not save updated weights: {e}");
        }
        return;
    }

    // Legacy AST format (v1)
    let program = match binary::decode(&data) {
        Ok(p) => p,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };
    let mut interp = interpreter::Interpreter::new();
    match interp.run(&program) {
        Ok(_) => {}
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}

fn compile_to_neural(path: &str) {
    let out = path.replace(".lycs", ".lyc");
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    let program = match parse_source(&src) {
        Ok(p) => p,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };
    let compiler = graph_compiler::GraphCompiler::new();
    let neural = compiler.compile(&program);
    let data = neural.to_bytes();
    match std::fs::write(&out, &data) {
        Ok(_) => eprintln!(
            "compiled {} -> {} ({} bytes, {} nodes, {} edges)",
            path, out, data.len(), neural.nodes.len(), neural.edges.len()
        ),
        Err(e) => { eprintln!("error writing {out}: {e}"); std::process::exit(1); }
    }
}

/// AI-readable JSON introspection of a .lyc graph.
/// This is what LLMs should read to understand and optimize Lycan programs.
fn inspect_json(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    println!("{{");
    println!("  \"format\": \"lycan-graph-v{}\",", ng.header.version);
    println!("  \"entry\": {},", ng.entry);
    println!("  \"total_nodes\": {},", ng.nodes.len());
    println!("  \"live_nodes\": {},", ng.nodes.iter().filter(|n| n.op != graph::OpCode::Noop).count());
    println!("  \"edges\": {},", ng.edges.len());
    println!("  \"strings\": {},", ng.string_table.len());
    println!("  \"journal_entries\": {},", ng.journal.len());

    let used_capabilities = capabilities_used_by_graph(&ng);
    println!("  \"capabilities_used\": [");
    for (i, cap) in used_capabilities.iter().enumerate() {
        if let Some(spec) = capabilities::get(cap) {
            print!("{}", capabilities::spec_json(spec, 4));
        } else {
            print!("    {{\"name\": \"{}\", \"known\": false}}", cap.replace('\"', "\\\""));
        }
        if i < used_capabilities.len() - 1 {
            print!(",");
        }
        println!();
    }
    println!("  ],");

    // Nodes — only live ones
    println!("  \"nodes\": [");
    let live: Vec<&graph::GraphNode> = ng.nodes.iter()
        .filter(|n| n.op != graph::OpCode::Noop)
        .collect();
    for (i, node) in live.iter().enumerate() {
        let op_name = format!("{:?}", node.op);
        let wk = match node.weight_kind {
            graph::WeightKind::Observational => "observational",
            graph::WeightKind::Adaptive => "adaptive",
            graph::WeightKind::TypeHint => "type_hint",
            graph::WeightKind::Strategy => "strategy",
            graph::WeightKind::Decision => "decision",
        };
        let weights_str: Vec<String> = node.weights.iter().map(|w| format!("{w:.4}")).collect();
        let annotation = node.annotation
            .map(|idx| ng.get_string(idx))
            .unwrap_or_default();

        let operand_strs: Vec<String> = node.operands.iter().map(|op| {
            match op {
                graph::Operand::NodeRef(id) => format!("{{\"ref\": {id}}}"),
                graph::Operand::Immediate(graph::ImmValue::Int(n)) => format!("{{\"int\": {n}}}"),
                graph::Operand::Immediate(graph::ImmValue::Float(f)) => format!("{{\"float\": {f}}}"),
                graph::Operand::Immediate(graph::ImmValue::Bool(b)) => format!("{{\"bool\": {b}}}"),
                graph::Operand::Immediate(graph::ImmValue::Null) => "\"null\"".to_string(),
                graph::Operand::StateRef(idx) => format!("{{\"state\": {idx}}}"),
                graph::Operand::StringRef(idx) => {
                    let s = ng.get_string(*idx).replace('\"', "\\\"");
                    format!("{{\"str\": \"{s}\"}}")
                }
                graph::Operand::VarSlot(slot) => format!("{{\"var\": {slot}}}"),
            }
        }).collect();

        print!("    {{\"id\": {}, \"op\": \"{}\", \"fired\": {}", node.id, op_name, node.activation_count);
        if !node.weights.is_empty() {
            print!(", \"weights\": [{}], \"weight_kind\": \"{}\"", weights_str.join(", "), wk);
        }
        if node.bias != 0.0 {
            print!(", \"type_hint\": {}", if node.bias == 1.0 { "\"int\"" } else if node.bias == 2.0 { "\"float\"" } else { "\"unknown\"" });
        }
        if !annotation.is_empty() {
            print!(", \"meaning\": \"{}\"", annotation.replace('\"', "\\\""));
        }
        if !operand_strs.is_empty() {
            print!(", \"operands\": [{}]", operand_strs.join(", "));
        }
        print!("}}");
        if i < live.len() - 1 { print!(","); }
        println!();
    }
    println!("  ],");

    // Edges
    println!("  \"edges\": [");
    for (i, edge) in ng.edges.iter().enumerate() {
        print!("    {{\"from\": {}, \"to\": {}, \"weight\": {:.4}", edge.from, edge.to, edge.weight);
        if let Some(g) = edge.gate { print!(", \"gate\": {g}"); }
        print!("}}");
        if i < ng.edges.len() - 1 { print!(","); }
        println!();
    }
    println!("  ],");

    // Journal
    println!("  \"journal\": [");
    for (i, entry) in ng.journal.iter().enumerate() {
        let mutation = format!("{:?}", entry.mutation);
        print!("    {{\"run\": {}, \"node\": {}, \"mutation\": \"{}\"", entry.run_number, entry.node_id, mutation);
        if entry.reason != u32::MAX {
            let reason = ng.get_string(entry.reason);
            if !reason.is_empty() {
                print!(", \"reason\": \"{}\"", reason.replace('\"', "\\\""));
            }
        }
        print!("}}");
        if i < ng.journal.len() - 1 { print!(","); }
        println!();
    }
    println!("  ]");

    println!("}}");
}

fn capabilities_used_by_graph(ng: &graph::NeuralGraph) -> Vec<String> {
    let mut out = Vec::new();
    for node in &ng.nodes {
        if node.op != graph::OpCode::Capability {
            continue;
        }
        let Some(first) = node.operands.first() else {
            continue;
        };
        let Some(name) = capability_name_from_operand(ng, first) else {
            continue;
        };
        if !out.iter().any(|existing| existing == &name) {
            out.push(name);
        }
    }
    out.sort();
    out
}

fn capability_name_from_operand(ng: &graph::NeuralGraph, operand: &graph::Operand) -> Option<String> {
    match operand {
        graph::Operand::StringRef(idx) => Some(ng.get_string(*idx)),
        graph::Operand::NodeRef(id) => {
            let node = ng.nodes.get(*id as usize)?;
            if node.op != graph::OpCode::ConstStr {
                return None;
            }
            match node.operands.first()? {
                graph::Operand::StringRef(idx) => Some(ng.get_string(*idx)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Decode binary and print source (old format)
fn explain_file(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    // Try compiled graph format first
    if data.len() >= 4 && data[0] == 0x4C && data[1] == 0x59 && data[2] == 0x43 && data[3] == 0x4E {
        let ng = match graph::NeuralGraph::from_bytes(&data) {
            Ok(g) => g,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        println!("Neural Graph v{}", ng.header.version);
        println!("  Nodes: {}", ng.nodes.len());
        println!("  Edges: {}", ng.edges.len());
        println!("  Strings: {}", ng.string_table.len());
        println!("  Entry: node #{}", ng.entry);
        println!();
        for node in &ng.nodes {
            let op_name = format!("{:?}", node.op);
            let weights: Vec<String> = node.weights.iter().map(|w| format!("{w:.3}")).collect();
            let w_str = if weights.is_empty() { String::new() } else { format!(" w[{}]", weights.join(",")) };
            println!("  #{:04} {:12} operands:{} fired:{}{}",
                node.id, op_name, node.operands.len(), node.activation_count, w_str);
        }
    } else {
        // Old format
        let program = match binary::decode(&data) {
            Ok(p) => p,
            Err(e) => { eprintln!("{e}"); std::process::exit(1); }
        };
        for node in &program.nodes {
            println!("{}", node_to_source(node));
        }
    }
}

/// Dump raw hex of compiled graph
fn dump_graph(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    // Print hex dump like machine code
    for (i, chunk) in data.chunks(16).enumerate() {
        print!("{:08x}  ", i * 16);
        for (j, byte) in chunk.iter().enumerate() {
            print!("{:02x} ", byte);
            if j == 7 { print!(" "); }
        }
        // Pad if short
        for _ in chunk.len()..16 {
            print!("   ");
        }
        print!(" |");
        for byte in chunk {
            if byte.is_ascii_graphic() || *byte == b' ' {
                print!("{}", *byte as char);
            } else {
                print!(".");
            }
        }
        println!("|");
    }
}

/// Show evolution statistics for a .lyc program
fn show_stats(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    let total_nodes = ng.nodes.len();
    let live_nodes = ng.nodes.iter().filter(|n| n.op != graph::OpCode::Noop).count();
    let dead_nodes = total_nodes - live_nodes;
    let total_activations: u64 = ng.nodes.iter().map(|n| n.activation_count).sum();
    let max_activations = ng.nodes.iter().map(|n| n.activation_count).max().unwrap_or(0);
    let specialized = ng.nodes.iter().filter(|n| n.bias != 0.0).count();

    let branches: Vec<&graph::GraphNode> = ng.nodes.iter()
        .filter(|n| matches!(n.op,
            graph::OpCode::Branch | graph::OpCode::AdaptiveChoice | graph::OpCode::Strategy))
        .collect();
    let converged = branches.iter()
        .filter(|n| n.weights.iter().any(|w| *w > 0.9 || *w < 0.1))
        .count();

    println!("=== {} ===", path);
    println!("  Nodes:          {} total, {} live, {} pruned", total_nodes, live_nodes, dead_nodes);
    println!("  Edges:          {}", ng.edges.len());
    println!("  Strings:        {}", ng.string_table.len());
    println!("  Total fired:    {}", total_activations);
    println!("  Max fired:      {} (hottest node)", max_activations);
    println!("  Specialized:    {} nodes", specialized);
    println!("  Branches:       {}", branches.len());
    println!("  Converged:      {} ({:.0}% of branches learned a preference)",
        converged,
        if branches.is_empty() { 0.0 } else { converged as f64 / branches.len() as f64 * 100.0 });
    println!("  Binary size:    {} bytes", data.len());

    if !branches.is_empty() {
        println!();
        println!("  Weighted nodes:");
        for b in &branches {
            let ws: Vec<String> = b.weights.iter().map(|w| format!("{w:.3}")).collect();
            let kind = match b.weight_kind {
                graph::WeightKind::Observational => "obs",
                graph::WeightKind::Adaptive => "ADAPTIVE",
                graph::WeightKind::TypeHint => "type",
                graph::WeightKind::Strategy | graph::WeightKind::Decision => "STRATEGY",
            };
            let status = if b.weights.iter().any(|w| *w > 0.95) { " <- CONVERGED" }
                else if b.weights.iter().any(|w| *w > 0.8) { " <- learning" }
                else { "" };
            println!("    #{:04} {:8} fired:{:>6} w[{}]{}", b.id, kind, b.activation_count, ws.join(", "), status);
        }
    }

    // Show top 10 hottest nodes
    let mut hot: Vec<&graph::GraphNode> = ng.nodes.iter()
        .filter(|n| n.op != graph::OpCode::Noop && n.activation_count > 0)
        .collect();
    hot.sort_by(|a, b| b.activation_count.cmp(&a.activation_count));

    if !hot.is_empty() {
        println!();
        println!("  Hottest nodes:");
        for n in hot.iter().take(10) {
            println!("    #{:04} {:12?} fired:{}", n.id, n.op, n.activation_count);
        }
    }
}

/// Transfer learned weights from source to target program
fn evolve_program(source_path: &str, target_path: &str) {
    let src_data = match std::fs::read(source_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {source_path}: {e}"); std::process::exit(1); }
    };
    let src = match graph::NeuralGraph::from_bytes(&src_data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    let tgt_data = match std::fs::read(target_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {target_path}: {e}"); std::process::exit(1); }
    };
    let mut tgt = match graph::NeuralGraph::from_bytes(&tgt_data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    // Transfer: match nodes by opcode + operand count, transfer weights
    let mut transferred = 0u32;
    for tgt_node in &mut tgt.nodes {
        if tgt_node.weights.is_empty() { continue; }
        // Find matching source node
        for src_node in &src.nodes {
            if src_node.op == tgt_node.op
                && src_node.operands.len() == tgt_node.operands.len()
                && src_node.weights.len() == tgt_node.weights.len()
                && src_node.activation_count > tgt_node.activation_count
            {
                // Blend weights: 70% source, 30% target
                for (i, sw) in src_node.weights.iter().enumerate() {
                    if let Some(tw) = tgt_node.weights.get_mut(i) {
                        *tw = 0.7 * sw + 0.3 * *tw;
                    }
                }
                tgt_node.bias = src_node.bias;
                transferred += 1;
                break;
            }
        }
    }

    let out = tgt.to_bytes();
    match std::fs::write(target_path, &out) {
        Ok(_) => eprintln!("evolved {} from {} ({} nodes received learned weights)", target_path, source_path, transferred),
        Err(e) => eprintln!("error writing {target_path}: {e}"),
    }
}

/// Run a .lyc and show detailed strategy learning report.
fn learn_report(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };
    if data.len() < 4 || data[0] != 0x4C || data[1] != 0x59 {
        eprintln!("not a .lyc file"); std::process::exit(1);
    }
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    // Find strategy nodes before run
    let strategy_nodes: Vec<u32> = ng.nodes.iter()
        .filter(|n| matches!(n.op, graph::OpCode::Strategy | graph::OpCode::AdaptiveChoice))
        .map(|n| n.id)
        .collect();

    if strategy_nodes.is_empty() {
        eprintln!("no Strategy/AdaptiveChoice nodes found in graph");
        std::process::exit(1);
    }

    // READ-ONLY: load persisted stats from state vector, never execute
    let mut stats_map: std::collections::HashMap<u32, Vec<graph_executor::OptionStats>> = std::collections::HashMap::new();
    for &nid in &strategy_nodes {
        let node = &ng.nodes[nid as usize];
        if let Some(slot) = node.state_slot {
            let n = node.weights.len();
            // For WithinTolerance, last weight is epsilon, so n_options = n - 1
            let n_options = if node.contract == graph::Contract::WithinTolerance && n > 1 { n - 1 } else { n };
            let mut stats = vec![graph_executor::OptionStats::default(); n_options];
            for i in 0..n_options {
                let base = slot as usize + i * 3;
                if base + 2 < ng.state.len() {
                    stats[i].tries = ng.state[base] as u64;
                    stats[i].total_ns = ng.state[base + 1] as u128;
                    stats[i].correct = ng.state[base + 2] as u64;
                }
            }
            stats_map.insert(nid, stats);
        }
    }

    // Print learning report — zero execution, zero side effects
    println!();
    println!("=== LYCAN LEARNING REPORT ===");
    println!();

    for &node_id in &strategy_nodes {
        let node = &ng.nodes[node_id as usize];
        let op_name = format!("{:?}", node.op);
        let wk = match node.weight_kind {
            graph::WeightKind::Observational => "observational",
            graph::WeightKind::Adaptive => "adaptive",
            graph::WeightKind::Strategy => "strategy",
            graph::WeightKind::Decision => "decision",
            graph::WeightKind::TypeHint => "type_hint",
        };
        println!("  {} #{:04} ({})", op_name, node_id, wk);
        println!("  fired: {} times", node.activation_count);

        let ws: Vec<String> = node.weights.iter().map(|w| format!("{w:.4}")).collect();
        println!("  weights: [{}]", ws.join(", "));

        if let Some(stats) = stats_map.get(&node_id) {
            println!();
            for (i, s) in stats.iter().enumerate() {
                let avg_ns = if s.tries > 0 { s.total_ns / s.tries as u128 } else { 0 };
                let avg_ms = avg_ns as f64 / 1_000_000.0;
                let pct = if s.tries > 0 { s.correct as f64 / s.tries as f64 * 100.0 } else { 0.0 };
                let weight = node.weights.get(i).copied().unwrap_or(0.0);
                let marker = if weight > 0.9 { " <- WINNER" }
                    else if weight > 0.7 { " <- leading" }
                    else { "" };
                println!("  option {i}:");
                println!("    tried:    {} times", s.tries);
                println!("    avg time: {:.3}ms", avg_ms);
                println!("    correct:  {}/{} ({:.0}%)", s.correct, s.tries, pct);
                println!("    weight:   {:.4}{}", weight, marker);
            }

            // Determine winner from options that have actually been correct.
            let all_tried = stats.iter().all(|s| s.tries > 0);
            if all_tried {
                let avg_times: Vec<f64> = stats.iter().map(|s| {
                    s.total_ns as f64 / s.tries as f64
                }).collect();
                let correct_indices: Vec<usize> = stats.iter().enumerate()
                    .filter(|(_, s)| s.tries > 0 && s.correct == s.tries)
                    .map(|(i, _)| i)
                    .collect();

                if correct_indices.is_empty() {
                    println!();
                    println!("  verdict: no fully correct option yet");
                    println!();
                    continue;
                }

                let best_idx = correct_indices.iter().copied()
                    .min_by(|a, b| avg_times[*a].partial_cmp(&avg_times[*b]).unwrap())
                    .unwrap_or(0);
                let slowest_correct_idx = correct_indices.iter().copied()
                    .max_by(|a, b| avg_times[*a].partial_cmp(&avg_times[*b]).unwrap())
                    .unwrap_or(best_idx);
                let worst_idx = avg_times.iter().enumerate()
                    .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .map(|(i, _)| i).unwrap_or(0);
                let slowest_idx = avg_times.iter().enumerate()
                    .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                    .map(|(i, _)| i).unwrap_or(0);
                let speedup = if avg_times[best_idx] > 0.0 {
                    avg_times[slowest_correct_idx] / avg_times[best_idx]
                } else { 1.0 };

                println!();
                println!("  verdict:");
                println!("    winner: option {best_idx}");
                println!("    reason: fastest fully-correct option");
                println!("    correct-speedup: {:.1}x faster than option {slowest_correct_idx}", speedup);
                if best_idx != worst_idx {
                    println!("    rejected-fastest: option {worst_idx} was faster but not consistently correct");
                }
                if slowest_idx != slowest_correct_idx {
                    println!("    slowest-overall: option {slowest_idx}");
                }
                let confidence = node.weights.get(best_idx).copied().unwrap_or(0.0) * 100.0;
                println!("    confidence: {:.1}%", confidence);
            } else {
                println!();
                println!("  verdict: still exploring (not all options tried)");
            }
        }
        println!();
    }

    // learn-report is READ-ONLY — does not mutate the binary.
    // Use `lycan <file.lyc>` to run and evolve.
}

fn capsule_create(lyc_path: &str, name: &str, intent: &str) {
    let out_dir = format!("{}.lycap", name);
    match capsule::create(lyc_path, &out_dir, name, intent, vec!["stdout".to_string()]) {
        Ok(()) => eprintln!("capsule created: {out_dir}/"),
        Err(e) => { eprintln!("capsule error: {e}"); std::process::exit(1); }
    }
}

fn capsule_verify(dir: &str) {
    match capsule::verify_capsule(dir) {
        Ok(()) => println!("VERIFIED: {dir} is a valid Lycan capsule"),
        Err(e) => { eprintln!("INVALID: {e}"); std::process::exit(1); }
    }
}

fn capsule_inspect(dir: &str) {
    let inspect_path = format!("{dir}/inspect.json");
    match std::fs::read_to_string(&inspect_path) {
        Ok(s) => print!("{s}"),
        Err(_) => {
            // Regenerate from program.lyc
            let lyc_path = format!("{dir}/program.lyc");
            let data = match std::fs::read(&lyc_path) {
                Ok(d) => d,
                Err(e) => { eprintln!("cannot read {lyc_path}: {e}"); std::process::exit(1); }
            };
            let _ng = match graph::NeuralGraph::from_bytes(&data) {
                Ok(g) => g,
                Err(e) => { eprintln!("{e}"); std::process::exit(1); }
            };
            // Print manifest + inspect
            let manifest_path = format!("{dir}/manifest.json");
            if let Ok(m) = std::fs::read_to_string(&manifest_path) {
                println!("=== MANIFEST ===");
                print!("{m}");
                println!();
            }
            println!("=== GRAPH ===");
            inspect_json(&format!("{dir}/program.lyc"));
        }
    }
}

fn capsule_run(dir: &str) {
    // Verify first — invalid capsules fail closed
    if let Err(e) = capsule::verify_capsule(dir) {
        eprintln!("capsule verification failed: {e}");
        std::process::exit(1);
    }

    // Load policy for runtime enforcement
    let policy = match capsule::load_policy(dir) {
        Ok(p) => p,
        Err(e) => { eprintln!("cannot load policy: {e}"); std::process::exit(1); }
    };

    let lyc_path = format!("{dir}/program.lyc");
    let mut ctx = context::ExecutionContext::with_policy(policy);
    ctx.working_dir = Some(std::path::PathBuf::from(dir));
    run_binary_with_context(&lyc_path, ctx);
}

fn run_binary_with_context(path: &str, ctx: context::ExecutionContext) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {path}: {e}"); std::process::exit(1); }
    };

    if data.len() < 4 || data[0] != 0x4C || data[1] != 0x59 || data[2] != 0x43 || data[3] != 0x4E {
        eprintln!("not a .lyc graph binary");
        std::process::exit(1);
    }

    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };
    if let Err(e) = verifier::verify(&ng) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    let mut executor = graph_executor::GraphExecutor::new_with_context(ng, ctx);
    match executor.run() {
        Ok(_) => {}
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }

    let mut updated = executor.into_graph();
    let stats = optimizer::optimize(&mut updated);
    if stats.nodes_specialized > 0 || stats.nodes_cached > 0 {
        optimizer::print_stats(&stats);
    }
    let updated_bytes = updated.to_bytes();
    if let Err(e) = std::fs::write(path, &updated_bytes) {
        eprintln!("warning: could not save updated weights: {e}");
    }
}

/// Apply a proposed strategy improvement to a .lyc file.
fn capsule_apply_proposal(lyc_path: &str, proposal_path: &str) {
    // Read proposal JSON
    let json = match std::fs::read_to_string(proposal_path) {
        Ok(s) => s,
        Err(e) => { eprintln!("cannot read proposal: {e}"); std::process::exit(1); }
    };

    // Parse proposal
    let proposal = match evolve::parse_proposal(&json) {
        Ok(p) => p,
        Err(e) => { eprintln!("invalid proposal: {e}"); std::process::exit(1); }
    };

    // Save original bytes for rollback
    let original_bytes = std::fs::read(lyc_path).unwrap_or_default();
    let backup_path = format!("{lyc_path}.backup");

    // Apply
    match evolve::apply_proposal(lyc_path, &proposal, 5) {
        Ok(result) => {
            if result.accepted {
                // Save backup of pre-mutation binary
                std::fs::write(&backup_path, &original_bytes).ok();
                println!("ACCEPTED: {}", result.reason);
                println!("backup saved: {backup_path}");
            } else {
                // Restore original bytes (apply_proposal may have written)
                std::fs::write(lyc_path, &original_bytes).ok();
                println!("REJECTED: {}", result.reason);
                std::process::exit(1);
            }
        }
        Err(e) => {
            // Restore original bytes on error
            std::fs::write(lyc_path, &original_bytes).ok();
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

/// Emit an AI-readable improvement brief for a capsule or .lyc file.
fn capsule_improve(path: &str) {
    // Accept either a capsule dir or a .lyc file directly
    let lyc_path = if std::path::Path::new(&format!("{path}/program.lyc")).exists() {
        format!("{path}/program.lyc")
    } else {
        path.to_string()
    };

    let data = match std::fs::read(&lyc_path) {
        Ok(d) => d,
        Err(e) => { eprintln!("error reading {lyc_path}: {e}"); std::process::exit(1); }
    };
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    let brief = evolve::emit_brief(&ng);
    if brief.is_empty() || brief == "[]" {
        eprintln!("no strategy nodes found — nothing to improve");
        std::process::exit(1);
    }
    println!("{brief}");
}

/// Delayed feedback CLI: lycan feedback <file> <node_id> --option <n> --reward <f> [--success <b>]
fn cli_feedback(args: &[String]) {
    if args.len() < 5 {
        eprintln!("usage: lycan feedback <file.lyc> <node_id> --option <n> --reward <float>");
        std::process::exit(1);
    }
    let path = &args[0];
    let node_id: u32 = args[1].parse().unwrap_or_else(|_| {
        eprintln!("invalid node_id: {}", args[1]); std::process::exit(1);
    });

    // Parse --option and --reward flags
    let mut option_idx: Option<usize> = None;
    let mut reward: f64 = 0.0;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--option" => { i += 1; option_idx = Some(args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0)); }
            "--reward" => { i += 1; reward = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0); }
            "--success" => { i += 1; let s = args.get(i).map(|s| s.as_str()).unwrap_or("true"); reward = if s == "true" { 1.0 } else { -1.0 }; }
            _ => {}
        }
        i += 1;
    }
    let option_idx = option_idx.unwrap_or_else(|| {
        eprintln!("--option <n> is required"); std::process::exit(1);
    });

    // Load graph
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("cannot read {path}: {e}"); std::process::exit(1); }
    };
    let mut ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    // Validate node
    let node = match ng.nodes.get(node_id as usize) {
        Some(n) => n,
        None => { eprintln!("node #{node_id} does not exist"); std::process::exit(1); }
    };
    if !matches!(node.op, graph::OpCode::Strategy | graph::OpCode::AdaptiveChoice) {
        eprintln!("node #{node_id} is {:?}, not Strategy/AdaptiveChoice", node.op);
        std::process::exit(1);
    }
    let n_options = if node.contract == graph::Contract::WithinTolerance && node.weights.len() > 1 {
        node.weights.len() - 1
    } else {
        node.weights.len()
    };
    if option_idx >= n_options {
        eprintln!("option {option_idx} out of range (node has {n_options} options)");
        std::process::exit(1);
    }

    // Print before
    let before: Vec<String> = ng.nodes[node_id as usize].weights[..n_options].iter()
        .map(|w| format!("{w:.4}")).collect();
    println!("before: [{}]", before.join(", "));

    // Update weights
    let learning_rate = 0.05;
    let delta = reward * learning_rate;
    let n = n_options;
    for j in 0..n {
        if j == option_idx {
            ng.nodes[node_id as usize].weights[j] =
                (ng.nodes[node_id as usize].weights[j] + delta).clamp(0.01, 0.99);
        } else if n > 1 {
            ng.nodes[node_id as usize].weights[j] =
                (ng.nodes[node_id as usize].weights[j] - delta / (n - 1) as f64).clamp(0.01, 0.99);
        }
    }
    // Normalize
    let sum: f64 = ng.nodes[node_id as usize].weights[..n].iter().sum();
    if sum > 0.0 {
        for j in 0..n { ng.nodes[node_id as usize].weights[j] /= sum; }
    }

    // Update stats: increment tries and correct count
    if let Some(slot) = ng.nodes[node_id as usize].state_slot {
        let base = slot as usize + option_idx * 3;
        if base + 2 < ng.state.len() {
            ng.state[base] += 1.0; // tries
            if reward > 0.0 { ng.state[base + 2] += 1.0; } // correct
        }
    }

    // Journal
    ng.journal.push(graph::JournalEntry {
        run_number: ng.nodes.get(ng.entry as usize)
            .map(|n| n.activation_count).unwrap_or(0),
        node_id,
        mutation: graph::MutationKind::FeedbackReceived,
        reason: u32::MAX,
    });

    // Print after
    let after: Vec<String> = ng.nodes[node_id as usize].weights[..n_options].iter()
        .map(|w| format!("{w:.4}")).collect();
    println!("after:  [{}]", after.join(", "));
    println!("feedback: option={option_idx} reward={reward} node=#{node_id}");

    // Save
    let updated = ng.to_bytes();
    match std::fs::write(path, &updated) {
        Ok(_) => {}
        Err(e) => { eprintln!("cannot write {path}: {e}"); std::process::exit(1); }
    }
}

/// Autonomous evolution loop.
fn cli_evolve(args: &[String]) {
    let path = &args[0];

    // Parse flags — mutually exclusive modes
    let mut agent_command: Option<String> = None;
    let mut proposal_path: Option<String> = None;
    let mut policy_path: Option<String> = None;
    let mut no_agent = false;
    let mut iterations: usize = 1;
    let mut budget_ms: u64 = 60000;
    let mut min_improvement: f64 = 0.05;
    let mut dry_run = false;
    let mut json_output = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--agent-command" => { i += 1; agent_command = args.get(i).cloned(); }
            "--proposal" => { i += 1; proposal_path = args.get(i).cloned(); }
            "--policy" => { i += 1; policy_path = args.get(i).cloned(); }
            "--no-agent" => { no_agent = true; }
            "--iterations" => { i += 1; iterations = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(1); }
            "--budget-ms" => { i += 1; budget_ms = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(60000); }
            "--min-improvement" => { i += 1; min_improvement = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.05); }
            "--dry-run" => { dry_run = true; }
            "--json" => { json_output = true; }
            _ => {}
        }
        i += 1;
    }

    // Validate mutually exclusive modes
    let mode_count = [agent_command.is_some(), proposal_path.is_some(), no_agent]
        .iter().filter(|&&v| v).count();
    if mode_count == 0 {
        eprintln!("evolve requires exactly one of: --agent-command, --proposal, --no-agent");
        std::process::exit(1);
    }
    if mode_count > 1 {
        eprintln!("evolve modes are mutually exclusive: --agent-command, --proposal, --no-agent");
        std::process::exit(1);
    }

    // Load policy: explicit --policy, auto-detect from .lycap, or unrestricted
    let policy = if let Some(ref pp) = policy_path {
        match capsule::load_policy(pp) {
            Ok(p) => Some(p),
            Err(e) => { eprintln!("cannot load policy: {e}"); std::process::exit(1); }
        }
    } else if std::path::Path::new(path).is_dir() {
        // .lycap directory — auto-load policy.json, fail closed
        match capsule::load_policy(path) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("warning: capsule policy load failed: {e} — using deny-all");
                Some(context::ExecutionPolicy {
                    allow_stdout: false, allow_stdin: false,
                    allow_file_read: false, allow_file_write: false,
                    allow_network: false,
                    file_root: None, allowed_hosts: vec![], deny_private_networks: true,
                })
            }
        }
    } else {
        None // raw .lyc — unrestricted
    };

    let config = evolution_loop::EvolutionConfig {
        iterations,
        budget_ms,
        min_improvement,
        dry_run,
        agent_command: if no_agent { None } else { agent_command },
        proposal_path: if no_agent { None } else { proposal_path },
        json_output,
        policy,
    };

    match evolution_loop::run_evolution(path, &config) {
        Ok(result) => {
            if json_output {
                let outcomes_json: Vec<String> = result.outcomes.iter().map(|o| {
                    format!(
                        r#"    {{"accepted":{},"reason":"{}","proposal":"{}","target":{},"before_hash":"{}"}}"#,
                        o.accepted,
                        o.reason.replace('"', "\\\""),
                        o.proposal_name.replace('"', "\\\""),
                        o.target_strategy,
                        o.before_hash,
                    )
                }).collect();
                println!(r#"{{
  "iterations": {},
  "proposals_received": {},
  "proposals_accepted": {},
  "proposals_rejected": {},
  "outcomes": [
{}
  ]
}}"#,
                    result.iterations_run,
                    result.proposals_received,
                    result.proposals_accepted,
                    result.proposals_rejected,
                    outcomes_json.join(",\n"),
                );
            } else {
                eprintln!("evolution complete: {} iteration(s), {} accepted, {} rejected",
                    result.iterations_run, result.proposals_accepted, result.proposals_rejected);
                for o in &result.outcomes {
                    let tag = if o.accepted { "ACCEPTED" } else { "REJECTED" };
                    eprintln!("  [{tag}] {} — {}", o.proposal_name, o.reason);
                }
            }
        }
        Err(e) => {
            eprintln!("evolution error: {e}");
            std::process::exit(1);
        }
    }
}

/// Decision Runtime: run program, return structured JSON decision.
fn cli_decide(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("cannot read {path}: {e}"); std::process::exit(1); }
    };
    if data.len() < 4 || data[0] != 0x4C || data[1] != 0x59 {
        eprintln!("not a .lyc file"); std::process::exit(1);
    }
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };
    if let Err(e) = verifier::verify(&ng) {
        eprintln!("{e}"); std::process::exit(1);
    }

    // Run the program (captures the result)
    let mut executor = graph_executor::GraphExecutor::new(ng);
    let result = match executor.run() {
        Ok(v) => v,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    // Find strategy/decision nodes and report what was chosen
    let graph = executor.into_graph();
    let mut decisions = Vec::new();

    for node in &graph.nodes {
        if !matches!(node.op, graph::OpCode::Strategy | graph::OpCode::AdaptiveChoice) {
            continue;
        }
        if node.activation_count == 0 { continue; }

        let n_options = if node.contract == graph::Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };

        // Which option was chosen? (stored in bias)
        let chosen = node.bias as usize;
        let confidence = node.weights.get(chosen).copied().unwrap_or(0.0);

        let objective = match node.objective {
            graph::Objective::Speed => "speed", graph::Objective::Accuracy => "accuracy",
            graph::Objective::Reliability => "reliability", graph::Objective::Cost => "cost",
            graph::Objective::Risk => "risk", graph::Objective::Confidence => "confidence",
            graph::Objective::Reward => "reward", graph::Objective::MultiObjective => "multi",
            graph::Objective::None => "general",
        };

        let weights: Vec<String> = node.weights[..n_options].iter()
            .map(|w| format!("{w:.4}")).collect();

        decisions.push(format!(
            r#"  {{
    "node_id": {},
    "chosen_option": {},
    "confidence": {:.4},
    "objective": "{}",
    "weights": [{}],
    "activations": {},
    "result": "{}"
  }}"#,
            node.id, chosen, confidence, objective,
            weights.join(", "), node.activation_count,
            format!("{result}").replace('"', "\\\""),
        ));
    }

    // Save updated weights
    let updated_bytes = graph.to_bytes();
    std::fs::write(path, &updated_bytes).ok();

    // Output decision JSON
    if decisions.len() == 1 {
        println!("{}", decisions[0]);
    } else {
        println!("[{}]", decisions.join(",\n"));
    }
}

/// Convert serde_json::Value to CapValue for injection into ExecutionContext.
fn json_to_capvalue(v: serde_json::Value) -> capabilities::CapValue {
    match v {
        serde_json::Value::Null => capabilities::CapValue::Null,
        serde_json::Value::Bool(b) => capabilities::CapValue::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { capabilities::CapValue::Int(i) }
            else { capabilities::CapValue::Float(n.as_f64().unwrap_or(0.0)) }
        }
        serde_json::Value::String(s) => capabilities::CapValue::Str(s),
        serde_json::Value::Array(a) => capabilities::CapValue::Array(
            a.into_iter().map(json_to_capvalue).collect()
        ),
        serde_json::Value::Object(o) => capabilities::CapValue::Array(
            o.into_iter().map(|(k, v)| capabilities::CapValue::Array(vec![
                capabilities::CapValue::Str(k),
                json_to_capvalue(v),
            ])).collect()
        ),
    }
}

/// Decision Runtime with injected JSON input.
fn cli_decide_with_input(path: &str, input_path: &str) {
    // Read and parse input JSON
    let json_str = match std::fs::read_to_string(input_path) {
        Ok(s) => s,
        Err(e) => { eprintln!("cannot read {input_path}: {e}"); std::process::exit(1); }
    };
    let json_val: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => { eprintln!("invalid JSON in {input_path}: {e}"); std::process::exit(1); }
    };
    let input = json_to_capvalue(json_val);

    // Load and verify graph
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("cannot read {path}: {e}"); std::process::exit(1); }
    };
    if data.len() < 4 || data[0] != 0x4C || data[1] != 0x59 {
        eprintln!("not a .lyc file"); std::process::exit(1);
    }
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };
    if let Err(e) = verifier::verify(&ng) {
        eprintln!("{e}"); std::process::exit(1);
    }

    // Run with input context
    let ctx = context::ExecutionContext::with_input(input);
    let mut executor = graph_executor::GraphExecutor::new_with_context(ng, ctx);
    let result = match executor.run() {
        Ok(v) => v,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };

    // Report decisions (same logic as cli_decide)
    let graph = executor.into_graph();
    let mut decisions = Vec::new();

    for node in &graph.nodes {
        if !matches!(node.op, graph::OpCode::Strategy | graph::OpCode::AdaptiveChoice) {
            continue;
        }
        if node.activation_count == 0 { continue; }

        let n_options = if node.contract == graph::Contract::WithinTolerance && node.weights.len() > 1 {
            node.weights.len() - 1
        } else {
            node.weights.len()
        };

        let chosen = node.bias as usize;
        let confidence = node.weights.get(chosen).copied().unwrap_or(0.0);

        let objective = match node.objective {
            graph::Objective::Speed => "speed", graph::Objective::Accuracy => "accuracy",
            graph::Objective::Reliability => "reliability", graph::Objective::Cost => "cost",
            graph::Objective::Risk => "risk", graph::Objective::Confidence => "confidence",
            graph::Objective::Reward => "reward", graph::Objective::MultiObjective => "multi",
            graph::Objective::None => "general",
        };

        let weights: Vec<String> = node.weights[..n_options].iter()
            .map(|w| format!("{w:.4}")).collect();

        decisions.push(format!(
            r#"  {{
    "node_id": {},
    "chosen_option": {},
    "confidence": {:.4},
    "objective": "{}",
    "weights": [{}],
    "activations": {},
    "result": "{}"
  }}"#,
            node.id, chosen, confidence, objective,
            weights.join(", "), node.activation_count,
            format!("{result}").replace('"', "\\\""),
        ));
    }

    // Save updated weights
    let updated_bytes = graph.to_bytes();
    std::fs::write(path, &updated_bytes).ok();

    // Output decision JSON
    if decisions.len() == 1 {
        println!("{}", decisions[0]);
    } else {
        println!("[{}]", decisions.join(",\n"));
    }
}

/// Weakness/plateau detection report.
fn cli_improve_report(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => { eprintln!("cannot read {path}: {e}"); std::process::exit(1); }
    };
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    };
    let reports = evolve::improve_report(&ng);
    if reports.is_empty() {
        println!("[]");
        eprintln!("no weaknesses detected — all strategy nodes performing well");
    } else {
        println!("{}", evolve::reports_to_json(&reports));
    }
}

/// Read-only decision report — shows decision/strategy state from persisted data.
fn decision_report(path: &str) {
    // Same as learn-report for now — both read persisted stats
    learn_report(path);
}

fn repl() {
    eprintln!("Lycan v0.1.0 — AI-native computation schema");
    eprintln!("Type expressions to evaluate. Ctrl-D to exit.");
    eprintln!();

    let mut interp = interpreter::Interpreter::new();
    let mut input = String::new();

    loop {
        print!(">> ");
        io::stdout().flush().ok();
        input.clear();
        match io::stdin().read_line(&mut input) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => { eprintln!("read error: {e}"); break; }
        }
        let trimmed = input.trim();
        if trimmed.is_empty() { continue; }

        match parse_source(trimmed) {
            Ok(program) => {
                for node in &program.nodes {
                    match interp.eval_node(node) {
                        Ok(val) => {
                            if !matches!(val, value::Value::Null) {
                                println!("{val}");
                            }
                        }
                        Err(e) => eprintln!("{e}"),
                    }
                }
            }
            Err(e) => eprintln!("{e}"),
        }
    }
}

fn parse_source(src: &str) -> error::LycanResult<ast::Program> {
    let mut lex = lexer::Lexer::new(src);
    let tokens = lex.tokenize()?;
    let mut par = parser::Parser::new(tokens);
    par.parse_program()
}

fn execute_source(src: &str) -> error::LycanResult<value::Value> {
    let program = parse_source(src)?;
    let mut interp = interpreter::Interpreter::new();
    interp.run(&program)
}

fn node_to_source(node: &ast::Node) -> String {
    match node {
        ast::Node::Int(n) => format!("{n}"),
        ast::Node::Float(f) => format!("{f}"),
        ast::Node::Str(s) => format!("\"{s}\""),
        ast::Node::Bool(b) => if *b { "true".into() } else { "false".into() },
        ast::Node::Null => "null".into(),
        ast::Node::Ident(name) => name.clone(),
        ast::Node::Bind { name, mutable, ty, value } => {
            let tag = if *mutable { "$!" } else { "$" };
            let t = ty.as_ref().map(type_str).unwrap_or_default();
            format!("({tag} {name}{t} {})", node_to_source(value))
        }
        ast::Node::Assign { name, value } =>
            format!("(= {name} {})", node_to_source(value)),
        ast::Node::Fn { name, params, ret, body, stateful } => {
            let tag = if *stateful { "F!" } else { "F" };
            let ps: Vec<String> = params.iter().map(|p| {
                let t = p.ty.as_ref().map(type_str).unwrap_or_default();
                format!("{}{t}", p.name)
            }).collect();
            let r = ret.as_ref().map(type_str).unwrap_or_default();
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            match name {
                Some(n) => format!("({tag} {n} ({}){r} {})", ps.join(" "), b.join(" ")),
                None => format!("(\\ ({}){r} {})", ps.join(" "), b.join(" ")),
            }
        }
        ast::Node::Call { callee, args } => {
            let a: Vec<String> = args.iter().map(node_to_source).collect();
            if a.is_empty() { format!("({})", node_to_source(callee)) }
            else { format!("({} {})", node_to_source(callee), a.join(" ")) }
        }
        ast::Node::If { cond, then_branch, else_branch } => {
            let e = else_branch.as_ref().map(|x| format!(" {}", node_to_source(x))).unwrap_or_default();
            format!("(? {} {}{e})", node_to_source(cond), node_to_source(then_branch))
        }
        ast::Node::While { cond, body } => {
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            format!("(W {} {})", node_to_source(cond), b.join(" "))
        }
        ast::Node::ForEach { var, iterable, body } => {
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            format!("(each {var} {} {})", node_to_source(iterable), b.join(" "))
        }
        ast::Node::Repeat { count, body } => {
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            format!("(# {} {})", node_to_source(count), b.join(" "))
        }
        ast::Node::Return(val) => format!("(^ {})", node_to_source(val)),
        ast::Node::Block(exprs) => {
            let b: Vec<String> = exprs.iter().map(node_to_source).collect();
            format!("(B {})", b.join(" "))
        }
        ast::Node::Array(elems) => {
            let e: Vec<String> = elems.iter().map(node_to_source).collect();
            format!("(A {})", e.join(" "))
        }
        ast::Node::Index { object, index } =>
            format!("(I {} {})", node_to_source(object), node_to_source(index)),
        ast::Node::Range { start, end } =>
            format!("(.. {} {})", node_to_source(start), node_to_source(end)),
        ast::Node::Op { op, args } => {
            let s = match op {
                ast::OpKind::Add => "+", ast::OpKind::Sub => "-",
                ast::OpKind::Mul => "*", ast::OpKind::Div => "/",
                ast::OpKind::Mod => "%", ast::OpKind::Eq => "==",
                ast::OpKind::Neq => "!=", ast::OpKind::Lt => "<",
                ast::OpKind::Gt => ">", ast::OpKind::Lte => "<=",
                ast::OpKind::Gte => ">=", ast::OpKind::And => "&&",
                ast::OpKind::Or => "||", ast::OpKind::Not => "not",
                ast::OpKind::Neg => "neg",
            };
            let a: Vec<String> = args.iter().map(node_to_source).collect();
            format!("({s} {})", a.join(" "))
        }
        ast::Node::Pipe { kind, data, func, init } => {
            let k = match kind {
                ast::PipeKind::Pipe => "|>", ast::PipeKind::Filter => "|?",
                ast::PipeKind::Map => "|*", ast::PipeKind::Reduce => "|+",
            };
            let i = init.as_ref().map(|x| format!(" {}", node_to_source(x))).unwrap_or_default();
            format!("({k} {} {}{i})", node_to_source(data), node_to_source(func))
        }
        ast::Node::Adapt { target, body } => {
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            format!("(~> {target} {})", b.join(" "))
        }
        ast::Node::Choice { options } => {
            let o: Vec<String> = options.iter().map(node_to_source).collect();
            format!("(choice {})", o.join(" "))
        }
        ast::Node::Guard { assumption, fast_path, fallback } => {
            format!("(guard {} {} {})", node_to_source(assumption), node_to_source(fast_path), node_to_source(fallback))
        }
        ast::Node::Strategy { options } => {
            let o: Vec<String> = options.iter().map(node_to_source).collect();
            format!("(strategy {})", o.join(" "))
        }
        ast::Node::Feedback { target, reward } => {
            format!("(feedback {} {})", node_to_source(target), node_to_source(reward))
        }
        ast::Node::Builtin { name, args } => {
            let a: Vec<String> = args.iter().map(node_to_source).collect();
            if a.is_empty() { format!("(!{name})") }
            else { format!("(!{name} {})", a.join(" ")) }
        }
    }
}

fn type_str(ty: &ast::Type) -> String {
    match ty {
        ast::Type::Int => " :i".into(),
        ast::Type::Float => " :f".into(),
        ast::Type::Str => " :s".into(),
        ast::Type::Bool => " :b".into(),
        ast::Type::Null => " :n".into(),
        ast::Type::Array(inner) => format!(" :[{}]", type_str(inner).trim()),
    }
}
