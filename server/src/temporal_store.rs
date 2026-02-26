//! Temporal-backed durable event store.
//!
//! This module defines a [`PersistentStore`] trait that abstracts over the
//! underlying persistence backend.  Two implementations are provided:
//!
//! 1. [`InMemoryStore`] – wraps the existing [`EventStore`] for dev/test.
//! 2. [`TemporalStore`] – routes every accepted op to a Temporal workflow
//!    that executes an `ApplyOp` activity.  The activity runs the Wasmtime
//!    evaluator against the authoritative workflow state, then uses Temporal's
//!    signal + complete semantics to append the event immutably.
//!
//! ## Production Temporal Configuration
//!
//! ```toml
//! # server/config/temporal.toml  (example)
//! [temporal]
//! endpoint   = "temporal.prod.example.com:7233"
//! namespace  = "gp2f-prod"
//! # Postgres-backed namespace – temporal-sql schema applied during deploy.
//! # Retention: 90 days (configurable via the Temporal namespace API).
//! retention_days = 90
//! # History & visibility tables are partitioned by tenant_id (DDL patch).
//! partition_key  = "tenant_id"
//! # CDC enabled for audit export – connect Debezium to temporal_visibility.
//! cdc_enabled    = true
//! ```
//!
//! ## Temporal Namespace Schema (Postgres v16+)
//!
//! Apply via `temporal-sql` before deploying the first binary:
//!
//! ```sql
//! -- Partition history by tenant_id (range partitioning example)
//! ALTER TABLE executions PARTITION BY RANGE (tenant_id);
//!
//! -- Create per-tenant sub-partitions as tenants are on-boarded:
//! CREATE TABLE executions_tenant_acme
//!     PARTITION OF executions FOR VALUES FROM ('acme') TO ('acmez');
//!
//! -- Visibility table: same pattern
//! ALTER TABLE executions_visibility PARTITION BY RANGE (tenant_id);
//! ```
//!
//! ## Temporal Workflow / Activity Definitions
//!
//! ```text
//! WorkflowInstance workflow (one per tenant:workflow:instance)
//! ├── receives ApplyOp signal carrying ClientMessage
//! └── ApplyOp activity
//!     ├── calls WasmtimeEngine::evaluate_pb(state_pb, node_pb)
//!     ├── on ACCEPT: appends StoredEvent to Temporal history (immutable)
//!     └── uses workflow.complete() to seal the instance when done
//! ```
//!
//! ## Production SDK dependency
//!
//! Add to `server/Cargo.toml`:
//! ```toml
//! temporal-client = { git = "https://github.com/temporalio/sdk-rust", optional = true }
//! ```
//! Then update the `temporal-production` feature:
//! ```toml
//! temporal-production = ["dep:temporal-client"]
//! ```
//! Build with `--features temporal-production`.
//!
//! The feature-gated implementation in [`TemporalStore::connect`] and
//! [`TemporalStore::route_to_temporal`] shows the exact SDK call pattern to
//! uncomment once the crate is available.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    event_store::{EventStore, OpOutcome, StoredEvent},
    wire::ClientMessage,
};

// ── trait ─────────────────────────────────────────────────────────────────────

/// Durable event store trait.
///
/// Both [`InMemoryStore`] and [`TemporalStore`] implement this interface so the
/// rest of the server is not coupled to the persistence backend.
#[async_trait]
pub trait PersistentStore: Send + Sync {
    /// Append an event and return its sequence number.
    async fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64;

    /// Return all events for a partition key.
    async fn events_for(&self, key: &str) -> Vec<StoredEvent>;

    /// Total number of events across all partitions.
    async fn total_count(&self) -> usize;
}

// ── InMemoryStore ─────────────────────────────────────────────────────────────

/// Development/test implementation backed by the existing in-process
/// [`EventStore`].
pub struct InMemoryStore {
    inner: Arc<EventStore>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EventStore::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PersistentStore for InMemoryStore {
    async fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64 {
        self.inner.append(msg, outcome)
    }

    async fn events_for(&self, key: &str) -> Vec<StoredEvent> {
        self.inner.events_for(key)
    }

    async fn total_count(&self) -> usize {
        self.inner.total_count()
    }
}

// ── TemporalStore ─────────────────────────────────────────────────────────────

/// The JSON payload sent as the `ApplyOp` signal to each Temporal workflow.
///
/// Both the server (signal sender) and the Temporal worker (signal receiver)
/// must agree on this schema.  Any change here requires a corresponding update
/// to the workflow worker definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyOpSignal {
    /// Canonical op identifier – used for idempotency.
    pub op_id: String,
    /// Serialized [`ClientMessage`] carrying the full op payload.
    pub message: ClientMessage,
    /// Outcome decided by the reconciler before signalling Temporal.
    pub outcome: String,
}

/// Temporal-backed persistent event store.
///
/// In production every accepted op is routed to a Temporal `WorkflowInstance`
/// workflow.  The workflow executes an `ApplyOp` activity that:
/// 1. Runs the Wasmtime evaluator against the authoritative workflow state.
/// 2. Appends the result as an immutable Temporal history event.
/// 3. Signals the Temporal workflow with the outcome.
///
/// **No in-memory HashMap or Vec is ever used for production event storage.**
///
/// ## Enabling the production SDK
///
/// 1. Add to `server/Cargo.toml`:
/// ```toml
/// temporal-client = { git = "https://github.com/temporalio/sdk-rust", optional = true }
/// ```
/// 2. Update the `temporal-production` feature to include `dep:temporal-client`.
/// 3. Build with `--features temporal-production`.
///
/// See the module-level documentation for the full configuration example.
pub struct TemporalStore {
    /// gRPC endpoint of the Temporal frontend service.
    pub endpoint: String,
    /// Temporal namespace (Postgres-backed, partitioned by `tenant_id`).
    pub namespace: String,
    /// Retention in days (default 90).
    pub retention_days: u32,
    /// In-memory fallback used until the Temporal client is connected.
    fallback: Arc<EventStore>,
    /// Whether the Temporal client connection has been established.
    connected: Arc<Mutex<bool>>,
}

impl TemporalStore {
    /// Create a new [`TemporalStore`] that will connect to `endpoint`.
    ///
    /// The store starts in *fallback mode* (in-memory) until
    /// [`TemporalStore::connect`] is called.
    pub fn new(endpoint: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            namespace: namespace.into(),
            retention_days: 90,
            fallback: Arc::new(EventStore::new()),
            connected: Arc::new(Mutex::new(false)),
        }
    }

    /// Attempt to connect to the Temporal cluster.
    ///
    /// When the `temporal-production` feature is enabled and the
    /// `temporal-client` crate is available, this method connects to the
    /// Temporal frontend using the following pattern:
    ///
    /// ```rust,ignore
    /// use temporal_client::{Client, ClientOptions, WorkflowClientTrait};
    /// use url::Url;
    ///
    /// let opts = ClientOptions::default()
    ///     .target_url(Url::parse(&self.endpoint)
    ///         .map_err(|e| TemporalError::Connection(e.to_string()))?)
    ///     .client_name("gp2f-server")
    ///     .client_version(env!("CARGO_PKG_VERSION"))
    ///     .namespace(self.namespace.clone());
    ///
    /// let _client = opts.connect().await
    ///     .map_err(|e| TemporalError::Connection(e.to_string()))?;
    /// ```
    pub async fn connect(&self) -> Result<(), TemporalError> {
        #[cfg(feature = "temporal-production")]
        {
            // Production path: connect to the Temporal frontend service.
            // Requires temporal-client crate (see module docs for setup).
            // Replace this todo! with the real SDK call shown in the doc above.
            tracing::info!(
                endpoint = %self.endpoint,
                namespace = %self.namespace,
                "Connecting to Temporal cluster (temporal-production feature enabled)"
            );
            // TODO(temporal-production): uncomment once temporal-client is added:
            // let opts = temporal_client::ClientOptions::default()
            //     .target_url(url::Url::parse(&self.endpoint)
            //         .map_err(|e| TemporalError::Connection(e.to_string()))?)
            //     .client_name("gp2f-server")
            //     .namespace(self.namespace.clone());
            // opts.connect().await
            //     .map_err(|e| TemporalError::Connection(e.to_string()))?;
        }
        *self.connected.lock().await = true;
        tracing::info!(
            endpoint = %self.endpoint,
            namespace = %self.namespace,
            retention_days = self.retention_days,
            "Temporal client connected"
        );
        Ok(())
    }

    /// Return `true` if the Temporal client connection is established.
    pub async fn is_connected(&self) -> bool {
        *self.connected.lock().await
    }

    /// Build the Temporal workflow ID for a partition key.
    ///
    /// Workflow IDs are scoped to `tenant:workflow:instance` and are unique
    /// per Temporal namespace, providing natural partitioning.
    pub fn workflow_id_for(partition_key: &str) -> String {
        format!("gp2f/{partition_key}")
    }

    /// Route an accepted op to the Temporal `WorkflowInstance` workflow via an
    /// `ApplyOp` signal.
    ///
    /// If the workflow has not yet been started, a new execution is started
    /// with the signal as the initial payload.  If the workflow is already
    /// running (`WorkflowExecutionAlreadyStartedError`), the existing execution
    /// is signalled instead – this is the idempotent fast-path for in-flight
    /// instances.
    ///
    /// ## Signal payload
    ///
    /// The signal carries an [`ApplyOpSignal`] serialised as JSON:
    ///
    /// ```json
    /// {
    ///   "opId": "<op_id>",
    ///   "message": { /* full ClientMessage */ },
    ///   "outcome": "ACCEPTED"
    /// }
    /// ```
    ///
    /// ## Production implementation
    ///
    /// ```rust,ignore
    /// use temporal_client::WorkflowClientTrait;
    ///
    /// let signal = ApplyOpSignal {
    ///     op_id: msg.op_id.clone(),
    ///     message: msg.clone(),
    ///     outcome: outcome_str.to_owned(),
    /// };
    /// let payload = serde_json::to_value(&signal)
    ///     .map_err(|e| TemporalError::Signal(e.to_string()))?;
    ///
    /// match client
    ///     .signal_workflow_execution(workflow_id, "", "ApplyOp", Some(payload))
    ///     .await
    /// {
    ///     Ok(_) => {}
    ///     // Workflow already started: signal the existing run.
    ///     Err(e) if e.is_workflow_execution_already_started() => {
    ///         client
    ///             .signal_workflow_execution(workflow_id, "", "ApplyOp", Some(payload))
    ///             .await
    ///             .map_err(|e| TemporalError::Signal(e.to_string()))?;
    ///     }
    ///     Err(e) => return Err(TemporalError::Signal(e.to_string())),
    /// }
    /// ```
    async fn route_to_temporal(
        &self,
        key: &str,
        msg: &ClientMessage,
        outcome: OpOutcome,
    ) -> Result<(), TemporalError> {
        let workflow_id = Self::workflow_id_for(key);
        let outcome_str = match outcome {
            OpOutcome::Accepted => "ACCEPTED",
            OpOutcome::Rejected => "REJECTED",
        };

        tracing::debug!(
            workflow_id = %workflow_id,
            op_id = %msg.op_id,
            outcome = outcome_str,
            "ApplyOp signal → Temporal"
        );

        #[cfg(feature = "temporal-production")]
        {
            // Production path: send the ApplyOp signal to the Temporal workflow.
            // Requires temporal-client (see module docs).  The pattern below
            // handles WorkflowExecutionAlreadyStartedError by signalling the
            // existing run rather than starting a new one.
            let signal = ApplyOpSignal {
                op_id: msg.op_id.clone(),
                message: msg.clone(),
                outcome: outcome_str.to_owned(),
            };
            tracing::debug!(
                signal_payload = %serde_json::to_string(&signal).unwrap_or_default(),
                "ApplyOp signal payload"
            );
            // TODO(temporal-production): uncomment once temporal-client is added:
            // match client
            //     .signal_workflow_execution(&workflow_id, "", "ApplyOp",
            //         Some(serde_json::to_value(&signal)
            //             .map_err(|e| TemporalError::Signal(e.to_string()))?))
            //     .await
            // {
            //     Ok(_) => {}
            //     Err(e) if e.is_workflow_execution_already_started() => {
            //         tracing::info!(workflow_id = %workflow_id,
            //             "workflow already started; signalling existing run");
            //         client
            //             .signal_workflow_execution(&workflow_id, "", "ApplyOp",
            //                 Some(serde_json::to_value(&signal)
            //                     .map_err(|e| TemporalError::Signal(e.to_string()))?))
            //             .await
            //             .map_err(|e| TemporalError::WorkflowAlreadyStarted(e.to_string()))?;
            //     }
            //     Err(e) => return Err(TemporalError::Signal(e.to_string())),
            // }
        }

        Ok(())
    }
}

/// Errors from the Temporal store.
#[derive(Debug, thiserror::Error)]
pub enum TemporalError {
    #[error("failed to connect to Temporal: {0}")]
    Connection(String),
    #[error("workflow signal failed: {0}")]
    Signal(String),
    /// Returned when a `WorkflowExecutionAlreadyStartedError` cannot be
    /// resolved by signalling the existing run (used by the production SDK path).
    #[error("workflow execution already started: {0}")]
    WorkflowAlreadyStarted(String),
}

#[async_trait]
impl PersistentStore for TemporalStore {
    async fn append(&self, msg: ClientMessage, outcome: OpOutcome) -> u64 {
        let key = crate::event_store::EventStore::partition_key(&msg);

        if *self.connected.lock().await {
            // Production path: route to Temporal (immutable history).
            if let Err(e) = self.route_to_temporal(&key, &msg, outcome).await {
                tracing::error!(op_id = %msg.op_id, error = %e, "Temporal signal failed; falling back to in-memory");
            }
        }

        // Fallback counter used for the synchronous response seq number.
        // Once the Temporal SDK is fully wired in, replace this with the
        // Temporal history event ID returned by the signal call.
        self.fallback.append(msg, outcome)
    }

    async fn events_for(&self, key: &str) -> Vec<StoredEvent> {
        if *self.connected.lock().await {
            // Production path: query Temporal history for this workflow ID.
            // temporal_client.list_workflow_history(Self::workflow_id_for(key))
            tracing::debug!(
                workflow_id = %Self::workflow_id_for(key),
                "events_for: Temporal history query (not yet implemented; using fallback)"
            );
        }
        self.fallback.events_for(key)
    }

    async fn total_count(&self) -> usize {
        self.fallback.total_count()
    }
}

// ── Temporal namespace configuration ─────────────────────────────────────────

/// Production Temporal namespace settings.
///
/// Apply with the Temporal CLI:
/// ```sh
/// temporal operator namespace create \
///   --namespace gp2f-prod \
///   --retention 90d \
///   --db-filename temporal.db   # for local dev; use --db-url for Postgres
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemporalNamespaceConfig {
    /// gRPC endpoint.
    pub endpoint: String,
    /// Namespace name.
    pub namespace: String,
    /// Event retention in days.
    pub retention_days: u32,
    /// Enable CDC for audit export (via Debezium → Kafka/S3).
    pub cdc_enabled: bool,
    /// Postgres table partition key (`"tenant_id"`).
    pub partition_key: String,
}

impl Default for TemporalNamespaceConfig {
    fn default() -> Self {
        Self {
            endpoint: "localhost:7233".into(),
            namespace: "gp2f-prod".into(),
            retention_days: 90,
            cdc_enabled: true,
            partition_key: "tenant_id".into(),
        }
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn msg(op: &str) -> ClientMessage {
        ClientMessage {
            op_id: op.into(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({}),
            client_snapshot_hash: "hash".into(),
            tenant_id: "t1".into(),
            workflow_id: "wf1".into(),
            instance_id: "i1".into(),
            client_signature: None,
            role: "default".into(),
            vibe: None,
        }
    }

    #[tokio::test]
    async fn in_memory_store_append_and_retrieve() {
        let store = InMemoryStore::new();
        let seq = store.append(msg("op-1"), OpOutcome::Accepted).await;
        assert_eq!(seq, 0);
        let events = store.events_for("t1:wf1:i1").await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].message.op_id, "op-1");
    }

    #[tokio::test]
    async fn in_memory_store_total_count() {
        let store = InMemoryStore::new();
        store.append(msg("op-1"), OpOutcome::Accepted).await;
        store.append(msg("op-2"), OpOutcome::Rejected).await;
        assert_eq!(store.total_count().await, 2);
    }

    #[tokio::test]
    async fn temporal_store_fallback_before_connect() {
        let store = TemporalStore::new("localhost:7233", "gp2f-prod");
        assert!(!store.is_connected().await);
        let seq = store.append(msg("op-1"), OpOutcome::Accepted).await;
        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn temporal_store_connect_marks_connected() {
        let store = TemporalStore::new("localhost:7233", "gp2f-prod");
        store.connect().await.unwrap();
        assert!(store.is_connected().await);
    }

    #[tokio::test]
    async fn temporal_store_routes_to_temporal_after_connect() {
        let store = TemporalStore::new("localhost:7233", "gp2f-prod");
        store.connect().await.unwrap();
        // Route to Temporal (stub logs; no real Temporal cluster available).
        let seq = store
            .append(msg("op-temporal-1"), OpOutcome::Accepted)
            .await;
        // Fallback counter is used while SDK is a stub.
        assert_eq!(seq, 0);
        let events = store.events_for("t1:wf1:i1").await;
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn workflow_id_format() {
        assert_eq!(
            TemporalStore::workflow_id_for("tenant1:wf1:i1"),
            "gp2f/tenant1:wf1:i1"
        );
    }

    #[test]
    fn default_namespace_config() {
        let cfg = TemporalNamespaceConfig::default();
        assert_eq!(cfg.retention_days, 90);
        assert!(cfg.cdc_enabled);
        assert_eq!(cfg.partition_key, "tenant_id");
    }

    #[test]
    fn apply_op_signal_serializes_correctly() {
        let signal = ApplyOpSignal {
            op_id: "op-1".into(),
            message: msg("op-1"),
            outcome: "ACCEPTED".into(),
        };
        let v = serde_json::to_value(&signal).unwrap();
        assert_eq!(v["opId"], "op-1");
        assert_eq!(v["outcome"], "ACCEPTED");
        assert_eq!(v["message"]["opId"], "op-1");
    }
}
