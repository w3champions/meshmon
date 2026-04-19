//! In-process broker that fans catalogue lifecycle events out to SSE
//! subscribers.
//!
//! Every mutating catalogue operation (create, update, delete, enrichment
//! progress) publishes exactly one [`CatalogueEvent`] on an owned
//! [`tokio::sync::broadcast`] channel. Subscribers that fall behind the
//! bounded capacity observe a `Lagged` receive error — intentional: the
//! publisher must never block on a slow SSE client.
//!
//! Capacity 512 is chosen so a burst of paste-flow inserts (operator
//! pastes a block of IPs that each fan out create + enrichment-progress
//! events) comfortably fits without dropping, while still bounding
//! memory.
//!
//! The broker is cheap to clone (`Arc<Sender>`) and is stored on
//! [`crate::state::AppState`] so every handler shares the same channel.
//! The SSE handler in [`super::sse`] subscribes per-connection and
//! drops its receiver when the client disconnects.

use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use utoipa::ToSchema;
use uuid::Uuid;

/// Default broadcast capacity for the per-service catalogue broker.
/// Sized for typical paste-flow bursts without blocking publishers.
pub const DEFAULT_CAPACITY: usize = 512;

/// Catalogue lifecycle event delivered to every SSE subscriber.
///
/// The `tag = "kind"` serde representation matches the wire shape the
/// frontend expects: one top-level `kind` discriminant plus flat
/// per-variant fields. Keep variant names in `snake_case` on the wire —
/// `utoipa` and serde both honour `rename_all = "snake_case"`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogueEvent {
    /// A new catalogue row was inserted.
    Created {
        /// Primary key of the newly-inserted row.
        id: Uuid,
        /// Textual rendering of the catalogued IP for convenient client-side
        /// display without a second fetch.
        ip: String,
    },
    /// An existing row's columns changed (operator edit or enrichment write).
    Updated {
        /// Primary key of the updated row.
        id: Uuid,
    },
    /// A row was deleted.
    Deleted {
        /// Primary key of the row that was removed.
        id: Uuid,
    },
    /// Enrichment pipeline advanced for a row.
    EnrichmentProgress {
        /// Primary key of the row whose enrichment status changed.
        id: Uuid,
        /// New enrichment status.
        status: super::model::EnrichmentStatus,
    },
}

/// In-process fan-out broker for [`CatalogueEvent`]s.
///
/// Wraps a `tokio::sync::broadcast::Sender` in an `Arc` so clones share
/// the same channel. Cheap to clone.
#[derive(Clone)]
pub struct CatalogueBroker {
    tx: Arc<broadcast::Sender<CatalogueEvent>>,
}

impl CatalogueBroker {
    /// Construct a broker with the given broadcast capacity. Capacity
    /// bounds how far a subscriber may lag before its receiver returns
    /// `Lagged`; the publisher never blocks regardless of capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx: Arc::new(tx) }
    }

    /// Subscribe to the event stream. Each call returns an independent
    /// receiver; only events published *after* subscription are observed.
    pub fn subscribe(&self) -> broadcast::Receiver<CatalogueEvent> {
        self.tx.subscribe()
    }

    /// Publish an event. Succeeds even with zero subscribers; the
    /// resulting `SendError` is intentionally ignored so publishers can
    /// fire-and-forget regardless of whether any SSE client is attached.
    pub fn publish(&self, ev: CatalogueEvent) {
        let _ = self.tx.send(ev);
    }
}

impl Default for CatalogueBroker {
    /// Construct a broker with [`DEFAULT_CAPACITY`].
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broker_delivers_events_to_subscribers() {
        let broker = CatalogueBroker::new(16);
        let mut rx = broker.subscribe();
        broker.publish(CatalogueEvent::Updated { id: Uuid::nil() });
        let ev = rx.recv().await.unwrap();
        match ev {
            CatalogueEvent::Updated { id } => assert_eq!(id, Uuid::nil()),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn lagging_subscriber_does_not_block_publisher() {
        let broker = CatalogueBroker::new(2);
        let mut rx = broker.subscribe();
        // Overflow the capacity many-fold. Every `publish` must return
        // immediately even though nobody is draining `rx`.
        for _ in 0..10 {
            broker.publish(CatalogueEvent::Updated { id: Uuid::nil() });
        }
        // The first recv surfaces a Lagged error per broadcast semantics.
        let err = rx.recv().await.unwrap_err();
        assert!(matches!(
            err,
            tokio::sync::broadcast::error::RecvError::Lagged(_)
        ));
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_infallible() {
        let broker = CatalogueBroker::new(4);
        // No `.subscribe()` call — ensure publish doesn't panic or error.
        broker.publish(CatalogueEvent::Deleted { id: Uuid::nil() });
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive_events() {
        let broker = CatalogueBroker::new(8);
        let mut rx1 = broker.subscribe();
        let mut rx2 = broker.subscribe();
        let id = Uuid::new_v4();
        broker.publish(CatalogueEvent::Created {
            id,
            ip: "10.0.0.1".to_string(),
        });
        let ev1 = rx1.recv().await.unwrap();
        let ev2 = rx2.recv().await.unwrap();
        for ev in [ev1, ev2] {
            match ev {
                CatalogueEvent::Created { id: got, ip } => {
                    assert_eq!(got, id);
                    assert_eq!(ip, "10.0.0.1");
                }
                other => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn event_kind_serializes_in_snake_case() {
        let ev = CatalogueEvent::EnrichmentProgress {
            id: Uuid::nil(),
            status: super::super::model::EnrichmentStatus::Enriched,
        };
        let json = serde_json::to_value(&ev).expect("serialize");
        assert_eq!(json["kind"], "enrichment_progress");
        assert_eq!(json["status"], "enriched");
    }
}
