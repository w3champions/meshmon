//! Session-authenticated reverse proxy for the bundled Grafana.
//!
//! Delegates the HTTP / WebSocket translation to
//! `axum_reverse_proxy::ReverseProxy`. Adds three bespoke concerns:
//!   - Enforces the meshmon session (via `login_required!` wrapping
//!     the router at wire-up time — see `http/mod.rs`).
//!   - Strips any client-supplied `X-WEBAUTH-*` headers, then injects
//!     `X-WEBAUTH-USER` from the validated session (Grafana's
//!     `auth.proxy` identity anchor).
//!   - Applies `X-Forwarded-For` / `X-Real-IP` honouring the service's
//!     `trust_forwarded_headers` policy (see `auth::client_ip`).

use crate::http::auth::{self, AuthSession};
use crate::http::proxy_common::{
    apply_forwarded_headers, strip_client_webauth_headers, strip_session_cookie,
    upstream_missing_response,
};
use crate::state::AppState;
use axum::extract::{ConnectInfo, State};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::Request;
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use axum_reverse_proxy::{ReverseProxy, Rfc9110Config, Rfc9110Layer};
use std::net::SocketAddr;
use tower::ServiceBuilder;

/// URL prefix at which the Grafana reverse proxy is mounted. Must match
/// the `root_url` / `serve_from_sub_path` values in the bundled Grafana's
/// config so internal links keep resolving through the meshmon origin.
pub const MOUNT_PREFIX: &str = "/grafana";

/// Assemble the `/grafana` sub-router. `login_required!` is applied by
/// the caller in `http::router` so this function stays framework-agnostic
/// and unit-testable.
pub fn build_router(state: AppState) -> Router<AppState> {
    let upstream = state
        .config()
        .upstream
        .grafana_url
        .clone()
        .unwrap_or_else(|| "http://grafana-unconfigured.invalid".to_string());

    let inner: Router<AppState> = ReverseProxy::new(MOUNT_PREFIX, &upstream).into();

    inner.layer(
        ServiceBuilder::new()
            .layer(Rfc9110Layer::with_config(Rfc9110Config::default()))
            .layer(from_fn_with_state(state, inject_grafana_headers)),
    )
}

/// Middleware: strip client-supplied `X-WEBAUTH-*`, inject
/// `X-WEBAUTH-USER` from the session, apply forwarded-IP headers.
async fn inject_grafana_headers(
    State(state): State<AppState>,
    auth_session: AuthSession,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.config().upstream.grafana_url.is_none() {
        return upstream_missing_response();
    }

    let Some(principal) = auth_session.user.as_ref() else {
        // `login_required!` should have intercepted this; return 401
        // defensively so a router-wiring mistake degrades gracefully
        // rather than panicking in the middleware.
        return (axum::http::StatusCode::UNAUTHORIZED, "not authenticated").into_response();
    };
    let user = principal.username.clone();

    let trust = state.config().service.trust_forwarded_headers;

    let (mut parts, body) = req.into_parts();
    let real_ip = auth::client_ip(&parts, trust).unwrap_or_else(|| peer.ip());

    strip_client_webauth_headers(&mut parts.headers);
    strip_session_cookie(&mut parts.headers);

    parts.headers.insert(
        HeaderName::from_static("x-webauth-user"),
        HeaderValue::try_from(user.as_str())
            .expect("config-validated username is a valid header value"),
    );

    apply_forwarded_headers(&mut parts.headers, real_ip, trust);

    let req = Request::from_parts(parts, body);
    next.run(req).await
}
