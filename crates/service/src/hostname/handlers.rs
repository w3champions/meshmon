//! Axum handlers for the hostname-resolution surface.
//!
//! Two endpoints:
//!
//! - `GET /api/hostnames/stream` — long-lived SSE stream of
//!   `hostname_resolved` events scoped to the caller's session.
//! - `POST /api/hostnames/{ip}/refresh` — force a re-resolution of a
//!   single IP; the resulting event lands on the caller's SSE stream.
//!
//! Both endpoints sit behind `login_required!` via
//! [`crate::http::openapi::api_router`]. The [`SessionId`] is derived
//! from `AuthSession::session.id()` so the broadcaster, the resolver,
//! and the refresh rate-limiter all agree on a single key per session.

use crate::{
    hostname::{HostnameEvent, SessionHandle, SessionId},
    http::auth::AuthSession,
    state::AppState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::Stream;
use std::{convert::Infallible, time::Duration};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

/// Resolve an `AuthSession` extractor to a stable session-id string.
///
/// `auth_session.session.id()` returns `Option<tower_sessions::Id>`.
/// An authenticated session always has an id; we fall back to a
/// placeholder only to keep the types total — `login_required!`
/// already rejects anonymous callers before the handler runs.
fn session_id_from(auth_session: &AuthSession) -> SessionId {
    SessionId::new(
        auth_session
            .session
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "no-session-id".to_string()),
    )
}

/// SSE stream of `hostname_resolved` events scoped to this session.
#[utoipa::path(
    get,
    path = "/api/hostnames/stream",
    tag = "hostnames",
    responses(
        (status = 200, description = "SSE stream of hostname resolutions for this session"),
        (status = 401, description = "No active session"),
    ),
)]
pub async fn hostname_stream(
    State(state): State<AppState>,
    auth_session: AuthSession,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let session = session_id_from(&auth_session);
    let (handle, rx): (SessionHandle, _) = state.hostname_broadcaster.register(session, 64);

    // The outer `move` captures `handle` by value, so the closure takes
    // ownership of the `SessionHandle` and drops it when the stream is
    // dropped (i.e. when the client disconnects). `SessionHandle::drop`
    // unregisters the session from the broadcaster's DashMap. The
    // `let _keep_alive = &handle;` inside the body keeps `handle`
    // referenced so rustc doesn't elide it as dead.
    let stream = ReceiverStream::new(rx).map(move |ev: HostnameEvent| {
        let _keep_alive = &handle;
        let payload = serde_json::to_string(&ev)
            .expect("HostnameEvent serialization is infallible — IpAddr and Option<String>");
        Ok::<_, Infallible>(Event::default().event("hostname_resolved").data(payload))
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Force a re-resolution of a single IP, bypassing cache freshness.
#[utoipa::path(
    post,
    path = "/api/hostnames/{ip}/refresh",
    tag = "hostnames",
    params(
        ("ip" = String, Path, description = "IPv4 or IPv6 literal to re-resolve"),
    ),
    responses(
        (status = 202, description = "Enqueued; event will arrive on the session's stream"),
        (status = 400, description = "Invalid IP literal"),
        (status = 401, description = "No active session"),
        (status = 429, description = "Session exceeded 60 refresh calls / minute"),
    ),
)]
pub async fn hostname_refresh(
    State(state): State<AppState>,
    auth_session: AuthSession,
    Path(ip): Path<String>,
) -> Result<StatusCode, (StatusCode, &'static str)> {
    let session = session_id_from(&auth_session);

    if !state.hostname_refresh_limiter.check_and_increment(&session) {
        return Err((StatusCode::TOO_MANY_REQUESTS, "rate limited"));
    }

    let parsed: std::net::IpAddr = ip
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid ip literal"))?;

    state.hostname_resolver.force_refresh(parsed, session).await;
    Ok(StatusCode::ACCEPTED)
}
