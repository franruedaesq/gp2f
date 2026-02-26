//! Canary rollout controller for AI features.
//!
//! Implements Phase 10 requirement 4: feature flag `ai_enabled` per tenant +
//! per-workflow, with automatic rollback when `agent_tool_failure_rate > 0.1 %`
//! over a rolling 5-minute window.
//!
//! ## Design
//!
//! * [`CanaryRegistry`] holds per-tenant, per-workflow flags.
//! * [`FailureTracker`] tracks tool-call outcomes in a ring buffer keyed by
//!   `(tenant_id, workflow_id)`.  When the failure rate in the last 5 minutes
//!   exceeds [`ROLLBACK_FAILURE_RATE_THRESHOLD`] the flag is automatically
//!   disabled and a `canary_rollback` tracing event is emitted.
//! * All state is in-process.  In production, replicate via Redis pub/sub or
//!   a Temporal workflow so all pods share the same flag state.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

// ── constants ─────────────────────────────────────────────────────────────────

/// Failure-rate threshold that triggers automatic rollback (0.1 %).
pub const ROLLBACK_FAILURE_RATE_THRESHOLD: f64 = 0.001;

/// Rolling window length for failure-rate calculation.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);

// ── feature flags ─────────────────────────────────────────────────────────────

/// Per-tenant, per-workflow AI feature flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiFeatureFlag {
    pub tenant_id: String,
    /// `None` means the flag applies to every workflow for this tenant.
    pub workflow_id: Option<String>,
    pub enabled: bool,
}

impl AiFeatureFlag {
    pub fn new(tenant_id: impl Into<String>, workflow_id: Option<String>, enabled: bool) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            workflow_id,
            enabled,
        }
    }
}

// ── failure tracker ───────────────────────────────────────────────────────────

/// A timestamped tool-call outcome sample.
#[derive(Debug, Clone)]
struct Sample {
    ts: Instant,
    failed: bool,
}

/// Sliding-window failure-rate tracker for a single `(tenant, workflow)` key.
struct FailureWindow {
    samples: Vec<Sample>,
}

impl FailureWindow {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
        }
    }

    fn record(&mut self, failed: bool) {
        self.samples.push(Sample {
            ts: Instant::now(),
            failed,
        });
    }

    /// Failure rate over the last [`FAILURE_WINDOW`].  Returns `None` when
    /// there are no samples in the window.
    fn failure_rate(&mut self) -> Option<f64> {
        let cutoff = Instant::now() - FAILURE_WINDOW;
        self.samples.retain(|s| s.ts >= cutoff);
        if self.samples.is_empty() {
            return None;
        }
        let failed = self.samples.iter().filter(|s| s.failed).count();
        Some(failed as f64 / self.samples.len() as f64)
    }
}

// ── registry ──────────────────────────────────────────────────────────────────

/// Canary rollout registry.
///
/// Thread-safe; cheap to clone (backed by `Arc<Mutex<…>>`).
pub struct CanaryRegistry {
    flags: Mutex<HashMap<(String, Option<String>), bool>>,
    windows: Mutex<HashMap<(String, String), FailureWindow>>,
}

impl CanaryRegistry {
    pub fn new() -> Self {
        Self {
            flags: Mutex::new(HashMap::new()),
            windows: Mutex::new(HashMap::new()),
        }
    }

    // ── flag management ───────────────────────────────────────────────────

    /// Set the `ai_enabled` flag for a tenant (optionally scoped to a workflow).
    pub fn set_flag(&self, tenant_id: &str, workflow_id: Option<&str>, enabled: bool) {
        let key = (tenant_id.to_owned(), workflow_id.map(ToOwned::to_owned));
        self.flags.lock().unwrap().insert(key, enabled);
    }

    /// Return `true` when AI is enabled for `(tenant_id, workflow_id)`.
    ///
    /// Look-up order:
    /// 1. Exact `(tenant_id, workflow_id)` match.
    /// 2. Tenant-wide `(tenant_id, None)` match.
    /// 3. Default: **enabled** (opt-in after first explicit disable).
    pub fn is_enabled(&self, tenant_id: &str, workflow_id: &str) -> bool {
        let flags = self.flags.lock().unwrap();
        // Exact match
        if let Some(&v) = flags.get(&(tenant_id.to_owned(), Some(workflow_id.to_owned()))) {
            return v;
        }
        // Tenant-wide
        if let Some(&v) = flags.get(&(tenant_id.to_owned(), None)) {
            return v;
        }
        // Default: enabled
        true
    }

    // ── failure tracking ──────────────────────────────────────────────────

    /// Record a tool-call outcome.  Triggers automatic rollback when the
    /// 5-minute failure rate exceeds [`ROLLBACK_FAILURE_RATE_THRESHOLD`].
    pub fn record_outcome(&self, tenant_id: &str, workflow_id: &str, failed: bool) {
        let key = (tenant_id.to_owned(), workflow_id.to_owned());
        let mut windows = self.windows.lock().unwrap();
        let window = windows.entry(key).or_insert_with(FailureWindow::new);
        window.record(failed);

        if let Some(rate) = window.failure_rate() {
            if rate > ROLLBACK_FAILURE_RATE_THRESHOLD {
                // Automatic rollback
                drop(windows); // release before acquiring flags lock
                self.set_flag(tenant_id, Some(workflow_id), false);
                tracing::warn!(
                    tenant_id,
                    workflow_id,
                    failure_rate = %format!("{:.4}%", rate * 100.0),
                    "canary rollback triggered: agent_tool_failure_rate exceeds threshold"
                );
            }
        }
    }

    /// Return the current failure rate for `(tenant_id, workflow_id)` or `None`
    /// when no samples are in the window.
    pub fn failure_rate(&self, tenant_id: &str, workflow_id: &str) -> Option<f64> {
        let key = (tenant_id.to_owned(), workflow_id.to_owned());
        self.windows.lock().unwrap().get_mut(&key)?.failure_rate()
    }
}

impl Default for CanaryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_flag_is_enabled() {
        let reg = CanaryRegistry::new();
        assert!(reg.is_enabled("any_tenant", "any_workflow"));
    }

    #[test]
    fn explicit_disable_is_respected() {
        let reg = CanaryRegistry::new();
        reg.set_flag("t1", Some("wf1"), false);
        assert!(!reg.is_enabled("t1", "wf1"));
        // Other workflow for same tenant should still be enabled
        assert!(reg.is_enabled("t1", "wf2"));
    }

    #[test]
    fn tenant_wide_flag_applies_to_all_workflows() {
        let reg = CanaryRegistry::new();
        reg.set_flag("t2", None, false);
        assert!(!reg.is_enabled("t2", "wf1"));
        assert!(!reg.is_enabled("t2", "wf2"));
    }

    #[test]
    fn workflow_flag_overrides_tenant_flag() {
        let reg = CanaryRegistry::new();
        reg.set_flag("t3", None, false); // disable tenant-wide
        reg.set_flag("t3", Some("wf_special"), true); // re-enable for one workflow
        assert!(reg.is_enabled("t3", "wf_special"));
        assert!(!reg.is_enabled("t3", "wf_other"));
    }

    #[test]
    fn enable_after_disable() {
        let reg = CanaryRegistry::new();
        reg.set_flag("t4", None, false);
        assert!(!reg.is_enabled("t4", "wf1"));
        reg.set_flag("t4", None, true);
        assert!(reg.is_enabled("t4", "wf1"));
    }

    #[test]
    fn failure_rate_below_threshold_does_not_rollback() {
        let reg = CanaryRegistry::new();
        // Record 10_000 calls, all successful
        for _ in 0..10_000 {
            reg.record_outcome("t5", "wf1", false);
        }
        // AI should still be enabled
        assert!(reg.is_enabled("t5", "wf1"));
    }

    #[test]
    fn failure_rate_above_threshold_triggers_rollback() {
        let reg = CanaryRegistry::new();
        // Record 1000 calls: 10 failed = 1.0 % > 0.1 %
        for _ in 0..990 {
            reg.record_outcome("t6", "wf1", false);
        }
        for _ in 0..10 {
            reg.record_outcome("t6", "wf1", true);
        }
        // Rollback should have been triggered
        assert!(!reg.is_enabled("t6", "wf1"));
    }

    #[test]
    fn rollback_is_per_workflow() {
        let reg = CanaryRegistry::new();
        // Trigger rollback for wf1
        for _ in 0..990 {
            reg.record_outcome("t7", "wf1", false);
        }
        for _ in 0..10 {
            reg.record_outcome("t7", "wf1", true);
        }
        // wf2 should remain enabled
        assert!(!reg.is_enabled("t7", "wf1"));
        assert!(reg.is_enabled("t7", "wf2"));
    }

    #[test]
    fn failure_rate_returns_none_when_no_samples() {
        let reg = CanaryRegistry::new();
        assert!(reg.failure_rate("unknown_tenant", "unknown_wf").is_none());
    }

    #[test]
    fn failure_rate_is_accurate() {
        let reg = CanaryRegistry::new();
        // 1 failed out of 4 = 25 %
        reg.record_outcome("t8", "wf1", false);
        reg.record_outcome("t8", "wf1", false);
        reg.record_outcome("t8", "wf1", false);
        reg.record_outcome("t8", "wf1", true);
        let rate = reg.failure_rate("t8", "wf1").unwrap();
        assert!((rate - 0.25).abs() < 1e-9);
    }
}
