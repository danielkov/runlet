//! Runlet's Phase 0 semantic executable model.
//!
//! The crate deliberately keeps execution deterministic and single-threaded. It
//! implements the complete language surface while leaving durable journals and
//! real asynchronous executors to the later phases described in `DESIGN.md`.

mod analyzer;
mod diagnostic;
mod graph;
mod lexer;
mod parser;
mod runtime;
mod schema;
mod syntax;
mod value;

pub use analyzer::{compile, CompiledProgram, ExternalInput};
pub use diagnostic::{Diagnostic, Fix, Phase, Severity, Span};
pub use graph::{Edge, EdgeKind, Graph, Node, NodeKind, NodeState};
pub use parser::parse;
pub use runtime::{Execution, Runtime, RuntimeBuilder, ToolContext, ToolError};
pub use schema::{CallSchema, ExecutionPolicy, Property, Schema, ToolDescriptor, ToolRegistry};
pub use syntax::{BinaryOp, Block, Expr, ExprKind, Program, Stmt, UnaryOp};
pub use value::{CanonicalValue, ValueError};

/// Language semantic version implemented by this crate.
pub const LANGUAGE_VERSION: &str = "1";
/// Canonical value encoding version implemented by this crate.
pub const VALUE_ENCODING_VERSION: &str = "rcve-v1";
