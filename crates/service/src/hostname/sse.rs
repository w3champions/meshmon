//! Per-session SSE broadcaster for `hostname_resolved` events.
//!
//! Every session that asks the service to resolve one or more IPs receives
//! only the events that resulted from its own requests. Unlike the
//! `catalogue::events::CatalogueBroker` — which broadcasts to every
//! subscriber — this broker routes events through a [`DashMap`] keyed by
//! [`SessionId`] so fan-out is O(recipients) rather than O(subscribers).
//!
//! A single session id may carry more than one concurrent subscription —
//! a user can open two tabs, and `EventSource` can race a reconnect with
//! the old connection's teardown. Each [`SessionHandle`] therefore carries
//! a unique token; the map value is `Vec<Subscription>`, and `Drop`
//! removes only the subscription that owns the token. Fanout delivers to
//! every live subscription under the session.
use dashmap::DashMap;
use serde::Serialize;
use std::{
    net::IpAddr,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
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
    sessions: DashMap<SessionId, Vec<Subscription>>,
    next_token: AtomicU64,
}

struct Subscription {
    token: u64,
    tx: mpsc::Sender<HostnameEvent>,
}

impl HostnameBroadcaster {
    /// Construct an empty broker with no registered sessions.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a subscription for `session`. Multiple subscriptions
    /// under the same session id coexist; each receives a dedicated
    /// channel plus a handle whose `Drop` removes only that
    /// subscription from the registry.
    pub fn register(
        &self,
        session: SessionId,
        capacity: usize,
    ) -> (SessionHandle, mpsc::Receiver<HostnameEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        let token = self.inner.next_token.fetch_add(1, Ordering::Relaxed);
        self.inner
            .sessions
            .entry(session.clone())
            .or_default()
            .push(Subscription { token, tx });
        let handle = SessionHandle {
            session,
            token,
            inner: self.inner.clone(),
        };
        (handle, rx)
    }

    /// Deliver `event` to every live subscription of every session in
    /// `recipients`. Channels that are closed or full silently drop the
    /// event (best-effort; the browser's EventSource reconnect and
    /// TanStack-Query refetch-on-reconnect are the recovery path).
    pub fn fanout(&self, recipients: &[SessionId], event: HostnameEvent) {
        for session in recipients {
            if let Some(subs) = self.inner.sessions.get(session) {
                for sub in subs.iter() {
                    let _ = sub.tx.try_send(event.clone());
                }
            }
        }
    }

    /// Number of distinct sessions currently registered. A session
    /// appears here regardless of how many subscriptions it owns.
    /// Diagnostic; used by integration tests to assert registry
    /// lifecycle.
    #[doc(hidden)]
    pub fn session_count(&self) -> usize {
        self.inner.sessions.len()
    }
}

/// RAII handle returned by [`HostnameBroadcaster::register`]. Dropping it
/// removes exactly the subscription it owns from the registry; other
/// subscriptions registered under the same session id are preserved.
pub struct SessionHandle {
    session: SessionId,
    token: u64,
    inner: Arc<Inner>,
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        // Scope the `get_mut` guard so the shard lock is released before
        // `remove_if` re-acquires it (DashMap's `remove_if` locks the
        // same shard internally, and holding a mut guard across the call
        // would deadlock).
        {
            if let Some(mut entry) = self.inner.sessions.get_mut(&self.session) {
                entry.retain(|sub| sub.token != self.token);
                if !entry.is_empty() {
                    return;
                }
            } else {
                return;
            }
        }
        // Atomically remove the entry only if it's still empty. A
        // concurrent `register` between the `get_mut` drop and here
        // would have re-pushed; the `is_empty` predicate keeps that
        // subscription alive.
        self.inner
            .sessions
            .remove_if(&self.session, |_, v| v.is_empty());
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

    #[tokio::test]
    async fn multiple_subscriptions_under_one_session_all_receive() {
        let broker = HostnameBroadcaster::new();
        let session = SessionId::new("a");
        let (_h1, mut rx1) = broker.register(session.clone(), 4);
        let (_h2, mut rx2) = broker.register(session.clone(), 4);

        assert_eq!(
            broker.session_count(),
            1,
            "two subscriptions under one session still count as one session",
        );

        broker.fanout(std::slice::from_ref(&session), sample_event());

        let got1 = timeout(Duration::from_millis(50), rx1.recv())
            .await
            .expect("first subscription should receive event")
            .expect("channel open");
        let got2 = timeout(Duration::from_millis(50), rx2.recv())
            .await
            .expect("second subscription should receive event")
            .expect("channel open");
        assert_eq!(got1.ip, sample_event().ip);
        assert_eq!(got2.ip, sample_event().ip);
    }

    #[tokio::test]
    async fn dropping_one_subscription_preserves_siblings() {
        let broker = HostnameBroadcaster::new();
        let session = SessionId::new("a");
        let (h1, mut rx1) = broker.register(session.clone(), 4);
        let (_h2, mut rx2) = broker.register(session.clone(), 4);

        // Drop the first handle. The session entry must stay alive
        // because the second subscription is still registered.
        drop(h1);
        assert_eq!(broker.session_count(), 1);

        broker.fanout(std::slice::from_ref(&session), sample_event());

        // The surviving subscription receives the event.
        let got = timeout(Duration::from_millis(50), rx2.recv())
            .await
            .expect("surviving subscription should receive event")
            .expect("channel open");
        assert_eq!(got.ip, sample_event().ip);

        // The dropped subscription's channel is closed — `recv()`
        // returns `None` rather than an event.
        assert!(
            matches!(
                timeout(Duration::from_millis(50), rx1.recv()).await,
                Ok(None) | Err(_)
            ),
            "dropped subscription must not receive the event",
        );
    }

    #[tokio::test]
    async fn dropping_all_subscriptions_clears_session() {
        let broker = HostnameBroadcaster::new();
        let session = SessionId::new("a");
        let (h1, _rx1) = broker.register(session.clone(), 4);
        let (h2, _rx2) = broker.register(session.clone(), 4);
        assert_eq!(broker.session_count(), 1);

        drop(h1);
        assert_eq!(broker.session_count(), 1);
        drop(h2);
        assert_eq!(broker.session_count(), 0);
    }
}
