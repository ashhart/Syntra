//! Language-level regressions.
//!
//! The v1 server regressions that used to live here (warmup, `learn=true`
//! scopes, substring decision lookup) tested code that v2 replaced; their
//! v2 equivalents are in `tests/server_v2.rs` and `tests/auth_routes.rs`.

// BUG-5: `(feedback name reward)` targeting a name that is not bound to a
// choice/strategy node compiled to a bare LoadVar reference, and the runtime
// silently dropped the credit (fail-open). The compiler must refuse such
// programs (learning-semantics §4.2: Feedback targets AdaptiveChoice/Strategy
// nodes only).
#[test]
fn feedback_target_must_resolve_to_choice_node() {
    fn compile(src: &str) -> Result<syntra::graph::NeuralGraph, String> {
        let tokens = syntra::lexer::Lexer::new(src).tokenize().expect("tokenize");
        let program = syntra::parser::Parser::new(tokens)
            .parse_program()
            .expect("parse");
        syntra::graph_compiler::GraphCompiler::new().compile(&program)
    }

    // Never-bound name → compile error.
    let err = compile("(feedback zzz 1.0)").expect_err("unbound feedback target must not compile");
    assert!(
        err.contains("'zzz' is not bound"),
        "unexpected error: {err}"
    );

    // Bound to a non-choice value → compile error.
    let err = compile("($ x 5)\n(feedback x 1.0)\nx")
        .expect_err("non-choice feedback target must not compile");
    assert!(
        err.contains("'x' is bound to a non-choice value"),
        "unexpected error: {err}"
    );

    // A `$`-bound choice remains the working pattern.
    compile("($ c (choice 0 1 2))\n(feedback c 1.0)\nc").expect("bound choice target must compile");
}
