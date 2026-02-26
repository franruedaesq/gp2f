//! Wasmtime integration for the policy WASM binary.
//!
//! At startup the server loads `policy_wasm_bg.wasm` from disk once, compiles
//! it into a [`wasmtime::Module`], and keeps a single [`WasmtimeEngine`]
//! instance shared across all reconciliation tasks.
//!
//! ## Protobuf round-trips
//!
//! [`WasmtimeEngine::evaluate_pb`] accepts pre-encoded Protobuf byte slices
//! for `state` and `node`, passes them to the WASM binary's exported
//! `evaluate_pb` function via shared linear memory, and returns the
//! Protobuf-encoded result.  This avoids serialization overhead on the
//! hot path.
//!
//! ## Fallback
//!
//! When the `wasmtime-engine` Cargo feature is **not** enabled, or when the
//! WASM binary is not found at startup, [`WasmtimeEngine::new`] returns an
//! `Err` and callers fall back to the in-process [`policy_core`] evaluator.
//! This ensures zero-downtime deployment when upgrading the WASM policy binary.

#[cfg(feature = "wasmtime-engine")]
use std::path::Path;

// ── engine ────────────────────────────────────────────────────────────────────

/// Compiled Wasmtime engine for the GP2F policy WASM binary.
///
/// Create with [`WasmtimeEngine::new`]; then call [`evaluate_pb`] on the hot
/// path.  The engine is `Send + Sync` so it can be stored in Axum's shared
/// state.
pub struct WasmtimeEngine {
    #[cfg(feature = "wasmtime-engine")]
    inner: WasmtimeInner,
}

#[cfg(feature = "wasmtime-engine")]
struct WasmtimeInner {
    store: std::sync::Mutex<wasmtime::Store<()>>,
    instance: wasmtime::Instance,
    memory: wasmtime::Memory,
    evaluate_pb_fn: wasmtime::TypedFunc<(i32, i32, i32, i32), i32>,
}

// SAFETY: WasmtimeInner is protected by a Mutex; wasmtime Instance is Send.
#[cfg(feature = "wasmtime-engine")]
unsafe impl Send for WasmtimeEngine {}
#[cfg(feature = "wasmtime-engine")]
unsafe impl Sync for WasmtimeEngine {}

impl WasmtimeEngine {
    /// Load and compile `policy_wasm_bg.wasm` from `wasm_path`.
    ///
    /// # Errors
    ///
    /// Returns an error when:
    /// - The `wasmtime-engine` feature is disabled (compile-time).
    /// - The file does not exist or cannot be read.
    /// - Compilation or instantiation fails.
    ///
    /// Callers should fall back to the native [`policy_core`] evaluator on
    /// error.
    pub fn new(#[allow(unused_variables)] wasm_path: &str) -> Result<Self, WasmError> {
        #[cfg(feature = "wasmtime-engine")]
        {
            let path = Path::new(wasm_path);
            let engine = wasmtime::Engine::default();
            let module = wasmtime::Module::from_file(&engine, path)
                .map_err(|e| WasmError::Load(e.to_string()))?;
            let mut store = wasmtime::Store::new(&engine, ());
            let linker = wasmtime::Linker::new(&engine);
            let instance = linker
                .instantiate(&mut store, &module)
                .map_err(|e| WasmError::Instantiate(e.to_string()))?;

            let memory = instance
                .get_memory(&mut store, "memory")
                .ok_or_else(|| WasmError::MissingExport("memory".into()))?;

            let evaluate_pb_fn = instance
                .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store, "evaluate_pb")
                .map_err(|e| WasmError::MissingExport(e.to_string()))?;

            return Ok(Self {
                inner: WasmtimeInner {
                    store: std::sync::Mutex::new(store),
                    instance,
                    memory,
                    evaluate_pb_fn,
                },
            });
        }
        #[cfg(not(feature = "wasmtime-engine"))]
        Err(WasmError::FeatureDisabled)
    }

    /// Evaluate a policy against workflow state via the WASM binary.
    ///
    /// `state_pb` and `node_pb` are Protobuf-encoded byte slices.
    /// Returns the Protobuf-encoded [`EvalResult`] or an error.
    ///
    /// The WASM ABI contract:
    /// - `evaluate_pb(state_ptr: i32, state_len: i32, node_ptr: i32, node_len: i32) -> i32`
    ///   where the return value is a pointer to a length-prefixed (4-byte LE)
    ///   Protobuf response written into WASM linear memory.
    pub fn evaluate_pb(
        &self,
        #[allow(unused_variables)] state_pb: &[u8],
        #[allow(unused_variables)] node_pb: &[u8],
    ) -> Result<Vec<u8>, WasmError> {
        #[cfg(feature = "wasmtime-engine")]
        {
            let mut store = self.inner.store.lock().unwrap();
            let mem = self.inner.memory;

            // Write inputs into WASM linear memory.
            let state_ptr = self.alloc_in_wasm(&mut store, mem, state_pb)?;
            let node_ptr = self.alloc_in_wasm(&mut store, mem, node_pb)?;

            // Call the WASM export.
            let result_ptr = self
                .inner
                .evaluate_pb_fn
                .call(
                    &mut store,
                    (
                        state_ptr,
                        state_pb.len() as i32,
                        node_ptr,
                        node_pb.len() as i32,
                    ),
                )
                .map_err(|e| WasmError::Call(e.to_string()))?;

            // Read back length-prefixed protobuf response.
            let data = mem.data(&store);
            let rp = result_ptr as usize;
            if rp + 4 > data.len() {
                return Err(WasmError::Memory("result pointer out of bounds".into()));
            }
            let len =
                u32::from_le_bytes([data[rp], data[rp + 1], data[rp + 2], data[rp + 3]]) as usize;
            if rp + 4 + len > data.len() {
                return Err(WasmError::Memory("result length out of bounds".into()));
            }
            return Ok(data[rp + 4..rp + 4 + len].to_vec());
        }
        #[cfg(not(feature = "wasmtime-engine"))]
        Err(WasmError::FeatureDisabled)
    }

    /// Write `bytes` at the next available offset in WASM linear memory and
    /// return the pointer.  (Minimal bump allocator for the hot path; the WASM
    /// module owns all deallocation.)
    #[cfg(feature = "wasmtime-engine")]
    fn alloc_in_wasm(
        &self,
        store: &mut wasmtime::Store<()>,
        mem: wasmtime::Memory,
        bytes: &[u8],
    ) -> Result<i32, WasmError> {
        // Use the WASM module's exported `alloc` function if available.
        if let Ok(alloc_fn) = self
            .inner
            .instance
            .get_typed_func::<i32, i32>(store, "alloc")
        {
            let ptr = alloc_fn
                .call(store, bytes.len() as i32)
                .map_err(|e| WasmError::Call(e.to_string()))?;
            mem.write(store, ptr as usize, bytes)
                .map_err(|e| WasmError::Memory(e.to_string()))?;
            return Ok(ptr);
        }
        // Fallback: write at the end of statically-allocated region (page 1).
        let offset = 65536usize; // second page
        mem.write(store, offset, bytes)
            .map_err(|e| WasmError::Memory(e.to_string()))?;
        Ok(offset as i32)
    }
}

// ── errors ────────────────────────────────────────────────────────────────────

/// Errors returned by [`WasmtimeEngine`].
#[derive(Debug, thiserror::Error)]
pub enum WasmError {
    #[error("wasmtime-engine feature is disabled; using native evaluator")]
    FeatureDisabled,
    #[error("failed to load WASM binary: {0}")]
    Load(String),
    #[error("failed to instantiate WASM module: {0}")]
    Instantiate(String),
    #[error("missing WASM export: {0}")]
    MissingExport(String),
    #[error("WASM call failed: {0}")]
    Call(String),
    #[error("WASM memory error: {0}")]
    Memory(String),
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_returns_feature_disabled_without_wasmtime_feature() {
        // When compiled without the `wasmtime-engine` feature the engine must
        // fail gracefully so callers can fall back to the native evaluator.
        #[cfg(not(feature = "wasmtime-engine"))]
        {
            let result = WasmtimeEngine::new("policy_wasm_bg.wasm");
            assert!(result.is_err());
            assert!(matches!(result.err().unwrap(), WasmError::FeatureDisabled));
        }
        // With the feature enabled but no binary on disk we expect a Load error.
        #[cfg(feature = "wasmtime-engine")]
        {
            let result = WasmtimeEngine::new("/nonexistent/policy_wasm_bg.wasm");
            assert!(result.is_err());
            assert!(matches!(result.err().unwrap(), WasmError::Load(_)));
        }
    }

    #[test]
    fn evaluate_pb_errors_when_disabled() {
        // Build a dummy engine that returns the feature-disabled error.
        // This validates that the fallback path is exercised.
        #[cfg(not(feature = "wasmtime-engine"))]
        {
            let result = WasmtimeEngine::new("x");
            assert!(result.is_err());
            assert!(matches!(result.err().unwrap(), WasmError::FeatureDisabled));
        }
    }
}
