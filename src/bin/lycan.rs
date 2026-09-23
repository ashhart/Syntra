use syntra::*;

fn main() {
    // Large stack for deep graph recursion (fib(20) = ~21K recursive calls)
    let builder = std::thread::Builder::new().stack_size(64 * 1024 * 1024);
    let handler = builder
        .spawn(|| {
            main_inner();
        })
        .unwrap();
    handler.join().unwrap();
}

fn main_inner() {
    let args: Vec<String> = std::env::args().collect();

    // Variable-arity commands — check before length-based dispatch
    if args.len() >= 2 && args[1] == "serve" {
        serve_from_args(&args[2..], "Lycan");
        return;
    }

    match args.len() {
        2 => match args[1].as_str() {
            "--help" | "-h" => print_usage(),
            "capabilities" => list_capabilities(),
            _ => run_file(&args[1]),
        },
        3 => match args[1].as_str() {
            "compile" => compile_to_neural(&args[2]),
            "explain" => explain_file(&args[2]),
            "inspect" => inspect_json(&args[2]),
            "dump" => dump_graph(&args[2]),
            "stats" => show_stats(&args[2]),
            _ => {
                eprintln!("unknown command '{}'", args[1]);
                print_usage();
                std::process::exit(2);
            }
        },
        4 if args[2] == "--input" => run_file_with_input(&args[1], &args[3]),
        4 => match args[1].as_str() {
            "capsule" => match args[2].as_str() {
                "verify" => capsule_verify(&args[3]),
                "inspect" => capsule_inspect(&args[3]),
                "run" => capsule_run(&args[3]),
                _ => print_usage(),
            },
            _ => print_usage(),
        },
        5 if args[1] == "capsule" && args[2] == "create" => {
            capsule_create(&args[3], &args[4], "no intent specified");
        }
        6 if args[1] == "capsule" && args[2] == "create" => {
            capsule_create(&args[3], &args[4], &args[5]);
        }
        _ => print_usage(),
    }
}

fn print_usage() {
    eprintln!("Lycan — compiler and graph runtime for Syntra capsule programs");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  lycan <file.lycs>         Compile, verify and run source");
    eprintln!("  lycan <file.lyc>          Verify and run a graph binary");
    eprintln!("  lycan <file> --input <request.json>  Run with injected runtime input");
    eprintln!("  lycan compile <file.lycs> Compile to .lyc graph binary");
    eprintln!("  lycan explain <file.lyc>  Translate binary to text");
    eprintln!("  lycan inspect <file.lyc>  JSON graph view");
    eprintln!("  lycan capabilities        List native capability registry");
    eprintln!("  lycan dump <file.lyc>     Dump graph binary hex");
    eprintln!("  lycan stats <file.lyc>    Show graph statistics");
    eprintln!("  lycan serve [--addr 127.0.0.1:8787] [--store ./lycan-store] [--admin-key <key>]");
    eprintln!("  lycan capsule create <file.lyc> <name> <intent>");
    eprintln!("  lycan capsule verify <dir>");
    eprintln!("  lycan capsule inspect <dir>");
    eprintln!("  lycan capsule run <dir>");
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
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };
    match execute_source(&src) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

fn run_binary(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };

    // Compiled graph format (v2)
    if data.len() >= 4 && data[0] == 0x4C && data[1] == 0x59 && data[2] == 0x43 && data[3] == 0x4E {
        let ng = match graph::NeuralGraph::from_bytes(&data) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        };
        // Verify before execution — invalid graphs must fail closed
        if let Err(e) = verifier::verify(&ng) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        let mut executor = graph_executor::GraphExecutor::new(ng);
        match executor.run() {
            Ok(_) => {}
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }

    // The legacy AST binary format was only executable by the tree-walking
    // interpreter, which now lives in Lycan Lab.
    eprintln!(
        "{path}: not a compiled graph binary (LYCN); recompile the source with `lycan compile`"
    );
    std::process::exit(1);
}

/// Run a program with a JSON document injected as `runtime.input`, the way
/// the server runs a capsule's feature program for a request. No policy is
/// applied (developer mode), matching `lycan <file>`.
fn run_file_with_input(path: &str, input_path: &str) {
    let fail = |msg: String| -> ! {
        eprintln!("{msg}");
        std::process::exit(1);
    };
    let input_text = std::fs::read_to_string(input_path)
        .unwrap_or_else(|e| fail(format!("error reading {input_path}: {e}")));
    let input_json: serde_json::Value = serde_json::from_str(&input_text)
        .unwrap_or_else(|e| fail(format!("{input_path}: invalid JSON: {e}")));
    let graph = if path.ends_with(".lycs") {
        let src = std::fs::read_to_string(path)
            .unwrap_or_else(|e| fail(format!("error reading {path}: {e}")));
        let program = parse_source(&src).unwrap_or_else(|e| fail(e.to_string()));
        graph_compiler::GraphCompiler::new()
            .compile(&program)
            .unwrap_or_else(|e| fail(format!("compile error: {e}")))
    } else {
        let data =
            std::fs::read(path).unwrap_or_else(|e| fail(format!("error reading {path}: {e}")));
        graph::NeuralGraph::from_bytes(&data).unwrap_or_else(|e| fail(e.to_string()))
    };
    if let Err(e) = verifier::verify(&graph) {
        fail(e.to_string());
    }
    let ctx = context::ExecutionContext::with_input(capabilities::CapValue::from_json(&input_json));
    let mut executor = graph_executor::GraphExecutor::new_with_context(graph, ctx);
    if let Err(e) = executor.run() {
        fail(e.to_string());
    }
}

fn compile_to_neural(path: &str) {
    let out = path.replace(".lycs", ".lyc");
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };
    let program = match parse_source(&src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let compiler = graph_compiler::GraphCompiler::new();
    let neural = match compiler.compile(&program) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("compile error: {e}");
            std::process::exit(1);
        }
    };
    let data = neural.to_bytes();
    match std::fs::write(&out, &data) {
        Ok(_) => eprintln!(
            "compiled {} -> {} ({} bytes, {} nodes, {} edges)",
            path,
            out,
            data.len(),
            neural.nodes.len(),
            neural.edges.len()
        ),
        Err(e) => {
            eprintln!("error writing {out}: {e}");
            std::process::exit(1);
        }
    }
}

/// Emit AI-readable JSON introspection of a `.lyc` graph.
fn inspect_json(path: &str) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    println!("{{");
    println!("  \"format\": \"lycan-graph-v{}\",", ng.header.version);
    println!("  \"entry\": {},", ng.entry);
    println!("  \"total_nodes\": {},", ng.nodes.len());
    println!(
        "  \"live_nodes\": {},",
        ng.nodes
            .iter()
            .filter(|n| n.op != graph::OpCode::Noop)
            .count()
    );
    println!("  \"edges\": {},", ng.edges.len());
    println!("  \"strings\": {},", ng.string_table.len());
    println!("  \"journal_entries\": {},", ng.journal.len());

    let used_capabilities = capabilities_used_by_graph(&ng);
    println!("  \"capabilities_used\": [");
    for (i, cap) in used_capabilities.iter().enumerate() {
        if let Some(spec) = capabilities::get(cap) {
            print!("{}", capabilities::spec_json(spec, 4));
        } else {
            print!(
                "    {{\"name\": \"{}\", \"known\": false}}",
                cap.replace('\"', "\\\"")
            );
        }
        if i < used_capabilities.len() - 1 {
            print!(",");
        }
        println!();
    }
    println!("  ],");

    // Nodes — only live ones
    println!("  \"nodes\": [");
    let live: Vec<&graph::GraphNode> = ng
        .nodes
        .iter()
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
        let annotation = node
            .annotation
            .map(|idx| ng.get_string(idx))
            .unwrap_or_default();

        let operand_strs: Vec<String> = node
            .operands
            .iter()
            .map(|op| match op {
                graph::Operand::NodeRef(id) => format!("{{\"ref\": {id}}}"),
                graph::Operand::Immediate(graph::ImmValue::Int(n)) => format!("{{\"int\": {n}}}"),
                graph::Operand::Immediate(graph::ImmValue::Float(f)) => {
                    format!("{{\"float\": {f}}}")
                }
                graph::Operand::Immediate(graph::ImmValue::Bool(b)) => format!("{{\"bool\": {b}}}"),
                graph::Operand::Immediate(graph::ImmValue::Null) => "\"null\"".to_string(),
                graph::Operand::StateRef(idx) => format!("{{\"state\": {idx}}}"),
                graph::Operand::StringRef(idx) => {
                    let s = ng.get_string(*idx).replace('\"', "\\\"");
                    format!("{{\"str\": \"{s}\"}}")
                }
                graph::Operand::VarSlot(slot) => format!("{{\"var\": {slot}}}"),
            })
            .collect();

        print!(
            "    {{\"id\": {}, \"op\": \"{}\", \"fired\": {}",
            node.id, op_name, node.activation_count
        );
        if !node.weights.is_empty() {
            print!(
                ", \"weights\": [{}], \"weight_kind\": \"{}\"",
                weights_str.join(", "),
                wk
            );
        }
        if node.bias != 0.0 {
            print!(
                ", \"type_hint\": {}",
                if node.bias == 1.0 {
                    "\"int\""
                } else if node.bias == 2.0 {
                    "\"float\""
                } else {
                    "\"unknown\""
                }
            );
        }
        if !annotation.is_empty() {
            print!(", \"meaning\": \"{}\"", annotation.replace('\"', "\\\""));
        }
        if !operand_strs.is_empty() {
            print!(", \"operands\": [{}]", operand_strs.join(", "));
        }
        print!("}}");
        if i < live.len() - 1 {
            print!(",");
        }
        println!();
    }
    println!("  ],");

    // Edges
    println!("  \"edges\": [");
    for (i, edge) in ng.edges.iter().enumerate() {
        print!(
            "    {{\"from\": {}, \"to\": {}, \"weight\": {:.4}",
            edge.from, edge.to, edge.weight
        );
        if let Some(g) = edge.gate {
            print!(", \"gate\": {g}");
        }
        print!("}}");
        if i < ng.edges.len() - 1 {
            print!(",");
        }
        println!();
    }
    println!("  ],");

    // Journal
    println!("  \"journal\": [");
    for (i, entry) in ng.journal.iter().enumerate() {
        let mutation = format!("{:?}", entry.mutation);
        print!(
            "    {{\"run\": {}, \"node\": {}, \"mutation\": \"{}\"",
            entry.run_number, entry.node_id, mutation
        );
        if entry.reason != u32::MAX {
            let reason = ng.get_string(entry.reason);
            if !reason.is_empty() {
                print!(", \"reason\": \"{}\"", reason.replace('\"', "\\\""));
            }
        }
        print!("}}");
        if i < ng.journal.len() - 1 {
            print!(",");
        }
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

fn capability_name_from_operand(
    ng: &graph::NeuralGraph,
    operand: &graph::Operand,
) -> Option<String> {
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
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };
    // Try compiled graph format first
    if data.len() >= 4 && data[0] == 0x4C && data[1] == 0x59 && data[2] == 0x43 && data[3] == 0x4E {
        let ng = match graph::NeuralGraph::from_bytes(&data) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
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
            let w_str = if weights.is_empty() {
                String::new()
            } else {
                format!(" w[{}]", weights.join(","))
            };
            println!(
                "  #{:04} {:12} operands:{} fired:{}{}",
                node.id,
                op_name,
                node.operands.len(),
                node.activation_count,
                w_str
            );
        }
    } else {
        // Old format
        let program = match binary::decode(&data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
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
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };
    // Print hex dump like machine code
    for (i, chunk) in data.chunks(16).enumerate() {
        print!("{:08x}  ", i * 16);
        for (j, byte) in chunk.iter().enumerate() {
            print!("{:02x} ", byte);
            if j == 7 {
                print!(" ");
            }
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
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };
    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let total_nodes = ng.nodes.len();
    let live_nodes = ng
        .nodes
        .iter()
        .filter(|n| n.op != graph::OpCode::Noop)
        .count();
    let dead_nodes = total_nodes - live_nodes;
    let total_activations: u64 = ng.nodes.iter().map(|n| n.activation_count).sum();
    let max_activations = ng
        .nodes
        .iter()
        .map(|n| n.activation_count)
        .max()
        .unwrap_or(0);
    let specialized = ng.nodes.iter().filter(|n| n.bias != 0.0).count();

    let branches: Vec<&graph::GraphNode> = ng
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.op,
                graph::OpCode::Branch | graph::OpCode::AdaptiveChoice | graph::OpCode::Strategy
            )
        })
        .collect();
    let converged = branches
        .iter()
        .filter(|n| n.weights.iter().any(|w| *w > 0.9 || *w < 0.1))
        .count();

    println!("=== {} ===", path);
    println!(
        "  Nodes:          {} total, {} live, {} pruned",
        total_nodes, live_nodes, dead_nodes
    );
    println!("  Edges:          {}", ng.edges.len());
    println!("  Strings:        {}", ng.string_table.len());
    println!("  Total fired:    {}", total_activations);
    println!("  Max fired:      {} (hottest node)", max_activations);
    println!("  Specialized:    {} nodes", specialized);
    println!("  Branches:       {}", branches.len());
    println!(
        "  Converged:      {} ({:.0}% of branches learned a preference)",
        converged,
        if branches.is_empty() {
            0.0
        } else {
            converged as f64 / branches.len() as f64 * 100.0
        }
    );
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
            let status = if b.weights.iter().any(|w| *w > 0.95) {
                " <- CONVERGED"
            } else if b.weights.iter().any(|w| *w > 0.8) {
                " <- learning"
            } else {
                ""
            };
            println!(
                "    #{:04} {:8} fired:{:>6} w[{}]{}",
                b.id,
                kind,
                b.activation_count,
                ws.join(", "),
                status
            );
        }
    }

    // Show top 10 hottest nodes
    let mut hot: Vec<&graph::GraphNode> = ng
        .nodes
        .iter()
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

fn capsule_create(lyc_path: &str, name: &str, intent: &str) {
    let out_dir = format!("{}.lycap", name);
    match capsule::create(lyc_path, &out_dir, name, intent, vec!["stdout".to_string()]) {
        Ok(()) => eprintln!("capsule created: {out_dir}/"),
        Err(e) => {
            eprintln!("capsule error: {e}");
            std::process::exit(1);
        }
    }
}

fn capsule_verify(dir: &str) {
    match capsule::verify_capsule(dir) {
        Ok(()) => println!("VERIFIED: {dir} is a valid Lycan capsule"),
        Err(e) => {
            eprintln!("INVALID: {e}");
            std::process::exit(1);
        }
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
                Err(e) => {
                    eprintln!("cannot read {lyc_path}: {e}");
                    std::process::exit(1);
                }
            };
            let _ng = match graph::NeuralGraph::from_bytes(&data) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
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
        Err(e) => {
            eprintln!("cannot load policy: {e}");
            std::process::exit(1);
        }
    };

    let lyc_path = format!("{dir}/program.lyc");
    let mut ctx = context::ExecutionContext::with_policy(policy);
    ctx.working_dir = Some(std::path::PathBuf::from(dir));
    run_binary_with_context(&lyc_path, ctx);
}

fn run_binary_with_context(path: &str, ctx: context::ExecutionContext) {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            std::process::exit(1);
        }
    };

    if data.len() < 4 || data[0] != 0x4C || data[1] != 0x59 || data[2] != 0x43 || data[3] != 0x4E {
        eprintln!("not a .lyc graph binary");
        std::process::exit(1);
    }

    let ng = match graph::NeuralGraph::from_bytes(&data) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = verifier::verify(&ng) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    let mut executor = graph_executor::GraphExecutor::new_with_context(ng, ctx);
    match executor.run() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
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
    let rt = |msg: String| error::LycanError::Runtime { msg };
    let graph = graph_compiler::GraphCompiler::new()
        .compile(&program)
        .map_err(|e| rt(format!("compile error: {e}")))?;
    verifier::verify(&graph).map_err(|e| rt(e.to_string()))?;
    let mut executor = graph_executor::GraphExecutor::new(graph);
    executor.run()?;
    Ok(value::Value::Null)
}

fn node_to_source(node: &ast::Node) -> String {
    match node {
        ast::Node::Int(n) => format!("{n}"),
        ast::Node::Float(f) => format!("{f}"),
        ast::Node::Str(s) => format!("\"{s}\""),
        ast::Node::Bool(b) => {
            if *b {
                "true".into()
            } else {
                "false".into()
            }
        }
        ast::Node::Null => "null".into(),
        ast::Node::Ident(name) => name.clone(),
        ast::Node::Bind {
            name,
            mutable,
            ty,
            value,
        } => {
            let tag = if *mutable { "$!" } else { "$" };
            let t = ty.as_ref().map(type_str).unwrap_or_default();
            format!("({tag} {name}{t} {})", node_to_source(value))
        }
        ast::Node::Assign { name, value } => format!("(= {name} {})", node_to_source(value)),
        ast::Node::Fn {
            name,
            params,
            ret,
            body,
            stateful,
        } => {
            let tag = if *stateful { "F!" } else { "F" };
            let ps: Vec<String> = params
                .iter()
                .map(|p| {
                    let t = p.ty.as_ref().map(type_str).unwrap_or_default();
                    format!("{}{t}", p.name)
                })
                .collect();
            let r = ret.as_ref().map(type_str).unwrap_or_default();
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            match name {
                Some(n) => format!("({tag} {n} ({}){r} {})", ps.join(" "), b.join(" ")),
                None => format!("(\\ ({}){r} {})", ps.join(" "), b.join(" ")),
            }
        }
        ast::Node::Call { callee, args } => {
            let a: Vec<String> = args.iter().map(node_to_source).collect();
            if a.is_empty() {
                format!("({})", node_to_source(callee))
            } else {
                format!("({} {})", node_to_source(callee), a.join(" "))
            }
        }
        ast::Node::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let e = else_branch
                .as_ref()
                .map(|x| format!(" {}", node_to_source(x)))
                .unwrap_or_default();
            format!(
                "(? {} {}{e})",
                node_to_source(cond),
                node_to_source(then_branch)
            )
        }
        ast::Node::While { cond, body } => {
            let b: Vec<String> = body.iter().map(node_to_source).collect();
            format!("(W {} {})", node_to_source(cond), b.join(" "))
        }
        ast::Node::ForEach {
            var,
            iterable,
            body,
        } => {
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
        ast::Node::Index { object, index } => {
            format!("(I {} {})", node_to_source(object), node_to_source(index))
        }
        ast::Node::Range { start, end } => {
            format!("(.. {} {})", node_to_source(start), node_to_source(end))
        }
        ast::Node::Op { op, args } => {
            let s = match op {
                ast::OpKind::Add => "+",
                ast::OpKind::Sub => "-",
                ast::OpKind::Mul => "*",
                ast::OpKind::Div => "/",
                ast::OpKind::Mod => "%",
                ast::OpKind::Eq => "==",
                ast::OpKind::Neq => "!=",
                ast::OpKind::Lt => "<",
                ast::OpKind::Gt => ">",
                ast::OpKind::Lte => "<=",
                ast::OpKind::Gte => ">=",
                ast::OpKind::And => "&&",
                ast::OpKind::Or => "||",
                ast::OpKind::Not => "not",
                ast::OpKind::Neg => "neg",
            };
            let a: Vec<String> = args.iter().map(node_to_source).collect();
            format!("({s} {})", a.join(" "))
        }
        ast::Node::Pipe {
            kind,
            data,
            func,
            init,
        } => {
            let k = match kind {
                ast::PipeKind::Pipe => "|>",
                ast::PipeKind::Filter => "|?",
                ast::PipeKind::Map => "|*",
                ast::PipeKind::Reduce => "|+",
            };
            let i = init
                .as_ref()
                .map(|x| format!(" {}", node_to_source(x)))
                .unwrap_or_default();
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
        ast::Node::Guard {
            assumption,
            fast_path,
            fallback,
        } => {
            format!(
                "(guard {} {} {})",
                node_to_source(assumption),
                node_to_source(fast_path),
                node_to_source(fallback)
            )
        }
        ast::Node::Strategy { options } => {
            let o: Vec<String> = options.iter().map(node_to_source).collect();
            format!("(strategy {})", o.join(" "))
        }
        ast::Node::Feedback { target, reward } => {
            format!(
                "(feedback {} {})",
                node_to_source(target),
                node_to_source(reward)
            )
        }
        ast::Node::Builtin { name, args } => {
            let a: Vec<String> = args.iter().map(node_to_source).collect();
            if a.is_empty() {
                format!("(!{name})")
            } else {
                format!("(!{name} {})", a.join(" "))
            }
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
