//! Server-Sent Events handler for the campaign broker.
//!
//! A single `GET /api/campaigns/stream` connection per client. The
//! handler subscribes to the broker on [`crate::state::AppState`] and
//! forwards every [`super::broker::CampaignStreamEvent`] as an
//! `Event::data` frame with the JSON-serialized payload. A 15-second
//! keep-alive comment keeps intermediate proxies from idling the
//! connection out.
//!
//! Lag semantics: if the broker's bounded buffer overflows (a slow
//! client, typically), the subscriber's receiver returns
//! `BroadcastStreamRecvError::Lagged(n)`. We translate that into a
//! synthetic `{"kind":"lag","missed":N}` frame so clients can reconcile
//! state by re-fetching rather than seeing a silent gap.

use crate::state::AppState;
use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::{Stream, StreamExt};
use std::{convert::Infallible, time::Duration};
use tokio_stream::wrappers::BroadcastStream;

/// SSE stream of campaign lifecycle events.
///
/// Requires an authenticated session (the enclosing router applies
/// `login_required!`). The response never ends on its own — the server
/// closes it on shutdown or when the client disconnects.
#[utoipa::path(
    get,
    path = "/api/campaigns/stream",
    tag = "campaigns",
    responses(
        (status = 200, description = "SSE stream of campaign changes"),
        (status = 401, description = "No active session"),
    ),
)]
// Wired into `api_router()` in [`crate::http::openapi`]; `pub` is required
// so the `utoipa_axum::routes!` macro can reference the handler across
// modules.
pub async fn campaign_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.campaign_broker.subscribe();
    let stream = BroadcastStream::new(rx).map(|res| {
        let payload = match res {
            Ok(ev) => serde_json::to_string(&ev).expect(
                "CampaignStreamEvent serialization is infallible — all fields are Uuid/enum",
            ),
            Err(err) => {
                let tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(missed) = err;
                // `missed` is u64; JS clients only safely represent integers
                // up to 2^53 − 1. A 512-slot broker can never lag that far,
                // so no cap is needed.
                serde_json::json!({ "kind": "lag", "missed": missed }).to_string()
            }
        };
        Ok::<_, Infallible>(Event::default().data(payload))
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}
