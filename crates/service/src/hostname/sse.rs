//! Per-session SSE broadcaster for `hostname_resolved` events.
//!
//! Every session that asks the service to resolve one or more IPs receives
//! only the events that resulted from its own requests. Unlike the
//! `catalogue::events::CatalogueBroker` — which broadcasts to every
//! subscriber — this broker routes events through a [`DashMap`] keyed by
//! [`SessionId`] so fan-out is O(recipients) rather than O(subscribers).
use dashmap::DashMap;
use serde::Serialize;
use std::{net::IpAddr, sync::Arc};
use tokio::sync::mpsc;
use utoipa::ToSchema;

/// Opaque session identifier. In production this comes from
/// `tower_sessions::Session::id()` (a ULID); tests construct
/// it from any stable string.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SessionId(pub String);

impl SessionId {
    /// Wrap any stable string identifier as a session key.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// Wire payload for the `hostname_resolved` SSE event.
///
/// `IpAddr` has no built-in `ToSchema` impl under utoipa 5, so we annotate
/// the field with `schema(value_type = String)`. Serde still uses the
/// default `IpAddr` display serializer, which matches the shape the
/// frontend expects (e.g. `"192.0.2.1"` / `"2001:db8::1"`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HostnameEvent {
    /// Canonicalized IP the lookup applies to.
    #[schema(value_type = String)]
    pub ip: IpAddr,
    /// Resolved hostname, or `None` when the lookup confirmed no PTR
    /// record exists.
    pub hostname: Option<String>,
}

/// Session-scoped broker for `hostname_resolved` SSE events.
#[derive(Clone, Default)]
pub struct HostnameBroadcaster {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    sessions: DashMap<SessionId, mpsc::Sender<HostnameEvent>>,
}

impl HostnameBroadcaster {
    /// Construct an empty broker with no registered sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a session. Returns a bounded receiver + a handle
    /// whose Drop removes the channel from the registry.
    pub fn register(
        &self,
        session: SessionId,
        capacity: usize,
    ) -> (SessionHandle, mpsc::Receiver<HostnameEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        self.inner.sessions.insert(session.clone(), tx);
        let handle = SessionHandle {
            session,
            inner: self.inner.clone(),
        };
        (handle, rx)
    }

    /// Deliver `event` to every session in `recipients`. Sessions
    /// whose channels are closed or full get the event silently
    /// dropped (best-effort; the browser's EventSource reconnect
    /// and TanStack-Query refetch-on-reconnect are the recovery
    /// path).
    pub fn fanout(&self, recipients: &[SessionId], event: HostnameEvent) {
        for session in recipients {
            if let Some(sender) = self.inner.sessions.get(session) {
                let _ = sender.try_send(event.clone());
            }
        }
    }

    /// Number of sessions currently registered.
    pub fn session_count(&self) -> usize {
        self.inner.sessions.len()
    }
}

/// RAII handle returned by [`HostnameBroadcaster::register`]. Dropping it
/// removes the session's channel from the registry.
pub struct SessionHandle {
    session: SessionId,
    inner: Arc<Inner>,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        self.inner.sessions.remove(&self.session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use tokio::time::timeout;

    fn sample_event() -> HostnameEvent {
        HostnameEvent {
            ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            hostname: Some("host.example.com".to_string()),
        }
    }

    #[tokio::test]
    async fn fanout_delivers_only_to_listed_sessions() {
        let broker = HostnameBroadcaster::new();
        let session_a = SessionId::new("a");
        let session_b = SessionId::new("b");
        let (_handle_a, mut rx_a) = broker.register(session_a.clone(), 8);
        let (_handle_b, mut rx_b) = broker.register(session_b.clone(), 8);

        let event = sample_event();
        broker.fanout(std::slice::from_ref(&session_a), event.clone());

        let got = timeout(Duration::from_millis(50), rx_a.recv())
            .await
            .expect("session A should receive event")
            .expect("channel still open");
        assert_eq!(got.ip, event.ip);
        assert_eq!(got.hostname, event.hostname);

        assert!(
            timeout(Duration::from_millis(50), rx_b.recv())
                .await
                .is_err(),
            "session B must not receive events addressed to A",
        );
    }

    #[tokio::test]
    async fn drop_removes_session() {
        let broker = HostnameBroadcaster::new();
        let session = SessionId::new("a");
        let (handle, _rx) = broker.register(session.clone(), 4);
        assert_eq!(broker.session_count(), 1);

        drop(handle);
        assert_eq!(broker.session_count(), 0);

        // Must not panic — just a no-op fanout.
        broker.fanout(&[session], sample_event());
    }

    #[tokio::test]
    async fn full_channel_drops_silently() {
        let broker = HostnameBroadcaster::new();
        let session = SessionId::new("a");
        let (_handle, mut rx) = broker.register(session.clone(), 1);

        for _ in 0..5 {
            broker.fanout(std::slice::from_ref(&session), sample_event());
        }

        // Only the first event fits; the remaining four are dropped.
        let first = timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("first event should land")
            .expect("channel still open");
        assert_eq!(first.ip, sample_event().ip);

        assert!(
            timeout(Duration::from_millis(50), rx.recv()).await.is_err(),
            "subsequent events should be silently dropped when the channel is full",
        );
    }
}
