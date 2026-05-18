/// Lycan — AI-native machine execution language.
///
/// This crate provides the language core: parser, compiler, graph format,
/// executor, capability registry, verifier, evolution engine, and shared runtime modules.

pub mod agent;
pub mod ast;
pub mod auth_tokens;
pub mod backup;
pub mod binary;
pub mod capabilities;
pub mod capsule;
pub mod change_detection;
pub mod conformal;
pub mod context;
pub mod environment;
pub mod error;
pub mod evolution_loop;
pub mod evolve;
pub mod feature_schema;
pub mod graph;
pub mod graph_compiler;
pub mod graph_executor;
pub mod hierarchical;
pub mod hierarchical_state;
pub mod interpreter;
pub mod lambert;
pub mod learning;
pub mod lexer;
pub mod linucb;
pub mod meta_bandit;
pub mod ood;
pub mod optimizer;
pub mod parser;
pub mod rate_limit;
pub mod reward_characterization;
pub mod server;
pub mod shared_state_strategy;
pub mod store;
pub mod token;
pub mod value;
pub mod verifier;
pub mod warmup;
