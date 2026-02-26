//! Operational limits and backpressure.
//!
//! Enforces configurable per-tenant and per-client limits:
//! - Maximum queued ops per tenant (default 500).
//! - Maximum concurrent WebSocket connections per tenant.
//!
//! When a limit is reached the [`LimitsGuard`] returns a
//! [`BackpressureSignal`] that callers translate into a REJECT or a
//! connection-close, gracefully slowing down clients while the server is
//! under load.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── constants ─────────────────────────────────────────────────────────────────

/// Default maximum number of queued (in-flight) ops per tenant.
pub const DEFAULT_MAX_QUEUED_OPS: u32 = 500;

/// Default maximum concurrent WebSocket connections per tenant.
pub const DEFAULT_MAX_WS_CONNECTIONS: u32 = 100;

// ── backpressure signal ───────────────────────────────────────────────────────

/// Signals that a per-tenant limit has been reached.
///
/// The server uses this to reject ops gracefully without crashing or
/// discarding data silently.
#[derive(Debug, Clone, Error, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackpressureSignal {
    #[error(
        "tenant '{tenant_id}' has reached the maximum queued ops limit ({limit}); apply backpressure"
    )]
    QueueFull { tenant_id: String, limit: u32 },

    #[error("tenant '{tenant_id}' has reached the maximum WebSocket connections limit ({limit})")]
    TooManyConnections { tenant_id: String, limit: u32 },
}

// ── per-tenant limits ─────────────────────────────────────────────────────────

/// Configurable limits for a single tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantLimits {
    /// Maximum number of ops that may be queued (in-flight) at once.
    pub max_queued_ops: u32,
    /// Maximum number of concurrent WebSocket connections.
    pub max_ws_connections: u32,
}

impl Default for TenantLimits {
    fn default() -> Self {
        Self {
            max_queued_ops: DEFAULT_MAX_QUEUED_OPS,
            max_ws_connections: DEFAULT_MAX_WS_CONNECTIONS,
        }
    }
}

// ── per-tenant runtime counters ───────────────────────────────────────────────

#[derive(Debug, Default)]
struct TenantCounters {
    queued_ops: u32,
    ws_connections: u32,
}

// ── limits guard ──────────────────────────────────────────────────────────────

/// Thread-safe guard that tracks per-tenant counters and enforces limits.
pub struct LimitsGuard {
    /// Per-tenant configuration; unknown tenants use [`TenantLimits::default`].
    config: Mutex<HashMap<String, TenantLimits>>,
    /// Live runtime counters.
    counters: Mutex<HashMap<String, TenantCounters>>,
}

impl LimitsGuard {
    /// Create a guard with no custom per-tenant overrides (all tenants use
    /// [`TenantLimits::default`]).
    pub fn new() -> Self {
        Self {
            config: Mutex::new(HashMap::new()),
            counters: Mutex::new(HashMap::new()),
        }
    }

    /// Override the limits for a specific tenant.
    pub fn set_limits(&self, tenant_id: &str, limits: TenantLimits) {
        self.config
            .lock()
            .unwrap()
            .insert(tenant_id.to_owned(), limits);
    }

    /// Returns the effective limits for a tenant (custom or default).
    pub fn limits_for(&self, tenant_id: &str) -> TenantLimits {
        self.config
            .lock()
            .unwrap()
            .get(tenant_id)
            .cloned()
            .unwrap_or_default()
    }

    // ── op queueing ───────────────────────────────────────────────────────

    /// Attempt to enqueue one op for `tenant_id`.
    ///
    /// Returns `Ok(())` on success; returns a [`BackpressureSignal`] if the
    /// tenant is at its queue limit.
    pub fn try_enqueue_op(&self, tenant_id: &str) -> Result<(), BackpressureSignal> {
        let limit = self.limits_for(tenant_id).max_queued_ops;
        let mut counters = self.counters.lock().unwrap();
        let entry = counters.entry(tenant_id.to_owned()).or_default();
        if entry.queued_ops >= limit {
            return Err(BackpressureSignal::QueueFull {
                tenant_id: tenant_id.to_owned(),
                limit,
            });
        }
        entry.queued_ops += 1;
        Ok(())
    }

    /// Mark an op as dequeued (completed or rejected).
    pub fn dequeue_op(&self, tenant_id: &str) {
        let mut counters = self.counters.lock().unwrap();
        if let Some(entry) = counters.get_mut(tenant_id) {
            entry.queued_ops = entry.queued_ops.saturating_sub(1);
        }
    }

    // ── WebSocket connections ─────────────────────────────────────────────

    /// Attempt to register a new WebSocket connection for `tenant_id`.
    ///
    /// Returns `Ok(())` on success; returns a [`BackpressureSignal`] if the
    /// tenant is at its connection limit.
    pub fn try_register_connection(&self, tenant_id: &str) -> Result<(), BackpressureSignal> {
        let limit = self.limits_for(tenant_id).max_ws_connections;
        let mut counters = self.counters.lock().unwrap();
        let entry = counters.entry(tenant_id.to_owned()).or_default();
        if entry.ws_connections >= limit {
            return Err(BackpressureSignal::TooManyConnections {
                tenant_id: tenant_id.to_owned(),
                limit,
            });
        }
        entry.ws_connections += 1;
        Ok(())
    }

    /// Decrement the WebSocket connection count for `tenant_id`.
    pub fn release_connection(&self, tenant_id: &str) {
        let mut counters = self.counters.lock().unwrap();
        if let Some(entry) = counters.get_mut(tenant_id) {
            entry.ws_connections = entry.ws_connections.saturating_sub(1);
        }
    }

    // ── diagnostics ───────────────────────────────────────────────────────

    /// Current queued-ops count for `tenant_id`.
    pub fn queued_ops(&self, tenant_id: &str) -> u32 {
        self.counters
            .lock()
            .unwrap()
            .get(tenant_id)
            .map(|c| c.queued_ops)
            .unwrap_or(0)
    }

    /// Current WebSocket connection count for `tenant_id`.
    pub fn ws_connections(&self, tenant_id: &str) -> u32 {
        self.counters
            .lock()
            .unwrap()
            .get(tenant_id)
            .map(|c| c.ws_connections)
            .unwrap_or(0)
    }
}

impl Default for LimitsGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_are_correct() {
        let limits = TenantLimits::default();
        assert_eq!(limits.max_queued_ops, DEFAULT_MAX_QUEUED_OPS);
        assert_eq!(limits.max_ws_connections, DEFAULT_MAX_WS_CONNECTIONS);
    }

    #[test]
    fn enqueue_up_to_limit() {
        let guard = LimitsGuard::new();
        guard.set_limits(
            "t1",
            TenantLimits {
                max_queued_ops: 3,
                max_ws_connections: 10,
            },
        );

        assert!(guard.try_enqueue_op("t1").is_ok());
        assert!(guard.try_enqueue_op("t1").is_ok());
        assert!(guard.try_enqueue_op("t1").is_ok());

        // 4th op should be rejected
        let err = guard.try_enqueue_op("t1").unwrap_err();
        assert!(matches!(err, BackpressureSignal::QueueFull { .. }));
        assert_eq!(guard.queued_ops("t1"), 3);
    }

    #[test]
    fn dequeue_decrements_counter() {
        let guard = LimitsGuard::new();
        guard.try_enqueue_op("t2").unwrap();
        guard.try_enqueue_op("t2").unwrap();
        guard.dequeue_op("t2");
        assert_eq!(guard.queued_ops("t2"), 1);
    }

    #[test]
    fn dequeue_below_zero_saturates() {
        let guard = LimitsGuard::new();
        guard.dequeue_op("t3"); // nothing queued yet
        assert_eq!(guard.queued_ops("t3"), 0);
    }

    #[test]
    fn ws_connections_enforced() {
        let guard = LimitsGuard::new();
        guard.set_limits(
            "t4",
            TenantLimits {
                max_queued_ops: 500,
                max_ws_connections: 2,
            },
        );

        assert!(guard.try_register_connection("t4").is_ok());
        assert!(guard.try_register_connection("t4").is_ok());

        let err = guard.try_register_connection("t4").unwrap_err();
        assert!(matches!(err, BackpressureSignal::TooManyConnections { .. }));
    }

    #[test]
    fn release_connection_decrements() {
        let guard = LimitsGuard::new();
        guard.try_register_connection("t5").unwrap();
        guard.try_register_connection("t5").unwrap();
        guard.release_connection("t5");
        assert_eq!(guard.ws_connections("t5"), 1);
    }

    #[test]
    fn tenants_are_isolated() {
        let guard = LimitsGuard::new();
        guard.set_limits(
            "a",
            TenantLimits {
                max_queued_ops: 1,
                max_ws_connections: 1,
            },
        );
        guard.set_limits(
            "b",
            TenantLimits {
                max_queued_ops: 1,
                max_ws_connections: 1,
            },
        );

        guard.try_enqueue_op("a").unwrap();
        // tenant "b" counter is independent
        assert!(guard.try_enqueue_op("b").is_ok());
    }

    #[test]
    fn custom_limits_override_defaults() {
        let guard = LimitsGuard::new();
        guard.set_limits(
            "custom",
            TenantLimits {
                max_queued_ops: 10,
                max_ws_connections: 5,
            },
        );
        let limits = guard.limits_for("custom");
        assert_eq!(limits.max_queued_ops, 10);
        assert_eq!(limits.max_ws_connections, 5);
    }

    #[test]
    fn unknown_tenant_uses_defaults() {
        let guard = LimitsGuard::new();
        let limits = guard.limits_for("unknown");
        assert_eq!(limits.max_queued_ops, DEFAULT_MAX_QUEUED_OPS);
    }
}
