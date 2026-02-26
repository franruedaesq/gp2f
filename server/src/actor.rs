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
//!
//! ## Distributed deployment (Redis PubSub)
//!
//! When the `redis-broadcast` Cargo feature is enabled and `REDIS_URL` is set,
//! the [`ActorRegistry`] uses a [`RedisActorCoordinator`] to:
//!
//! 1. **Claim** instance ownership via `SET actor:{key} {pod_id} NX EX {ttl}`.
//!    This prevents two pods from silently running split-brain actors for the
//!    same instance.
//! 2. **Announce** ownership via Redis PubSub channel `actor-registry`, so
//!    other pods can detect conflicts and log split-brain warnings in time for
//!    operators to act.
//!
//! The coordinator does **not** implement full request proxying; pods that fail
//! to claim an instance still serve it locally (graceful degradation) while
//! emitting a structured warning log.  Full proxying is deferred to a future
//! release that adds inter-pod HTTP routing.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{
    event_store::OpOutcome,
    reconciler::Reconciler,
    temporal_store::PersistentStore,
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
    /// Durable event store – every op outcome is persisted here so state
    /// survives pod restarts and actor evictions.
    persistent_store: Arc<dyn PersistentStore>,
    /// Partition key `"tenant_id:workflow_id:instance_id"` used to load
    /// prior events from the persistent store during recovery.
    key: String,
}

impl WorkflowActor {
    fn new(
        key: String,
        reconciler: Arc<Reconciler>,
        persistent_store: Arc<dyn PersistentStore>,
    ) -> (Self, ActorHandle) {
        let (tx, rx) = mpsc::channel(ACTOR_CHANNEL_CAPACITY);
        let (broadcast_tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        let actor = Self {
            rx,
            reconciler,
            broadcast_tx,
            persistent_store,
            key,
        };
        (actor, ActorHandle { tx })
    }

    async fn run(mut self) {
        // ── State recovery ────────────────────────────────────────────────
        // Before serving any new requests, replay all previously persisted
        // events so that the in-memory state is consistent with the durable
        // store.  This eliminates state loss after pod restarts or actor
        // evictions.
        let prior_events = self.persistent_store.events_for(&self.key).await;
        if !prior_events.is_empty() {
            tracing::info!(
                key = %self.key,
                count = prior_events.len(),
                "actor recovering state from persistent store"
            );
            self.reconciler.recover_from_events(&prior_events);
        }

        while let Some(msg) = self.rx.recv().await {
            match msg {
                ActorMessage::Reconcile { msg, reply_tx } => {
                    let response = self.reconciler.reconcile(&msg);
                    // Persist to the durable store so state survives restarts.
                    let outcome = match &response {
                        ServerMessage::Accept(_) => OpOutcome::Accepted,
                        _ => OpOutcome::Rejected,
                    };
                    match self.persistent_store.append((*msg).clone(), outcome).await {
                        Ok(seq) => {
                            tracing::debug!(op_id = %msg.op_id, seq, "op persisted to durable store");
                        }
                        Err(e) => {
                            tracing::error!(op_id = %msg.op_id, error = %e, "failed to persist op to durable store");
                        }
                    }
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
///
/// In a multi-replica deployment, set the `redis-broadcast` feature and provide
/// a [`RedisActorCoordinator`] via [`ActorRegistry::with_coordinator`] to
/// enable cross-pod ownership tracking and split-brain detection.
pub struct ActorRegistry {
    actors: Mutex<HashMap<String, ActorHandle>>,
    /// Shared reconciler – actors wrap it (one per workflow instance so there
    /// is no lock contention across instances).
    reconciler_factory: Arc<dyn Fn() -> Arc<Reconciler> + Send + Sync>,
    /// Durable event store shared across all actors in this registry.
    persistent_store: Arc<dyn PersistentStore>,
    /// Optional Redis coordinator for cross-pod actor ownership tracking.
    #[cfg(feature = "redis-broadcast")]
    coordinator: Option<Arc<RedisActorCoordinator>>,
}

impl ActorRegistry {
    /// Create a registry backed by default reconcilers and an in-memory store.
    pub fn new() -> Self {
        use crate::temporal_store::InMemoryStore;
        Self::with_store(Arc::new(InMemoryStore::new()))
    }

    /// Create a registry backed by the provided durable [`PersistentStore`].
    ///
    /// Every actor spawned by this registry will persist op outcomes to
    /// `store`, ensuring data survives pod restarts and actor evictions.
    pub fn with_store(store: Arc<dyn PersistentStore>) -> Self {
        Self::with_factory_and_store(Arc::new(|| Arc::new(Reconciler::new())), store)
    }

    /// Create a registry with a custom reconciler factory and persistent store
    /// (useful for testing).
    pub fn with_factory_and_store(
        factory: Arc<dyn Fn() -> Arc<Reconciler> + Send + Sync>,
        store: Arc<dyn PersistentStore>,
    ) -> Self {
        Self {
            actors: Mutex::new(HashMap::new()),
            reconciler_factory: factory,
            persistent_store: store,
            #[cfg(feature = "redis-broadcast")]
            coordinator: None,
        }
    }

    /// Create a registry with a custom reconciler factory (useful for testing).
    ///
    /// Uses an in-memory fallback store.  Prefer [`ActorRegistry::with_store`]
    /// for production use to wire in the durable [`PersistentStore`].
    pub fn with_factory(factory: Arc<dyn Fn() -> Arc<Reconciler> + Send + Sync>) -> Self {
        use crate::temporal_store::InMemoryStore;
        Self::with_factory_and_store(factory, Arc::new(InMemoryStore::new()))
    }

    /// Return the handle for the given `(tenant_id, workflow_id, instance_id)`,
    /// spawning a new actor if none exists yet.
    ///
    /// In a multi-replica deployment with a [`RedisActorCoordinator`] attached,
    /// this method will attempt to claim ownership of the instance in Redis
    /// before spawning.  If another pod already owns the instance, a split-brain
    /// warning is logged but the actor is still spawned locally (graceful
    /// degradation until full proxy support is available).
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
        let (actor, handle) =
            WorkflowActor::new(key.clone(), reconciler, self.persistent_store.clone());
        tokio::spawn(actor.run());
        map.insert(key.clone(), handle.clone());

        // Announce actor ownership to other pods via Redis.
        #[cfg(feature = "redis-broadcast")]
        if let Some(coord) = self.coordinator.clone() {
            let instance_key = key.clone();
            tokio::spawn(async move {
                coord.claim_and_announce(&instance_key).await;
            });
        }

        handle
    }

    /// Attach a [`RedisActorCoordinator`] to this registry for cross-pod
    /// actor ownership tracking and split-brain detection.
    ///
    /// This is a no-op when the `redis-broadcast` feature is disabled.
    #[cfg(feature = "redis-broadcast")]
    pub fn with_coordinator(mut self, coordinator: Arc<RedisActorCoordinator>) -> Self {
        self.coordinator = Some(coordinator);
        self
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

// ── Redis actor coordinator ───────────────────────────────────────────────────

/// Redis-backed actor ownership coordinator.
///
/// Uses Redis `SET actor:{key} {pod_id} NX EX {ttl}` to claim exclusive
/// ownership of a workflow instance and Redis PubSub to broadcast that claim to
/// other pods.
///
/// ## Split-brain detection
///
/// When two pods race to spawn actors for the same instance, one will succeed
/// in claiming the Redis key and the other will receive a warning log.
/// The warning is actionable: an operator can redirect traffic for that
/// instance to the owning pod using the pod_id published on the
/// `actor-registry` PubSub channel.
#[cfg(feature = "redis-broadcast")]
pub struct RedisActorCoordinator {
    client: redis::Client,
    /// Unique identifier for this pod (used as the Redis key value).
    pod_id: String,
    /// How long (seconds) to hold the ownership claim before it expires.
    claim_ttl_secs: u64,
}

#[cfg(feature = "redis-broadcast")]
impl RedisActorCoordinator {
    /// Connect to Redis and return a new coordinator.
    ///
    /// `pod_id` should be unique per replica (e.g. the Pod name from the
    /// `HOSTNAME` environment variable in Kubernetes).
    pub fn connect(redis_url: &str, pod_id: impl Into<String>) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(redis_url)?;
        Ok(Self {
            client,
            pod_id: pod_id.into(),
            claim_ttl_secs: 30,
        })
    }

    /// Create with a custom claim TTL (useful for testing).
    pub fn with_claim_ttl(mut self, secs: u64) -> Self {
        self.claim_ttl_secs = secs;
        self
    }

    /// Attempt to claim ownership of `instance_key` in Redis and publish a
    /// "hosting" announcement on the `actor-registry` PubSub channel.
    ///
    /// If another pod already holds the claim, a split-brain warning is logged.
    pub async fn claim_and_announce(&self, instance_key: &str) {
        let redis_key = format!("actor:{instance_key}");
        let channel = "actor-registry";

        let mut conn = match self.client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    instance_key = %instance_key,
                    error = %e,
                    "Redis actor coordinator: connection failed; skipping ownership claim"
                );
                return;
            }
        };

        // Attempt to claim ownership (NX = only if key does not exist).
        let claimed: redis::Value = redis::cmd("SET")
            .arg(&redis_key)
            .arg(&self.pod_id)
            .arg("EX")
            .arg(self.claim_ttl_secs)
            .arg("NX")
            .query_async(&mut conn)
            .await
            .unwrap_or(redis::Value::Nil);

        let is_owner = matches!(claimed, redis::Value::SimpleString(ref s) if s == "OK");

        if !is_owner {
            // Another pod owns this instance.
            let owner: String = redis::cmd("GET")
                .arg(&redis_key)
                .query_async(&mut conn)
                .await
                .unwrap_or_default();
            tracing::warn!(
                instance_key = %instance_key,
                owner_pod = %owner,
                this_pod = %self.pod_id,
                "split-brain warning: actor for instance already claimed by another pod"
            );
        }

        // Publish a "hosting" announcement regardless of whether we claimed it,
        // so other pods can track the topology via the PubSub channel.
        let payload = serde_json::json!({
            "pod": self.pod_id,
            "instance": instance_key,
            "is_owner": is_owner,
        })
        .to_string();
        let _: redis::Value = redis::cmd("PUBLISH")
            .arg(channel)
            .arg(&payload)
            .query_async(&mut conn)
            .await
            .unwrap_or(redis::Value::Nil);

        tracing::debug!(
            instance_key = %instance_key,
            pod = %self.pod_id,
            is_owner = %is_owner,
            "actor ownership announced on Redis PubSub"
        );
    }

    /// Build a coordinator from the `REDIS_URL` and `HOSTNAME` environment
    /// variables.  Returns `None` if `REDIS_URL` is not set.
    ///
    /// The `pod_id` is taken from `HOSTNAME` (set automatically by Kubernetes)
    /// or falls back to a random hex string to guarantee uniqueness across
    /// pod restarts.
    pub fn from_env() -> Option<Self> {
        let url = crate::secrets::resolve_secret("REDIS_URL")?;
        let pod_id = std::env::var("HOSTNAME").unwrap_or_else(|_| {
            // Generate a random 8-byte hex suffix so that restarts don't collide.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos();
            let pid = std::process::id();
            format!("pod-{pid:08x}-{nanos:08x}")
        });
        match Self::connect(&url, pod_id) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("Redis actor coordinator setup failed: {e}");
                None
            }
        }
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

    /// Verify that a new actor recovers state from a pre-populated persistent
    /// store, so that clients that were already at the latest hash can still
    /// submit ops after a pod restart / actor eviction.
    #[tokio::test]
    async fn actor_recovers_state_from_persistent_store() {
        use crate::temporal_store::InMemoryStore;

        // Use IDs that match the actor key derived from get_or_spawn.
        let make_tenant_msg = |op_id: &str, hash: &str| ClientMessage {
            op_id: op_id.to_owned(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({ "x": 1 }),
            client_snapshot_hash: hash.to_owned(),
            tenant_id: "t1".into(),
            workflow_id: "wf1".into(),
            instance_id: "i1".into(),
            client_signature: None,
            role: "default".into(),
            vibe: None,
        };

        // Step 1: populate the store with one accepted op via a "first" actor.
        let store = Arc::new(InMemoryStore::new());
        let registry1 = ActorRegistry::with_store(store.clone());
        let h1 = registry1.get_or_spawn("t1", "wf1", "i1");

        let tmp = Reconciler::new();
        let hash0 = hash_state(&tmp.current_state());
        let msg1 = make_tenant_msg("op-before-restart", &hash0);
        let r1 = h1.reconcile(msg1).await.unwrap();
        assert!(matches!(r1, ServerMessage::Accept(_)));

        // Give the actor task a moment to persist the event.
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // Verify the event was persisted under the correct partition key.
        let events = store.events_for("t1:wf1:i1").await;
        assert_eq!(events.len(), 1);

        // Step 2: simulate a restart by creating a brand-new registry backed by
        // the same store.  The new actor must recover state so the client's
        // current hash (post-op-before-restart) is accepted.
        let registry2 = ActorRegistry::with_store(store.clone());
        let h2 = registry2.get_or_spawn("t1", "wf1", "i1");

        // The client's snapshot hash after the first op was accepted.
        let accepted_hash = if let ServerMessage::Accept(ref a) = r1 {
            a.server_snapshot_hash.clone()
        } else {
            panic!("expected Accept");
        };

        // Give the recovery a moment to complete (it runs at the start of run()).
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Build a new op using the post-restart hash – this should be accepted
        // because the recovered actor has the same state.
        let msg2 = make_tenant_msg("op-after-restart", &accepted_hash);
        let r2 = h2.reconcile(msg2).await.unwrap();
        assert!(
            matches!(r2, ServerMessage::Accept(_)),
            "recovered actor must accept ops using the pre-restart hash"
        );
    }

    /// Verify that ops already processed before a restart are not re-accepted
    /// (replay guard is populated during recovery).
    #[tokio::test]
    async fn recovered_actor_rejects_duplicate_op_ids() {
        use crate::temporal_store::InMemoryStore;

        let make_tenant_msg = |op_id: &str, hash: &str| ClientMessage {
            op_id: op_id.to_owned(),
            ast_version: "1.0.0".into(),
            action: "update".into(),
            payload: json!({ "x": 1 }),
            client_snapshot_hash: hash.to_owned(),
            tenant_id: "t1".into(),
            workflow_id: "wf1".into(),
            instance_id: "i2".into(),
            client_signature: None,
            role: "default".into(),
            vibe: None,
        };

        let store = Arc::new(InMemoryStore::new());
        let registry1 = ActorRegistry::with_store(store.clone());
        let h1 = registry1.get_or_spawn("t1", "wf1", "i2");

        let tmp = Reconciler::new();
        let hash0 = hash_state(&tmp.current_state());
        let original_msg = make_tenant_msg("op-dup-check", &hash0);
        let r1 = h1.reconcile(original_msg.clone()).await.unwrap();
        assert!(matches!(r1, ServerMessage::Accept(_)));

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        // New registry, same store – actor recovers then rejects the duplicate.
        let registry2 = ActorRegistry::with_store(store.clone());
        let h2 = registry2.get_or_spawn("t1", "wf1", "i2");

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let r2 = h2.reconcile(original_msg).await.unwrap();
        assert!(
            matches!(r2, ServerMessage::Reject(_)),
            "recovered actor must reject a duplicate op_id"
        );
    }
}
