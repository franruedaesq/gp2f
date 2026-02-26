// Opt out of the Rust standard library when the "std" feature is not enabled.
// This allows policy-core to be compiled for bare-metal / WASM targets.
// When "std" IS enabled (the default), the attribute has no effect.
#![cfg_attr(not(feature = "std"), no_std)]

// In no_std mode, pull in the `alloc` crate for heap-allocated types
// (String, Vec, format!, etc.).
#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod ast;
// CRDT support relies on `yrs`, which requires std.
#[cfg(feature = "std")]
pub mod crdt;
pub mod evaluator;
pub mod timestamp;
pub mod version;

pub use ast::{AstNode, NodeKind};
#[cfg(feature = "std")]
pub use crdt::{CrdtDoc, DocumentSchema, FieldSchema, FieldStrategy};
pub use evaluator::{EvalResult, Evaluator};
pub use timestamp::normalize_timestamp;
pub use version::VersionPolicy;
