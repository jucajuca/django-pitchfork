//! pitchfork-core: pure Rust core for django-pitchfork.
//!
//! Layer 1 of the workspace: schema, query AST and the versioned wire
//! protocol. Zero I/O, zero PyO3, no network — this crate compiles and
//! tests standalone.

pub mod query_ast;
pub mod schema;
pub mod wire;

pub use query_ast::{FilterOp, QueryNode};
pub use schema::{FieldDef, FieldType, PitchforkSchema};
pub use wire::WIRE_VERSION;
