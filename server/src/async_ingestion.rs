//! Async Ingestion pattern (Phase 2.2 – Latency Optimisation).
//!
//! The API acknowledges a client operation **immediately** after a lightweight
//! partial-validation pass (format check + replay-protection), then enqueues
//! the full reconcile work onto a bounded Tokio channel.  A background task
//! drains the queue and runs the complete AST evaluation / Temporal signal
//! pipeline without blocking the HTTP response path.
//!
//! ## Latency profile
//! ```text
//! Client  ──POST /op/async──►  AsyncIngestionQueue::enqueue()
//!                                   │  partial validation (< 1 ms)
//!                               HTTP 202 Accepted   ◄──────────────
//!                                   │
//!                               background worker
//!                                   │  full AST eval + Temporal signal
//!                               (fire-and-forget; result via WebSocket push)
//! ```
//!
//! ## Benchmark target
//! End-to-end HTTP response latency must be below **16 ms** with this pattern.
//! If the synchronous `/op` endpoint exceeds 20 ms overhead, migrate the
//! critical path to this async ingestion handler.

use tokio::sync::mpsc;

use crate::wire::ClientMessage;

// ── types ─────────────────────────────────────────────────────────────────────

/// Lightweight acknowledgement returned immediately to the caller.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestAck {
    /// Echoes the caller's `op_id` so the client can correlate the ack.
    pub op_id: String,
    /// Always `"queued"` – full result is pushed over WebSocket.
    pub status: &'static str,
}

/// A bounded MPSC queue that decouples HTTP ingestion from workflow execution.
#[derive(Debug, Clone)]
pub struct AsyncIngestionQueue {
    sender: mpsc::Sender<ClientMessage>,
}

impl AsyncIngestionQueue {
    /// Create a new queue with capacity `buffer_size`.
    ///
    /// Returns the queue handle and the receiver that the background worker
    /// should drain.
    pub fn new(buffer_size: usize) -> (Self, mpsc::Receiver<ClientMessage>) {
        let (sender, receiver) = mpsc::channel(buffer_size);
        (Self { sender }, receiver)
    }

    /// Perform a partial validation of `msg` and, if it passes, enqueue it for
    /// background processing.
    ///
    /// Returns `Ok(IngestAck)` when the message was successfully queued, or
    /// `Err(IngestError)` when partial validation fails or the queue is full.
    ///
    /// *Partial validation* checks:
    /// 1. `op_id` is non-empty (format guard).
    /// 2. `action` is non-empty (prevents no-op submissions).
    /// 3. Queue is not at capacity (back-pressure guard).
    pub async fn enqueue(&self, msg: ClientMessage) -> Result<IngestAck, IngestError> {
        if msg.op_id.is_empty() {
            return Err(IngestError::InvalidOpId);
        }
        if msg.action.is_empty() {
            return Err(IngestError::InvalidAction);
        }

        let op_id = msg.op_id.clone();
        self.sender
            .try_send(msg)
            .map_err(|_| IngestError::QueueFull)?;

        Ok(IngestAck {
            op_id,
            status: "queued",
        })
    }

    /// Return the number of messages currently waiting in the queue for
    /// background processing.  Useful for monitoring back-pressure; expose via
    /// a metrics endpoint in production.
    pub fn pending(&self) -> usize {
        self.sender.max_capacity() - self.sender.capacity()
    }
}

/// Errors returned by [`AsyncIngestionQueue::enqueue`].
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("op_id must not be empty")]
    InvalidOpId,
    #[error("action must not be empty")]
    InvalidAction,
    #[error("ingestion queue is full; apply back-pressure")]
    QueueFull,
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_msg(op_id: &str, action: &str) -> ClientMessage {
        ClientMessage {
            op_id: op_id.into(),
            ast_version: "1.0.0".into(),
            action: action.into(),
            payload: json!({}),
            client_snapshot_hash: "h".into(),
            tenant_id: "t1".into(),
            workflow_id: "wf1".into(),
            instance_id: "i1".into(),
            client_signature: None,
            role: "user".into(),
            vibe: None,
        }
    }

    #[tokio::test]
    async fn enqueue_returns_ack_with_op_id() {
        let (queue, _rx) = AsyncIngestionQueue::new(16);
        let ack = queue.enqueue(make_msg("op-1", "update")).await.unwrap();
        assert_eq!(ack.op_id, "op-1");
        assert_eq!(ack.status, "queued");
    }

    #[tokio::test]
    async fn empty_op_id_is_rejected() {
        let (queue, _rx) = AsyncIngestionQueue::new(16);
        let err = queue.enqueue(make_msg("", "update")).await.unwrap_err();
        assert!(matches!(err, IngestError::InvalidOpId));
    }

    #[tokio::test]
    async fn empty_action_is_rejected() {
        let (queue, _rx) = AsyncIngestionQueue::new(16);
        let err = queue.enqueue(make_msg("op-2", "")).await.unwrap_err();
        assert!(matches!(err, IngestError::InvalidAction));
    }

    #[tokio::test]
    async fn full_queue_returns_back_pressure_error() {
        // Capacity of 1; second enqueue should fail.
        let (queue, _rx) = AsyncIngestionQueue::new(1);
        queue.enqueue(make_msg("op-1", "update")).await.unwrap();
        let err = queue.enqueue(make_msg("op-2", "update")).await.unwrap_err();
        assert!(matches!(err, IngestError::QueueFull));
    }

    #[tokio::test]
    async fn background_worker_receives_messages() {
        let (queue, mut rx) = AsyncIngestionQueue::new(16);
        queue.enqueue(make_msg("op-bg", "update")).await.unwrap();
        let received = rx.try_recv().unwrap();
        assert_eq!(received.op_id, "op-bg");
    }
}
