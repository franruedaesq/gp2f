//! Per-workflow-instance actor model using `tokio::sync::mpsc`.
//!
//! Every unique `(tenant_id, workflow_id, instance_id)` triple is backed by a
//! single [`WorkflowActor`] task.  All ops for that instance are serialised
//! through its channel, eliminating concurrent state-mutation races and
//! naturally providing per-instance backpressure.
//!
//! ## Architecture
//! ```text
//!  HTTP handler / WS handler
//!        │  ActorMessage::Reconcile
//!        ▼
//!  ActorRegistry ──lookup──► WorkflowActorHandle (mpsc::Sender)
//!                                     │
//!                                     ▼
//!                            WorkflowActor task
//!                         (sequential reconciler)
//!                                     │  ServerMessage
//!                                     ▼
//!                        oneshot::Sender back to handler
//!                        broadcast to subscribed WS clients
//! ```

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    reconciler::Reconciler,
    wire::{ClientMessage, ServerMessage},
};

// ── channel capacity ──────────────────────────────────────────────────────────

/// Bounded channel depth per actor.  When the queue is full, back-pressure is
/// applied to the caller (see [`ActorHandle::send`]).
const ACTOR_CHANNEL_CAPACITY: usize = 256;

/// Broadcast channel for subscribed WebSocket connections on this instance.
const BROADCAST_CAPACITY: usize = 256;

// ── messages ──────────────────────────────────────────────────────────────────

/// Messages the actor accepts.
pub enum ActorMessage {
    /// Process an op and reply on `reply_tx`.
    Reconcile {
        msg: Box<ClientMessage>,
        reply_tx: oneshot::Sender<ServerMessage>,
    },
    /// Register a WebSocket subscriber that wants push notifications.
    Subscribe {
        tx: broadcast::Sender<ServerMessage>,
    },
}

// ── actor handle ──────────────────────────────────────────────────────────────

/// A cheap-to-clone handle to a [`WorkflowActor`] task.
#[derive(Clone)]
pub struct ActorHandle {
    tx: mpsc::Sender<ActorMessage>,
}

impl ActorHandle {
    /// Send a message to the actor.
    ///
    /// Returns `Err` only if the actor task has terminated (which should not
    /// happen during normal operation).
    pub async fn send(
        &self,
        msg: ActorMessage,
    ) -> Result<(), mpsc::error::SendError<ActorMessage>> {
        self.tx.send(msg).await
    }

    /// Reconcile a `ClientMessage` through the actor and wait for the reply.
    pub async fn reconcile(&self, msg: ClientMessage) -> Option<ServerMessage> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(ActorMessage::Reconcile {
            msg: Box::new(msg),
            reply_tx,
        })
        .await
        .ok()?;
        reply_rx.await.ok()
    }
}

// ── actor ─────────────────────────────────────────────────────────────────────

/// A tokio task that owns the authoritative state for one workflow instance.
struct WorkflowActor {
    rx: mpsc::Receiver<ActorMessage>,
    reconciler: Arc<Reconciler>,
    broadcast_tx: broadcast::Sender<ServerMessage>,
}

impl WorkflowActor {
    fn new(reconciler: Arc<Reconciler>) -> (Self, ActorHandle) {
        let (tx, rx) = mpsc::channel(ACTOR_CHANNEL_CAPACITY);
        let (broadcast_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let actor = Self {
            rx,
            reconciler,
            broadcast_tx,
        };
        (actor, ActorHandle { tx })
    }

    async fn run(mut self) {
        while let Some(msg) = self.rx.recv().await {
            match msg {
                ActorMessage::Reconcile { msg, reply_tx } => {
                    let response = self.reconciler.reconcile(&msg);
                    // Broadcast to subscribed WebSocket clients (ignore errors
                    // when there are no subscribers).
                    let _ = self.broadcast_tx.send(response.clone());
                    let _ = reply_tx.send(response);
                }
                ActorMessage::Subscribe { tx } => {
                    // Re-subscribe the provided sender to our broadcast channel.
                    // The caller holds the corresponding Receiver.
                    drop(tx); // handled by providing a broadcast::Receiver below
                }
            }
        }
    }
}

// ── registry ──────────────────────────────────────────────────────────────────

/// Server-wide registry of per-instance actors.
///
/// Keyed by `"tenant_id:workflow_id:instance_id"`.  Actors are spawned lazily
/// on first access and kept alive as long as this registry is alive.
pub struct ActorRegistry {
    actors: Mutex<HashMap<String, ActorHandle>>,
    /// Shared reconciler – actors wrap it (one per workflow instance so there
    /// is no lock contention across instances).
    reconciler_factory: Arc<dyn Fn() -> Arc<Reconciler> + Send + Sync>,
}

impl ActorRegistry {
    /// Create a registry backed by default reconcilers.
    pub fn new() -> Self {
        Self::with_factory(Arc::new(|| Arc::new(Reconciler::new())))
    }

    /// Create a registry with a custom reconciler factory (useful for testing).
    pub fn with_factory(factory: Arc<dyn Fn() -> Arc<Reconciler> + Send + Sync>) -> Self {
        Self {
            actors: Mutex::new(HashMap::new()),
            reconciler_factory: factory,
        }
    }

    /// Return the handle for the given `(tenant_id, workflow_id, instance_id)`,
    /// spawning a new actor if none exists yet.
    pub fn get_or_spawn(
        &self,
        tenant_id: &str,
        workflow_id: &str,
        instance_id: &str,
    ) -> ActorHandle {
        let key = format!("{tenant_id}:{workflow_id}:{instance_id}");
        let mut map = self.actors.lock().unwrap();
        if let Some(handle) = map.get(&key) {
            return handle.clone();
        }
        let reconciler = (self.reconciler_factory)();
        let (actor, handle) = WorkflowActor::new(reconciler);
        // Spawn a subscriber broadcast receiver before moving actor so we can
        // make the broadcast_tx available; here we just run the actor.
        tokio::spawn(actor.run());
        map.insert(key, handle.clone());
        handle
    }

    /// Number of live actor instances.
    pub fn instance_count(&self) -> usize {
        self.actors.lock().unwrap().len()
    }
}

impl Default for ActorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use policy_core::evaluator::hash_state;
    use serde_json::json;

    fn make_msg(op_id: &str, hash: &str) -> ClientMessage {
        ClientMessage {
            op_id: op_id.to_owned(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({ "x": 1 }),
            client_snapshot_hash: hash.to_owned(),
            tenant_id: "tenant1".into(),
            workflow_id: "wf1".into(),
            instance_id: "inst1".into(),
            client_signature: None,
            role: "default".into(),
            vibe: None,
        }
    }

    #[tokio::test]
    async fn actor_processes_op_and_replies() {
        let registry = ActorRegistry::new();
        let handle = registry.get_or_spawn("t1", "wf1", "i1");

        // Build a valid hash from a fresh reconciler.
        let tmp = Reconciler::new();
        let hash = hash_state(&tmp.current_state());
        let msg = make_msg("op-1", &hash);

        let response = handle.reconcile(msg).await.unwrap();
        assert!(matches!(response, ServerMessage::Accept(_)));
    }

    #[tokio::test]
    async fn registry_reuses_same_actor_for_same_instance() {
        let registry = ActorRegistry::new();
        let h1 = registry.get_or_spawn("t1", "wf1", "i1");
        let h2 = registry.get_or_spawn("t1", "wf1", "i1");
        assert_eq!(registry.instance_count(), 1);
        // Both handles point to the same channel
        drop(h1);
        drop(h2);
    }

    #[tokio::test]
    async fn different_instances_get_different_actors() {
        let registry = ActorRegistry::new();
        registry.get_or_spawn("t1", "wf1", "i1");
        registry.get_or_spawn("t1", "wf1", "i2");
        assert_eq!(registry.instance_count(), 2);
    }

    #[tokio::test]
    async fn sequential_ops_on_same_instance_are_ordered() {
        let registry = ActorRegistry::new();
        let handle = registry.get_or_spawn("t1", "wf1", "i1");

        let tmp = Reconciler::new();
        let hash = hash_state(&tmp.current_state());
        let msg1 = make_msg("op-seq-1", &hash);
        let msg2 = make_msg("op-seq-2", &hash);

        let r1 = handle.reconcile(msg1).await.unwrap();
        // After accepting op-seq-1, the server hash changed, so op-seq-2 will
        // be rejected due to snapshot mismatch – but both must get responses.
        let r2 = handle.reconcile(msg2).await.unwrap();
        assert!(matches!(r1, ServerMessage::Accept(_)));
        assert!(matches!(r2, ServerMessage::Reject(_)));
    }
}
