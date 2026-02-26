//! ACCEPT / REJECT broadcast via tokio broadcast channels.
//!
//! The [`Broadcaster`] wraps a `tokio::sync::broadcast` sender.  Each
//! WebSocket handler subscribes to it and forwards messages to its client.
//!
//! This design is Redis-ready: swap the inner channel for a Redis Pub/Sub
//! publisher by replacing [`Broadcaster::publish`] and creating a Redis
//! subscriber task that feeds the broadcast channel from Redis messages.

use tokio::sync::broadcast;

use crate::wire::ServerMessage;

/// Capacity of the in-process broadcast ring buffer.
const CHANNEL_CAPACITY: usize = 256;

/// Server-wide ACCEPT/REJECT broadcaster.
///
/// Clone the [`Broadcaster`] to share it across Axum handler tasks.
#[derive(Clone)]
pub struct Broadcaster {
    tx: broadcast::Sender<ServerMessage>,
}

impl Broadcaster {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { tx }
    }

    /// Publish a [`ServerMessage`] to all active subscribers.
    ///
    /// Returns the number of subscribers that received the message.
    /// A return value of `0` means no WebSocket connections are currently open.
    pub fn publish(&self, msg: ServerMessage) -> usize {
        self.tx.send(msg).unwrap_or_default()
    }

    /// Subscribe to the broadcast channel.
    ///
    /// Each WebSocket handler should call this once and poll the returned
    /// [`broadcast::Receiver`] in a `tokio::select!` loop alongside the
    /// WebSocket stream.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerMessage> {
        self.tx.subscribe()
    }
}

impl Default for Broadcaster {
    fn default() -> Self {
        Self::new()
    }
}

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
    async fn subscriber_receives_published_message() {
        let broadcaster = Broadcaster::new();
        let mut rx = broadcaster.subscribe();
        broadcaster.publish(accept("op-1"));
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, ServerMessage::Accept(ref a) if a.op_id == "op-1"));
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let broadcaster = Broadcaster::new();
        let mut rx1 = broadcaster.subscribe();
        let mut rx2 = broadcaster.subscribe();
        broadcaster.publish(accept("op-2"));
        assert!(matches!(
            rx1.recv().await.unwrap(),
            ServerMessage::Accept(_)
        ));
        assert!(matches!(
            rx2.recv().await.unwrap(),
            ServerMessage::Accept(_)
        ));
    }

    #[test]
    fn publish_with_no_subscribers_returns_zero() {
        let broadcaster = Broadcaster::new();
        let n = broadcaster.publish(accept("op-3"));
        assert_eq!(n, 0);
    }
}
