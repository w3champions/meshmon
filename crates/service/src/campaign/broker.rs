//! In-process broker that fans campaign lifecycle events out to SSE
//! subscribers.
//!
//! Mirrors the catalogue broker at [`crate::catalogue::events::CatalogueBroker`],
//! but the publisher is *not* the request path. The scheduler flips
//! `running → completed` autonomously, and the writer fires
//! `campaign_pair_settled` as it settles agent-reported results — neither
//! runs inside an operator HTTP call. A dedicated [`PgListener`]
//! ([`super::listener::spawn_campaign_listener`]) tails the two existing
//! NOTIFY channels and calls [`CampaignBroker::publish`] on every wake-up.
//!
//! [`PgListener`]: sqlx::postgres::PgListener
//!
//! Capacity 512 matches the catalogue broker; a settling campaign fans
//! out one `pair_settled` per pair, and 512 comfortably fits the largest
//! batch the scheduler will claim per tick.
//!
//! The broker is cheap to clone (`Arc<Sender>`) and is stored on
//! [`crate::state::AppState`] so every handler (and the listener task)
//! shares the same channel. The SSE handler in [`super::sse`] subscribes
//! per-connection and drops its receiver when the client disconnects.

use super::model::CampaignState;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;
use utoipa::ToSchema;
use uuid::Uuid;

/// Default broadcast capacity for the per-service campaign broker.
/// Sized for scheduler-driven burst settles without blocking publishers.
pub const DEFAULT_CAPACITY: usize = 512;

/// Campaign lifecycle event delivered to every SSE subscriber.
///
/// The `tag = "kind"` serde representation matches the wire shape the
/// frontend expects: one top-level `kind` discriminant plus flat
/// per-variant fields. Keep variant names in `snake_case` on the wire.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CampaignStreamEvent {
    /// A campaign transitioned to a new lifecycle state. Emitted by the
    /// [`super::listener`] task after parsing a `campaign_state_changed`
    /// NOTIFY and resolving the current `state` column.
    StateChanged {
        /// Primary key of the campaign whose state changed.
        campaign_id: Uuid,
        /// New lifecycle state.
        state: CampaignState,
    },
    /// A pair belonging to this campaign reached a terminal resolution.
    /// Emitted by the [`super::listener`] task on every
    /// `campaign_pair_settled` NOTIFY. The payload intentionally omits
    /// the pair id — subscribers re-fetch the affected pairs through
    /// the existing `/api/campaigns/{id}/pairs` endpoint rather than
    /// attempting to reconstruct state from the SSE stream alone.
    PairSettled {
        /// Primary key of the campaign owning the settled pair.
        campaign_id: Uuid,
    },
    /// The campaign's `campaign_evaluations` row was rewritten. Emitted
    /// by the `POST /api/campaigns/:id/evaluate` handler after the
    /// evaluation is persisted. Distinct from `StateChanged` because
    /// re-evaluating an already-`Evaluated` campaign does not transition
    /// state, yet must still invalidate the frontend's evaluation query
    /// cache.
    Evaluated {
        /// Primary key of the campaign whose evaluation was rewritten.
        campaign_id: Uuid,
    },
}

/// In-process fan-out broker for [`CampaignStreamEvent`]s.
///
/// Wraps a `tokio::sync::broadcast::Sender` in an `Arc` so clones share
/// the same channel. Cheap to clone.
#[derive(Clone)]
pub struct CampaignBroker {
    tx: Arc<broadcast::Sender<CampaignStreamEvent>>,
}

impl CampaignBroker {
    /// Construct a broker with the given broadcast capacity. Capacity
    /// bounds how far a subscriber may lag before its receiver returns
    /// `Lagged`; the publisher never blocks regardless of capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx: Arc::new(tx) }
    }

    /// Subscribe to the event stream. Each call returns an independent
    /// receiver; only events published *after* subscription are observed.
    pub fn subscribe(&self) -> broadcast::Receiver<CampaignStreamEvent> {
        self.tx.subscribe()
    }

    /// Publish an event. Succeeds even with zero subscribers; the
    /// resulting `SendError` is intentionally ignored so publishers can
    /// fire-and-forget regardless of whether any SSE client is attached.
    pub fn publish(&self, ev: CampaignStreamEvent) {
        let _ = self.tx.send(ev);
    }
}

impl Default for CampaignBroker {
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
        let broker = CampaignBroker::new(16);
        let mut rx = broker.subscribe();
        let id = Uuid::new_v4();
        broker.publish(CampaignStreamEvent::StateChanged {
            campaign_id: id,
            state: CampaignState::Running,
        });
        let ev = rx.recv().await.unwrap();
        match ev {
            CampaignStreamEvent::StateChanged { campaign_id, state } => {
                assert_eq!(campaign_id, id);
                assert_eq!(state, CampaignState::Running);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn lagging_subscriber_does_not_block_publisher() {
        let broker = CampaignBroker::new(2);
        let mut rx = broker.subscribe();
        // Overflow the capacity many-fold. Every `publish` must return
        // immediately even though nobody is draining `rx`.
        for _ in 0..10 {
            broker.publish(CampaignStreamEvent::PairSettled {
                campaign_id: Uuid::nil(),
            });
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
        let broker = CampaignBroker::new(4);
        // No `.subscribe()` call — ensure publish doesn't panic or error.
        broker.publish(CampaignStreamEvent::PairSettled {
            campaign_id: Uuid::nil(),
        });
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive_events() {
        let broker = CampaignBroker::new(8);
        let mut rx1 = broker.subscribe();
        let mut rx2 = broker.subscribe();
        let id = Uuid::new_v4();
        broker.publish(CampaignStreamEvent::StateChanged {
            campaign_id: id,
            state: CampaignState::Completed,
        });
        let ev1 = rx1.recv().await.unwrap();
        let ev2 = rx2.recv().await.unwrap();
        for ev in [ev1, ev2] {
            match ev {
                CampaignStreamEvent::StateChanged { campaign_id, state } => {
                    assert_eq!(campaign_id, id);
                    assert_eq!(state, CampaignState::Completed);
                }
                other => panic!("wrong variant: {other:?}"),
            }
        }
    }

    #[test]
    fn event_kind_serializes_in_snake_case() {
        let id = Uuid::new_v4();
        let changed = CampaignStreamEvent::StateChanged {
            campaign_id: id,
            state: CampaignState::Running,
        };
        let json = serde_json::to_value(&changed).expect("serialize state_changed");
        assert_eq!(json["kind"], "state_changed");
        assert_eq!(json["state"], "running");
        assert_eq!(json["campaign_id"], id.to_string());

        let settled = CampaignStreamEvent::PairSettled { campaign_id: id };
        let json = serde_json::to_value(&settled).expect("serialize pair_settled");
        assert_eq!(json["kind"], "pair_settled");
        assert_eq!(json["campaign_id"], id.to_string());
    }

    #[test]
    fn evaluated_event_wire_name() {
        let ev = CampaignStreamEvent::Evaluated {
            campaign_id: Uuid::nil(),
        };
        let j = serde_json::to_value(&ev).unwrap();
        assert_eq!(j["kind"], "evaluated");
    }
}
