//! Session-authenticated reverse proxy for the bundled Alertmanager.
//!
//! Shares the `axum-reverse-proxy` + `Rfc9110Layer` pattern with
//! `grafana_proxy.rs`. Differences:
//!   - No `X-WEBAUTH-USER` injection — Alertmanager has no `auth.proxy`
//!     equivalent; edge session check + bridge isolation are the
//!     entire auth surface.
//!   - Client-supplied `X-WEBAUTH-*` is still stripped for symmetry
//!     (AM ignores the header, but operators browsing the AM SPA
//!     through the proxy shouldn't have random `X-WEBAUTH-*` headers
//!     show up in AM's access logs either way).
//!
//! `alerts_proxy.rs` (the JSON-normalising `/api/alerts` gateway)
//! stays independent; both surfaces point at the same upstream via
//! `upstream.alertmanager_url`.

use crate::http::auth;
use crate::http::proxy_common::{
    apply_forwarded_headers, strip_client_webauth_headers, strip_session_cookie,
    upstream_missing_response,
};
use crate::state::AppState;
use axum::extract::{ConnectInfo, State};
use axum::http::Request;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::Response;
use axum::Router;
use axum_reverse_proxy::{ReverseProxy, Rfc9110Config, Rfc9110Layer};
use std::net::SocketAddr;
use tower::ServiceBuilder;

/// Path prefix where the bundled Alertmanager is mounted.
pub const MOUNT_PREFIX: &str = "/alertmanager";

/// Assemble the `/alertmanager` sub-router. `login_required!` is applied
/// by the caller in `http::router` so this function stays framework-
/// agnostic and unit-testable.
pub fn build_router(state: AppState) -> Router<AppState> {
    let upstream = state
        .config()
        .upstream
        .alertmanager_url
        .clone()
        .unwrap_or_else(|| "http://alertmanager-unconfigured.invalid".to_string());

    let inner: Router<AppState> = ReverseProxy::new(MOUNT_PREFIX, &upstream).into();

    inner.layer(
        ServiceBuilder::new()
            .layer(Rfc9110Layer::with_config(Rfc9110Config::default()))
            .layer(from_fn_with_state(state, inject_am_headers)),
    )
}

/// Middleware: strip client-supplied `X-WEBAUTH-*` (symmetry with the
/// Grafana proxy — AM ignores these but they're noise in access logs),
/// then apply forwarded-IP headers.
async fn inject_am_headers(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.config().upstream.alertmanager_url.is_none() {
        return upstream_missing_response();
    }

    let trust = state.config().service.trust_forwarded_headers;
    let (mut parts, body) = req.into_parts();
    let real_ip = auth::client_ip(&parts, trust).unwrap_or_else(|| peer.ip());

    // Strip any stray `X-WEBAUTH-*` for symmetry with the Grafana proxy.
    // AM doesn't consume the header, but leaking operator-typed-in
    // values into AM access logs is still noise.
    strip_client_webauth_headers(&mut parts.headers);
    strip_session_cookie(&mut parts.headers);
    apply_forwarded_headers(&mut parts.headers, real_ip, trust);

    req = Request::from_parts(parts, body);
    next.run(req).await
}
