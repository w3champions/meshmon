//! Tonic gRPC service assembly.
//!
//! Exposes [`routes`] which returns an `axum::Router` that REST code
//! can `.merge(...)` into the main router. The `auto::Builder` in
//! `main.rs` dispatches HTTP/1.1 vs HTTP/2 on the same port; both
//! protocols share this single `Router`.
//!
//! Middleware layers applied (outside→in):
//!   1. `AgentApiServer::with_interceptor(agent_grpc_interceptor)` — auth.
//!   2. `.max_decoding_message_size(MAX_GRPC_DECODING_BYTES)` — 1 MiB cap
//!      on incoming payloads (design §3.6).
//!   3. `GovernorLayer` via `.layer(rate_limit)` on the axum router — per-IP
//!      rate limit via tower-governor.

pub mod agent_api;

use crate::http::auth::{agent_api_rate_limit_layer, agent_grpc_interceptor};
use crate::state::AppState;
use meshmon_protocol::AgentApiServer;

/// 1 MiB cap on per-RPC decoded payload size (design §3.6).
pub const MAX_GRPC_DECODING_BYTES: usize = 1 << 20;

/// Build the tonic half of the router.
///
/// Returns an `axum::Router<AppState>` ready to merge into the REST router.
/// The service trait impl ([`agent_api::AgentApiImpl`]) is constructed from
/// `state`; the interceptor and rate-limit layer are attached here.
///
/// # tonic 0.14 API notes
///
/// `tonic::service::Routes::new(svc)` is the single-service constructor.
/// `Routes::prepare()` runs axum's internal router optimisation (see tonic
/// source — it calls `.with_state(())`). `into_axum_router()` returns
/// `axum::Router` (= `Router<()>`).
///
/// To merge the tonic-produced `Router<()>` into the REST router's
/// `Router<AppState>` chain, we call `.with_state::<AppState>(())` after the
/// layer. This is safe because every gRPC handler captures its `AppState`
/// clone directly (via `AgentApiImpl`); no axum `State` extractor is used on
/// the gRPC side.
///
/// # `max_decoding_message_size` placement
///
/// The method lives on `AgentApiServer<T>`, not on `InterceptedService`.
/// We build the sized server first, then wrap it with the auth interceptor
/// via `tonic::service::interceptor::InterceptedService::new`.
pub fn routes(state: AppState) -> axum::Router<AppState> {
    let cfg = state.config();
    let rate_limit = agent_api_rate_limit_layer(
        cfg.service.trust_forwarded_headers,
        cfg.agent_api.rate_limit_per_minute,
        cfg.agent_api.rate_limit_burst,
    );
    let impl_ = agent_api::AgentApiImpl::new(state.clone());
    // Build the server with the 1 MiB decode cap before wrapping with the
    // auth interceptor. `InterceptedService` does not forward the
    // `max_decoding_message_size` setter, so the order matters.
    let sized_server =
        AgentApiServer::new(impl_).max_decoding_message_size(MAX_GRPC_DECODING_BYTES);
    let server = tonic::service::interceptor::InterceptedService::new(
        sized_server,
        agent_grpc_interceptor(state.clone()),
    );

    tonic::service::Routes::new(server)
        .prepare()
        .into_axum_router()
        .layer(rate_limit)
        // `into_axum_router()` yields `Router<()>`; promote to `Router<AppState>`
        // so it can be merged into the REST router's typed chain. No axum State
        // extractor is used on the gRPC side — state lives in `AgentApiImpl`.
        .with_state(())
}
