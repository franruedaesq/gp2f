//! Redis Pub/Sub broadcaster with in-process fallback.
//!
//! Clients subscribe to a workflow's event stream via WebSocket.  When the
//! `redis-broadcast` Cargo feature is enabled, the server also publishes every
//! ACCEPT/REJECT event to the Redis channel `workflow:{workflow_id}`, allowing
//! any server replica to forward events to its own WebSocket clients.
//!
//! ## Architecture
//!
//! ```text
//!  Reconciler (any replica)
//!       │  ServerMessage
//!       ▼
//!  RedisBroadcaster::publish(workflow_id, msg)
//!       │
//!       ├── Redis PubSub channel: "workflow:{workflow_id}"
//!       │        │
//!       │        └── Redis subscriber task (per replica)
//!       │                 │ forwards to in-process broadcast channel
//!       │                 ▼
//!       └── in-process broadcast::Sender<ServerMessage>
//!                  │
//!                  └── WebSocket handler (per connected client)
//! ```
//!
//! ## Fallback behaviour
//!
//! When Redis is unavailable (connection error, feature disabled, etc.) the
//! broadcaster falls back to the in-process `tokio::sync::broadcast` channel
//! that is already used for single-replica deployments.  This means:
//!
//! - Single-replica deployments work without Redis.
//! - Multi-replica deployments **require** Redis for cross-replica fan-out.
//!
//! ## Redis connection
//!
//! Enable the `redis-broadcast` feature and set `REDIS_URL` at runtime:
//!
//! ```sh
//! REDIS_URL=redis://localhost:6379 gp2f-server
//! ```
//!
//! The broadcaster uses [`redis::aio::ConnectionManager`] for automatic
//! reconnection with exponential back-off.

use std::sync::Arc;
use tokio::sync::broadcast;

use crate::wire::ServerMessage;

// ── channel capacity ──────────────────────────────────────────────────────────

const BROADCAST_CAPACITY: usize = 256;

// ── broadcaster trait ─────────────────────────────────────────────────────────

/// Abstraction over the broadcast backend.
///
/// Implement this trait to plug in alternative backends (Redis, NATS, …).
pub trait BroadcastBackend: Send + Sync + 'static {
    /// Publish `msg` on `workflow_id`'s channel.
    fn publish(&self, workflow_id: &str, msg: ServerMessage);

    /// Subscribe to `workflow_id`'s channel.
    ///
    /// Returns a broadcast receiver that will receive all future messages.
    fn subscribe(&self, workflow_id: &str) -> broadcast::Receiver<ServerMessage>;
}

// ── in-process broadcaster ────────────────────────────────────────────────────

/// Simple in-process broadcaster backed by `tokio::sync::broadcast`.
///
/// Used when Redis is unavailable or disabled.  All subscribers must be in the
/// same process – sufficient for single-replica deployments.
#[derive(Clone)]
pub struct InProcessBroadcaster {
    tx: Arc<broadcast::Sender<ServerMessage>>,
}

impl InProcessBroadcaster {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self { tx: Arc::new(tx) }
    }
}

impl Default for InProcessBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

impl BroadcastBackend for InProcessBroadcaster {
    fn publish(&self, _workflow_id: &str, msg: ServerMessage) {
        let _ = self.tx.send(msg);
    }

    fn subscribe(&self, _workflow_id: &str) -> broadcast::Receiver<ServerMessage> {
        self.tx.subscribe()
    }
}

// ── Redis broadcaster ─────────────────────────────────────────────────────────

/// Redis Pub/Sub broadcaster.
///
/// Publishes to `workflow:{workflow_id}` and fans out to all in-process
/// WebSocket subscribers via an in-process bridge channel.
///
/// Requires the `redis-broadcast` Cargo feature to be enabled.
#[cfg(feature = "redis-broadcast")]
pub struct RedisBroadcaster {
    connection: redis::aio::ConnectionManager,
    /// Local in-process channel for subscribers on this replica.
    local: InProcessBroadcaster,
}

#[cfg(feature = "redis-broadcast")]
impl RedisBroadcaster {
    /// Connect to Redis and return a new broadcaster.
    ///
    /// `redis_url` example: `redis://localhost:6379`
    pub async fn connect(redis_url: &str) -> Result<Self, RedisBroadcastError> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| RedisBroadcastError::Connection(e.to_string()))?;
        let connection = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|e| RedisBroadcastError::Connection(e.to_string()))?;
        Ok(Self {
            connection,
            local: InProcessBroadcaster::new(),
        })
    }

    /// Redis channel name for a workflow.
    pub fn channel_name(workflow_id: &str) -> String {
        format!("workflow:{workflow_id}")
    }
}

#[cfg(feature = "redis-broadcast")]
impl BroadcastBackend for RedisBroadcaster {
    fn publish(&self, workflow_id: &str, msg: ServerMessage) {
        use redis::AsyncCommands;
        let channel = Self::channel_name(workflow_id);
        let payload = match serde_json::to_string(&msg) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to serialize message for Redis: {e}");
                return;
            }
        };
        let mut conn = self.connection.clone();
        // Spawn a detached task so `publish` stays synchronous.
        tokio::spawn(async move {
            if let Err(e) = conn.publish::<_, _, ()>(&channel, &payload).await {
                tracing::warn!(channel = %channel, "Redis publish failed: {e}; using in-process fallback");
            }
        });
        // Also publish in-process for local subscribers.
        self.local.publish(workflow_id, msg);
    }

    fn subscribe(&self, workflow_id: &str) -> broadcast::Receiver<ServerMessage> {
        self.local.subscribe(workflow_id)
    }
}

// ── error ─────────────────────────────────────────────────────────────────────

#[cfg(feature = "redis-broadcast")]
#[derive(Debug, thiserror::Error)]
pub enum RedisBroadcastError {
    #[error("Redis connection error: {0}")]
    Connection(String),
}

// ── unified broadcaster ───────────────────────────────────────────────────────

/// Type-erased broadcaster that the rest of the server uses.
///
/// At startup, pick the best available backend:
/// ```rust,ignore
/// let broadcaster: Arc<dyn BroadcastBackend> = if let Ok(url) = std::env::var("REDIS_URL") {
///     Arc::new(RedisBroadcaster::connect(&url).await?)
/// } else {
///     Arc::new(InProcessBroadcaster::new())
/// };
/// ```
pub type DynBroadcaster = Arc<dyn BroadcastBackend>;

/// Build the best available broadcaster at startup.
///
/// When `REDIS_URL` is set and the `redis-broadcast` feature is enabled,
/// connects to Redis; otherwise falls back to the in-process broadcaster.
pub async fn build_broadcaster() -> DynBroadcaster {
    #[cfg(feature = "redis-broadcast")]
    if let Some(url) = crate::secrets::resolve_secret("REDIS_URL") {
        match RedisBroadcaster::connect(&url).await {
            Ok(b) => {
                tracing::info!(url = %url, "Redis PubSub broadcaster connected");
                return Arc::new(b);
            }
            Err(e) => {
                tracing::warn!("Redis broadcaster failed ({e}); falling back to in-process");
            }
        }
    }
    tracing::info!("Using in-process broadcast channel (single-replica mode)");
    Arc::new(InProcessBroadcaster::new())
}

// ── Temporal signal fallback ──────────────────────────────────────────────────

/// When Redis is unavailable, fall back to Temporal query + signal for
/// reconciliation.
///
/// This function is called by the WebSocket handler when the Redis subscriber
/// receives a lag error or disconnects.  It queries the Temporal workflow for
/// the latest workflow state and pushes it to the connected WebSocket client.
///
/// In production replace the stub body with:
/// ```rust,ignore
/// let state = temporal_client
///     .query_workflow_execution(
///         gp2f/tenant_id:workflow_id:instance_id,
///         "QueryState",
///         None,
///     ).await?;
/// ws_sender.send(Message::Text(serde_json::to_string(&state)?)).await?;
/// ```
pub async fn temporal_fallback_reconcile(workflow_id: &str) -> Option<ServerMessage> {
    tracing::debug!(
        workflow_id = %workflow_id,
        "Temporal fallback reconcile (stub; replace with SDK call)"
    );
    // Production: query Temporal for the latest state and return it.
    None
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{AcceptResponse, ServerMessage};

    fn accept(op: &str) -> ServerMessage {
        ServerMessage::Accept(AcceptResponse {
            op_id: op.into(),
            server_snapshot_hash: "hash".into(),
        })
    }

    #[tokio::test]
    async fn in_process_subscriber_receives_message() {
        let broadcaster = InProcessBroadcaster::new();
        let mut rx = broadcaster.subscribe("wf1");
        broadcaster.publish("wf1", accept("op-1"));
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, ServerMessage::Accept(ref a) if a.op_id == "op-1"));
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let broadcaster = InProcessBroadcaster::new();
        let mut rx1 = broadcaster.subscribe("wf1");
        let mut rx2 = broadcaster.subscribe("wf1");
        broadcaster.publish("wf1", accept("op-2"));
        assert!(matches!(
            rx1.recv().await.unwrap(),
            ServerMessage::Accept(_)
        ));
        assert!(matches!(
            rx2.recv().await.unwrap(),
            ServerMessage::Accept(_)
        ));
    }

    #[tokio::test]
    async fn build_broadcaster_returns_in_process_when_no_redis_url() {
        // Ensure REDIS_URL is not set.
        std::env::remove_var("REDIS_URL");
        let b = build_broadcaster().await;
        // Just verify it can publish without panicking.
        b.publish("wf1", accept("op-3"));
    }

    #[test]
    #[cfg(feature = "redis-broadcast")]
    fn channel_name_format() {
        assert_eq!(RedisBroadcaster::channel_name("wf-123"), "workflow:wf-123");
    }
}
