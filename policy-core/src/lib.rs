// Opt out of the Rust standard library when the "std" feature is not enabled.
// This allows policy-core to be compiled for bare-metal / WASM targets.
// When "std" IS enabled (the default), the attribute has no effect.
#![cfg_attr(not(feature = "std"), no_std)]

// In no_std mode, pull in the `alloc` crate for heap-allocated types
// (String, Vec, format!, etc.).
#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod ast;
// Schema / strategy types — available in std AND no_std (no yrs dependency).
pub mod crdt_schema;
// Full CRDT support (CrdtDoc backed by yrs) requires std.
#[cfg(feature = "std")]
pub mod crdt;
pub mod evaluator;
pub mod timestamp;
pub mod version;

pub use ast::{AstNode, NodeKind};
// Re-export schema types from the always-available crdt_schema module so
// existing callers that use `policy_core::crdt_schema::*` work out of the box.
#[cfg(feature = "std")]
pub use crdt::CrdtDoc;
pub use crdt_schema::{DocumentSchema, FieldSchema, FieldStrategy};
pub use evaluator::{EvalResult, Evaluator};
pub use timestamp::normalize_timestamp;
pub use version::VersionPolicy;
