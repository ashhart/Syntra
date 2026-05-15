/// Lycan — AI-native machine execution language.
///
/// This crate provides the language core: parser, compiler, graph format,
/// executor, capability registry, verifier, evolution engine, and shared runtime modules.

pub mod agent;
pub mod ast;
pub mod binary;
pub mod capabilities;
pub mod capsule;
pub mod context;
pub mod environment;
pub mod error;
pub mod evolution_loop;
pub mod evolve;
pub mod graph;
pub mod graph_compiler;
pub mod graph_executor;
pub mod interpreter;
pub mod lambert;
pub mod learning;
pub mod lexer;
pub mod optimizer;
pub mod parser;
pub mod server;
pub mod store;
pub mod token;
pub mod value;
pub mod verifier;
