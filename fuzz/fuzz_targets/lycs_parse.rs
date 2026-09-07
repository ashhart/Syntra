//! Fuzz the `.lycs` source pipeline: lexer -> parser -> graph compiler.
//! Capsule source is AI/caller-generated; every stage must reject bad
//! input with an error instead of panicking (deep nesting included —
//! libFuzzer treats stack overflow as a crash).

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(tokens) = syntra::lexer::Lexer::new(src).tokenize() else {
        return;
    };
    let Ok(program) = syntra::parser::Parser::new(tokens).parse_program() else {
        return;
    };
    let _ = syntra::graph_compiler::GraphCompiler::new().compile(&program);
});
