//! GP2F native Node.js bindings.
//!
//! This crate wraps `policy-core` and provides an idiomatic TypeScript/Node.js
//! API using `napi-rs`.  Heavy lifting stays in Rust; JavaScript receives
//! ergonomic classes and objects.
//!
//! ## Exported API
//! - [`NodeKind`] – string enum of all AST node kinds.
//! - [`JsAstNode`] – plain JS object representing a policy AST node.
//! - [`JsActivityConfig`] – configuration object for an activity.
//! - [`JsWorkflow`] – `Workflow` class.
//! - [`JsServerConfig`] – server configuration object.
//! - [`JsGP2FServer`] – `GP2FServer` class.

#![deny(clippy::all)]

pub mod policy;
pub mod server;
pub mod workflow;
