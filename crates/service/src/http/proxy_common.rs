//! Bespoke glue that wraps `axum-reverse-proxy`:
//!
//! - **`forwarded_headers`** — honour the existing
//!   `service.trust_forwarded_headers` policy when setting
//!   `X-Forwarded-For` + `X-Real-IP` on the upstream-bound request.
//!   `trust=true` appends the resolved real client to the existing
//!   XFF chain (behind nginx — preserve the chain). `trust=false`
//!   replaces any inbound XFF/Forwarded with a fresh single-hop value
//!   (direct exposure — inbound chain is untrusted).
//! - **`strip_client_webauth_headers`** — defence-in-depth for the
//!   Grafana proxy. Runs BEFORE the `X-WEBAUTH-USER` injection so an
//!   attacker with a session cannot also supply their own identity
//!   header.
//! - **`upstream_missing_response`** — canonical 503 body when
//!   `[upstream].grafana_url` / `.alertmanager_url` is unset.
//!
//! Hop-by-hop header hygiene is NOT in this module — it lives in
//! `axum_reverse_proxy::Rfc9110Layer`, which the handlers mount
//! explicitly. Keep it layered once at the proxy boundary, not
//! reimplemented here.

// Callers land in the follow-up tasks that wire the grafana/alertmanager
// proxy handlers through these helpers; until then the fns appear unused
// inside the crate under `pub(crate)` visibility. The `#[cfg(test)]` tests
// below still exercise them.
#![allow(dead_code)]

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

/// Append the resolved real client IP to `X-Forwarded-For` and set
/// `X-Real-IP`.
///
/// * `trust_forwarded = true` — we are behind nginx-proxy and the
///   inbound XFF chain is meaningful; append `real_ip` as the last
///   element so the upstream audit log still shows the whole path.
///   (`real_ip` in this mode is already the *leftmost* XFF entry
///   extracted by `auth::client_ip`, so we're echoing the value that
///   made its way through.)
/// * `trust_forwarded = false` — we are directly exposed; any inbound
///   XFF is attacker-controlled. Remove the inbound header entirely,
///   then write a fresh XFF containing only `real_ip` (which equals
///   the TCP peer in this mode).
///
/// `Forwarded` (RFC 7239) gets the same treatment: dropped in the
/// not-trusted case, otherwise left alone — we don't build a new
/// `Forwarded` element because upstream (Grafana / Alertmanager) don't
/// parse it and our nginx deploy doesn't emit it today.
pub(crate) fn apply_forwarded_headers(
    headers: &mut HeaderMap,
    real_ip: IpAddr,
    trust_forwarded: bool,
) {
    let real_ip_v = HeaderValue::try_from(real_ip.to_string())
        .expect("a parsed IpAddr always renders to a valid header value");

    if trust_forwarded {
        let existing: Option<String> = {
            let parts: Vec<String> = headers
                .get_all(HeaderName::from_static("x-forwarded-for"))
                .iter()
                .filter_map(|v| v.to_str().ok())
                .map(|s| s.to_owned())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        };
        let combined = match existing {
            Some(chain) if !chain.is_empty() => format!("{chain}, {real_ip}"),
            _ => real_ip.to_string(),
        };
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::try_from(combined).expect("joined IPs form a valid header value"),
        );
    } else {
        headers.remove(HeaderName::from_static("x-forwarded-for"));
        headers.remove(HeaderName::from_static("forwarded"));
        headers.insert(
            HeaderName::from_static("x-forwarded-for"),
            real_ip_v.clone(),
        );
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
    fn forwarded_trust_true_appends_to_existing_chain() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.1, 10.0.0.1"));
        apply_forwarded_headers(&mut h, "192.0.2.50".parse().unwrap(), true);
        assert_eq!(
            h.get("x-forwarded-for").unwrap(),
            "198.51.100.1, 10.0.0.1, 192.0.2.50"
        );
        assert_eq!(h.get("x-real-ip").unwrap(), "192.0.2.50");
    }

    #[test]
    fn forwarded_trust_true_creates_chain_when_empty() {
        let mut h = HeaderMap::new();
        apply_forwarded_headers(&mut h, "192.0.2.50".parse().unwrap(), true);
        assert_eq!(h.get("x-forwarded-for").unwrap(), "192.0.2.50");
    }

    #[test]
    fn forwarded_trust_false_drops_inbound_and_writes_fresh() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.1, 10.0.0.1"));
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
