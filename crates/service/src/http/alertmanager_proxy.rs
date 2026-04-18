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
//!
//! # Reload behaviour
//!
//! Like [`crate::http::grafana_proxy`], the upstream URL is captured
//! at construction; a SIGHUP change to `upstream.alertmanager_url`
//! logs a warn from `main.rs` and takes effect only after restart.

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
///
/// The startup value of `upstream.alertmanager_url` fully determines
/// whether this proxy is active. If the URL is `None` at startup,
/// every request returns a canonical 503 regardless of later reloads
/// — matching the crate's "upstream is bound at router construction"
/// constraint.
pub fn build_router(state: AppState) -> Router<AppState> {
    let Some(upstream) = state.config().upstream.alertmanager_url.clone() else {
        return unconfigured_router(state);
    };

    let inner: Router<AppState> = ReverseProxy::new(MOUNT_PREFIX, &upstream).into();

    inner.layer(
        ServiceBuilder::new()
            .layer(Rfc9110Layer::with_config(Rfc9110Config::default()))
            .layer(from_fn_with_state(state, inject_am_headers)),
    )
}

/// Router that short-circuits every `/alertmanager/*` hit with the
/// canonical 503 body. Used when `upstream.alertmanager_url` is unset
/// at startup.
fn unconfigured_router(state: AppState) -> Router<AppState> {
    let inner: Router<AppState> =
        ReverseProxy::new(MOUNT_PREFIX, "http://alertmanager-unconfigured.invalid").into();
    inner.layer(from_fn_with_state(state, force_upstream_missing))
}

/// Fallback middleware for the unconfigured branch: returns 503 on
/// every request without ever forwarding.
async fn force_upstream_missing(
    _state: State<AppState>,
    _req: Request<axum::body::Body>,
    _next: Next,
) -> Response {
    upstream_missing_response()
}

/// Middleware: strip client-supplied `X-WEBAUTH-*` (symmetry with the
/// Grafana proxy — AM ignores these but they're noise in access logs),
/// then apply forwarded-IP headers. Reached only when
/// [`build_router`] found a configured upstream at startup.
async fn inject_am_headers(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
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
