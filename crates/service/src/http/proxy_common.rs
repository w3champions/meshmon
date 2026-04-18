//! Bespoke glue that wraps `axum-reverse-proxy`:
//!
//! - **`apply_forwarded_headers`** — honour the existing
//!   `service.trust_forwarded_headers` policy when setting
//!   `X-Forwarded-For` + `X-Real-IP` on the upstream-bound request.
//!   `trust=true` preserves the inbound XFF chain unchanged (the edge
//!   proxy already built the canonical chain). `trust=false` replaces
//!   any inbound XFF/Forwarded with a fresh single-hop value (direct
//!   exposure — inbound chain is untrusted).
//! - **`strip_client_webauth_headers`** — defence-in-depth for the
//!   Grafana proxy. Runs BEFORE the `X-WEBAUTH-USER` injection so an
//!   attacker with a session cannot also supply their own identity
//!   header.
//! - **`strip_session_cookie`** — drop the meshmon session cookie
//!   from the forwarded `Cookie` header so the bearer-equivalent
//!   secret doesn't land in upstream access logs.
//! - **`upstream_missing_response`** — canonical 503 body when
//!   `[upstream].grafana_url` / `.alertmanager_url` is unset.
//!
//! Hop-by-hop header hygiene is NOT in this module — it lives in
//! `axum_reverse_proxy::Rfc9110Layer`, which the handlers mount
//! explicitly. Keep it layered once at the proxy boundary, not
//! reimplemented here.

use crate::http::auth::SESSION_COOKIE_NAME;
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::net::IpAddr;

/// Strip every `X-WEBAUTH-*` header the client tried to set. Call
/// this on the Grafana proxy before injecting the trusted
/// `X-WEBAUTH-USER`; Alertmanager calls this too for symmetry even
/// though AM ignores the header.
///
/// `HeaderName::as_str()` already returns lowercase bytes per the http
/// crate contract, so a simple `starts_with` check suffices — no manual
/// lowercasing needed.
pub(crate) fn strip_client_webauth_headers(headers: &mut HeaderMap) {
    let to_remove: Vec<HeaderName> = headers
        .keys()
        .filter(|k| k.as_str().starts_with("x-webauth-"))
        .cloned()
        .collect();
    for k in to_remove {
        headers.remove(k);
    }
}

/// Drop the meshmon session cookie from the forwarded `Cookie` header.
///
/// The cookie is a bearer-equivalent secret that upstream services
/// (Grafana / Alertmanager) never consume, but leave sitting in their
/// access logs — a "here's the thing to steal" pointer for any
/// operator doing a debug capture on the upstream side. Strip it at
/// the trust boundary, exactly like `strip_client_webauth_headers`.
///
/// Preserves every other cookie in the header so Grafana's own
/// `grafana_session` cookie (and anything else the browser sends) still
/// reaches the upstream.
pub(crate) fn strip_session_cookie(headers: &mut HeaderMap) {
    let cookie_header = HeaderName::from_static("cookie");
    let inbound: Vec<HeaderValue> = headers.get_all(&cookie_header).iter().cloned().collect();
    if inbound.is_empty() {
        return;
    }

    let prefix = format!("{SESSION_COOKIE_NAME}=");
    let remaining: Vec<String> = inbound
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|raw| raw.split(';').map(|s| s.trim().to_owned()))
        .filter(|pair| !pair.is_empty() && !pair.starts_with(&prefix))
        .collect();

    headers.remove(&cookie_header);
    if remaining.is_empty() {
        return;
    }
    if let Ok(combined) = HeaderValue::try_from(remaining.join("; ")) {
        headers.insert(cookie_header, combined);
    }
}

/// Normalize `X-Forwarded-For` / `X-Real-IP` for the upstream-bound
/// request.
///
/// * `trust_forwarded = true` — we are behind nginx-proxy and the
///   inbound XFF chain is meaningful. Pass it through unchanged; the
///   edge proxy already assembled the canonical chain, and appending
///   `real_ip` (which is the leftmost entry of that same chain) would
///   duplicate the client and corrupt upstream provenance/audit logs.
///   Also collapse multiple `X-Forwarded-For` headers into the
///   RFC-preferred single comma-separated header so the upstream sees
///   one canonical representation.
/// * `trust_forwarded = false` — we are directly exposed; any inbound
///   XFF is attacker-controlled. Remove the inbound header entirely,
///   then write a fresh XFF containing only `real_ip` (which equals
///   the TCP peer in this mode).
///
/// `Forwarded` (RFC 7239) is dropped in the not-trusted case and left
/// alone otherwise — we don't construct a new `Forwarded` element
/// because neither Grafana nor Alertmanager parse it, and our nginx
/// deploy doesn't emit it today.
///
/// `X-Real-IP` always reflects the resolved real client, in both
/// modes, so upstream audit logic has one canonical "who is this"
/// answer regardless of chain length.
pub(crate) fn apply_forwarded_headers(
    headers: &mut HeaderMap,
    real_ip: IpAddr,
    trust_forwarded: bool,
) {
    let real_ip_v = HeaderValue::try_from(real_ip.to_string())
        .expect("a parsed IpAddr always renders to a valid header value");
    let xff = HeaderName::from_static("x-forwarded-for");

    if trust_forwarded {
        // Collapse any split `X-Forwarded-For` headers into a single
        // canonical `client, proxy1, proxy2` string. Do not append
        // anything: the inbound chain is already authoritative.
        let inbound: Vec<String> = headers
            .get_all(&xff)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if !inbound.is_empty() {
            let combined = inbound.join(", ");
            headers.remove(&xff);
            if let Ok(v) = HeaderValue::try_from(combined) {
                headers.insert(xff, v);
            }
        }
    } else {
        headers.remove(&xff);
        headers.remove(HeaderName::from_static("forwarded"));
        headers.insert(xff, real_ip_v.clone());
    }

    headers.insert(HeaderName::from_static("x-real-ip"), real_ip_v);
}

/// 503 response body for an unconfigured upstream.
pub(crate) fn upstream_missing_response() -> Response {
    let mut resp = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "upstream not configured" })),
    )
        .into_response();
    resp.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_removes_all_webauth_variants() {
        let mut h = HeaderMap::new();
        h.insert("x-webauth-user", HeaderValue::from_static("eve"));
        h.insert("x-webauth-email", HeaderValue::from_static("eve@evil"));
        h.insert("cookie", HeaderValue::from_static("session=abc"));
        strip_client_webauth_headers(&mut h);
        assert!(h.get("x-webauth-user").is_none());
        assert!(h.get("x-webauth-email").is_none());
        assert_eq!(h.get("cookie").unwrap(), "session=abc");
    }

    #[test]
    fn strip_session_cookie_removes_only_meshmon_session() {
        let mut h = HeaderMap::new();
        h.insert(
            "cookie",
            HeaderValue::from_static("meshmon_session=deadbeef; grafana_session=abc; other=keep"),
        );
        strip_session_cookie(&mut h);
        let combined = h.get("cookie").unwrap().to_str().unwrap();
        assert!(
            !combined.contains("meshmon_session"),
            "meshmon_session must be removed, got: {combined}"
        );
        assert!(combined.contains("grafana_session=abc"));
        assert!(combined.contains("other=keep"));
    }

    #[test]
    fn strip_session_cookie_removes_header_when_only_cookie() {
        let mut h = HeaderMap::new();
        h.insert("cookie", HeaderValue::from_static("meshmon_session=xyz"));
        strip_session_cookie(&mut h);
        assert!(
            h.get("cookie").is_none(),
            "empty cookie header should be removed entirely",
        );
    }

    #[test]
    fn strip_session_cookie_noop_when_missing() {
        let mut h = HeaderMap::new();
        h.insert("cookie", HeaderValue::from_static("grafana_session=abc"));
        strip_session_cookie(&mut h);
        assert_eq!(h.get("cookie").unwrap(), "grafana_session=abc");
    }

    #[test]
    fn strip_session_cookie_handles_multiple_cookie_headers() {
        // RFC 6265 forbids multiple Cookie headers from the client,
        // but axum/hyper accept them — fold both into a single
        // filtered output.
        let mut h = HeaderMap::new();
        h.append("cookie", HeaderValue::from_static("meshmon_session=a"));
        h.append("cookie", HeaderValue::from_static("grafana_session=b"));
        strip_session_cookie(&mut h);
        let values: Vec<_> = h.get_all("cookie").iter().collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0], "grafana_session=b");
    }

    #[test]
    fn forwarded_trust_true_passes_inbound_chain_unchanged() {
        // `real_ip` in this mode is already the leftmost entry of the
        // inbound chain (auth::client_ip returns XFF[0]); appending it
        // would produce `client, ..., client`. Leave the authoritative
        // chain alone.
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 10.0.0.1"),
        );
        apply_forwarded_headers(&mut h, "198.51.100.1".parse().unwrap(), true);
        assert_eq!(h.get("x-forwarded-for").unwrap(), "198.51.100.1, 10.0.0.1");
        assert_eq!(h.get("x-real-ip").unwrap(), "198.51.100.1");
    }

    #[test]
    fn forwarded_trust_true_collapses_split_xff_headers() {
        // RFC 9110 §5.3 recommends a single comma-separated value;
        // axum/hyper accept multiple XFF headers though. Fold them
        // into one canonical representation for the upstream.
        let mut h = HeaderMap::new();
        h.append("x-forwarded-for", HeaderValue::from_static("198.51.100.1"));
        h.append("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        apply_forwarded_headers(&mut h, "198.51.100.1".parse().unwrap(), true);
        let all: Vec<_> = h.get_all("x-forwarded-for").iter().collect();
        assert_eq!(all.len(), 1, "split XFF must be collapsed");
        assert_eq!(all[0], "198.51.100.1, 10.0.0.1");
    }

    #[test]
    fn forwarded_trust_true_noop_when_no_inbound_xff() {
        // No inbound chain → don't fabricate one. Upstream will see
        // no XFF (fine — X-Real-IP still identifies the caller).
        let mut h = HeaderMap::new();
        apply_forwarded_headers(&mut h, "192.0.2.50".parse().unwrap(), true);
        assert!(
            h.get("x-forwarded-for").is_none(),
            "trust=true must not synthesize XFF from real_ip alone"
        );
        assert_eq!(h.get("x-real-ip").unwrap(), "192.0.2.50");
    }

    #[test]
    fn forwarded_trust_false_drops_inbound_and_writes_fresh() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.1, 10.0.0.1"),
        );
        h.insert("forwarded", HeaderValue::from_static("for=198.51.100.1"));
        apply_forwarded_headers(&mut h, "10.1.2.3".parse().unwrap(), false);
        assert_eq!(h.get("x-forwarded-for").unwrap(), "10.1.2.3");
        assert!(
            h.get("forwarded").is_none(),
            "untrusted inbound `Forwarded` must be dropped"
        );
        assert_eq!(h.get("x-real-ip").unwrap(), "10.1.2.3");
    }
}
