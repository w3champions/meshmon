//! ipgeolocation.io enrichment provider.
//!
//! Wraps the ipgeolocation.io REST API (`/ipgeo` single-lookup endpoint and
//! `/v3/ipgeo-bulk` batched endpoint) and maps the JSON response shape into
//! the runner's field-neutral [`EnrichmentResult`].
//!
//! # Split between `map_single` and `lookup`
//!
//! As with the RDAP provider (Task 7) the parsing logic is split out from
//! the network call so it is directly unit-testable:
//!
//! - [`IpGeoProvider::map_single`] is pure — `serde_json::Value` in,
//!   [`EnrichmentResult`] out. Used by both the single-lookup path and the
//!   bulk-response fan-out.
//! - [`IpGeoProvider::lookup`] performs the actual `GET /ipgeo` request.
//!   Response classifications (200 / 401 / 403 / 404 / 429 / 5xx) map to
//!   the corresponding [`EnrichmentError`] variants so the runner can decide
//!   retry/backoff correctly.
//! - [`IpGeoProvider::bulk`] posts to `/v3/ipgeo-bulk` for up to 50 000 IPs
//!   per call. The runner wires it in Task 10 when ≥ 25 pending jobs
//!   accumulate; today it is reachable but unused (marked `#[allow(dead_code)]`).
//!
//! # API key handling
//!
//! The ipgeolocation.io API takes its key as a query-string parameter
//! (`?apiKey=...`). To avoid leaking the key into log lines or error
//! messages we build URLs via [`reqwest::Url::parse_with_params`] and
//! never embed the fully-qualified URL in mapped error strings.

use crate::catalogue::model::Field;
use crate::enrichment::{EnrichmentError, EnrichmentProvider, EnrichmentResult, FieldValue};
use async_trait::async_trait;
use reqwest::{Client, Url};
use serde_json::Value;
use std::net::IpAddr;
use std::time::Duration;
use tracing::{debug, trace};

/// Stable identifier for this provider — appears in logs and metrics
/// labels. Kept as a module-level constant so the
/// [`EnrichmentProvider::id`] return value and any future metric /
/// logging sites cannot drift.
pub(crate) const ID: &str = "ipgeolocation";

/// Base URL of the single-lookup endpoint. Split out as a constant so
/// tests can swap it when running against a `wiremock` mock server.
const SINGLE_ENDPOINT: &str = "https://api.ipgeolocation.io/ipgeo";

/// Base URL of the v3 bulk endpoint (up to 50 000 IPs per call).
const BULK_ENDPOINT: &str = "https://api.ipgeolocation.io/v3/ipgeo-bulk";

/// HTTP client timeout — caps a single hung request. The runner also
/// enforces its own upstream budget; this is the inner floor.
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// Default back-off hint returned in [`EnrichmentError::RateLimited`]
/// when the upstream does not carry a `Retry-After` header. One minute
/// aligns with ipgeolocation.io's documented per-minute quota buckets.
const DEFAULT_RATE_LIMIT_RETRY_AFTER: Duration = Duration::from_secs(60);

/// ipgeolocation.io provider — queries `api.ipgeolocation.io` for city,
/// country, geo coordinates, ASN and organisation.
pub struct IpGeoProvider {
    /// Shared `reqwest` client. Held on the struct so connection pools
    /// are reused across lookups.
    client: Client,
    /// API key used for the `apiKey=` query parameter. The key is never
    /// written to log lines or mapped error strings — only attached to
    /// the outbound request URL via [`Url::parse_with_params`].
    api_key: String,
    /// Base URL of the single-lookup endpoint. Defaults to
    /// [`SINGLE_ENDPOINT`]; tests override via
    /// [`IpGeoProvider::with_endpoint`].
    single_endpoint: String,
    /// Base URL of the bulk endpoint. Defaults to [`BULK_ENDPOINT`];
    /// tests override via [`IpGeoProvider::with_endpoint`].
    bulk_endpoint: String,
}

impl IpGeoProvider {
    /// Construct a new provider with the given API key.
    ///
    /// Only builds the HTTP client — no network access happens until
    /// the first [`EnrichmentProvider::lookup`] call. Returns
    /// [`EnrichmentError::Permanent`] if the underlying `reqwest`
    /// client cannot be built (TLS initialisation failure and similar
    /// non-retryable conditions). Matching the RDAP provider's
    /// constructor shape keeps the provider wiring uniform across the
    /// chain.
    pub fn new(api_key: String) -> Result<Self, EnrichmentError> {
        let client = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| {
                EnrichmentError::Permanent(format!("ipgeolocation client init failed: {e}"))
            })?;
        Ok(Self {
            client,
            api_key,
            single_endpoint: SINGLE_ENDPOINT.to_string(),
            bulk_endpoint: BULK_ENDPOINT.to_string(),
        })
    }

    /// Override the upstream endpoints. Used from tests to point the
    /// provider at a `wiremock::MockServer`.
    #[cfg(test)]
    pub(crate) fn with_endpoint(
        mut self,
        single: impl Into<String>,
        bulk: impl Into<String>,
    ) -> Self {
        self.single_endpoint = single.into();
        self.bulk_endpoint = bulk.into();
        self
    }

    /// Pure mapping from the provider JSON shape to the runner's
    /// [`EnrichmentResult`] shape.
    ///
    /// Fields absent from the body are not inserted — the runner's
    /// first-writer-wins merge relies on missing keys, not
    /// present-with-empty-value. `latitude` / `longitude` accept both
    /// string form (ipgeolocation.io's documented default) and native
    /// JSON float form, so the mapping is robust against response
    /// shape changes or bulk-endpoint variants that drop the stringified
    /// encoding.
    pub(crate) fn map_single(body: &Value) -> EnrichmentResult {
        let mut out = EnrichmentResult::default();
        if let Some(s) = body["city"].as_str() {
            out.fields.insert(Field::City, FieldValue::Text(s.into()));
        }
        if let Some(s) = body["country_code2"].as_str() {
            out.fields
                .insert(Field::CountryCode, FieldValue::Text(s.into()));
        }
        if let Some(s) = body["country_name"].as_str() {
            out.fields
                .insert(Field::CountryName, FieldValue::Text(s.into()));
        }
        if let Some(lat) = parse_float(&body["latitude"]) {
            out.fields.insert(Field::Latitude, FieldValue::F64(lat));
        }
        if let Some(lon) = parse_float(&body["longitude"]) {
            out.fields.insert(Field::Longitude, FieldValue::F64(lon));
        }
        // ASN shape depends on the account / endpoint version:
        //   * Legacy flat response:  `"asn": "AS15169"`
        //   * v2 nested response:    `"asn": { "as_number": "AS15169", ... }`
        //   * Defensive bare int:    `"asn": 15169`
        // 4-byte ASNs (RFC 4893) above `i32::MAX` are dropped rather than
        // silently wrapped to negative values — `Field::Asn` is stored as i32.
        let asn = body["asn"]["as_number"]
            .as_str()
            .or_else(|| body["asn"].as_str())
            .and_then(parse_as_number)
            .or_else(|| body["asn"].as_i64().and_then(|n| i32::try_from(n).ok()));
        if let Some(asn) = asn {
            out.fields.insert(Field::Asn, FieldValue::I32(asn));
        }
        // Organization is top-level on the legacy/flat response and nested
        // under `asn` (or `company`) on v2. Walk the three shapes in the
        // same preference order as ASN so both API versions populate the
        // field without a config switch.
        let organization = body["organization"]
            .as_str()
            .or_else(|| body["asn"]["organization"].as_str())
            .or_else(|| body["company"]["organization"].as_str());
        if let Some(org) = organization {
            out.fields
                .insert(Field::NetworkOperator, FieldValue::Text(org.into()));
        }
        out
    }

    /// Build the `?apiKey=…&ip=…` URL for the single-lookup endpoint.
    ///
    /// Kept separate so [`EnrichmentError::Transient`] messages from a
    /// failed `reqwest::send` don't embed the URL (which contains the
    /// API key) in log output.
    fn single_url(&self, ip: IpAddr) -> Result<Url, EnrichmentError> {
        Url::parse_with_params(
            &self.single_endpoint,
            &[("apiKey", self.api_key.as_str()), ("ip", &ip.to_string())],
        )
        .map_err(|e| EnrichmentError::Permanent(format!("invalid single-lookup URL: {e}")))
    }

    /// Build the `?apiKey=…` URL for the bulk endpoint.
    fn bulk_url(&self) -> Result<Url, EnrichmentError> {
        Url::parse_with_params(&self.bulk_endpoint, &[("apiKey", self.api_key.as_str())])
            .map_err(|e| EnrichmentError::Permanent(format!("invalid bulk URL: {e}")))
    }

    /// Bulk lookup via `POST /v3/ipgeo-bulk` — up to 50 000 IPs per call.
    ///
    /// Wired by the Task-10 runner when ≥ 25 pending jobs accumulate.
    /// Until then this method is reachable but unused, so
    /// `#[allow(dead_code)]` keeps the lint clean.
    // `dead_code` allow: only invoked once the Task 10 runner is wired.
    // Ships in Task 8 so Task 10 can integrate it without another
    // per-provider edit.
    #[allow(dead_code)]
    pub(crate) async fn bulk(
        &self,
        ips: &[IpAddr],
    ) -> Result<Vec<(IpAddr, EnrichmentResult)>, EnrichmentError> {
        let body = serde_json::json!({
            "ips": ips.iter().map(|ip| ip.to_string()).collect::<Vec<_>>()
        });
        let url = self.bulk_url()?;
        let resp = self
            .client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| EnrichmentError::Transient(e.without_url().to_string()))?;
        // Mirror lookup()'s 5-class HTTP status map so bulk() does not surface
        // 401 (permanent credential failure) or 429 (needs backoff) as generic
        // Transient errors. Keeping the two methods in sync is the reason the
        // array-parsing path stays inside the 200 arm.
        match resp.status().as_u16() {
            200 => {
                let arr: Vec<Value> = resp
                    .json()
                    .await
                    .map_err(|e| EnrichmentError::Transient(e.without_url().to_string()))?;
                Ok(arr
                    .into_iter()
                    .filter_map(|v| {
                        let ip: IpAddr = v["ip"].as_str()?.parse().ok()?;
                        Some((ip, Self::map_single(&v)))
                    })
                    .collect())
            }
            401 | 403 => Err(EnrichmentError::Unauthorized),
            // TODO(T10): read upstream `Retry-After` header when present and
            // prefer it over DEFAULT_RATE_LIMIT_RETRY_AFTER; the runner's
            // backoff budget will honour a server-supplied hint once wired.
            429 => Err(EnrichmentError::RateLimited {
                retry_after: Some(DEFAULT_RATE_LIMIT_RETRY_AFTER),
            }),
            404 => Err(EnrichmentError::NotFound),
            code if (500..600).contains(&code) => {
                Err(EnrichmentError::Transient(format!("http {code}")))
            }
            code => Err(EnrichmentError::Permanent(format!("http {code}"))),
        }
    }
}

/// Parse ipgeolocation.io's ASN string form (e.g. `"AS15169"`) into a
/// plain `i32`. Accepts the bare numeric form (`"15169"`) as well so a
/// future response shape that drops the `AS` prefix still round-trips.
/// Case-insensitive on the prefix. Returns `None` when the trimmed
/// remainder is not a valid `i32` — 4-byte ASNs above `i32::MAX` fall
/// into this branch and are dropped rather than wrapped to negatives.
fn parse_as_number(s: &str) -> Option<i32> {
    let trimmed = s.trim();
    let numeric = trimmed
        .strip_prefix("AS")
        .or_else(|| trimmed.strip_prefix("as"))
        .unwrap_or(trimmed);
    numeric.parse::<i32>().ok()
}

/// Parse a JSON value as an `f64`. ipgeolocation.io historically returns
/// `latitude` / `longitude` as strings (e.g. `"37.3861"`); some endpoints
/// (notably the bulk response) emit native JSON floats. Both are accepted.
///
/// Non-finite values (NaN, ±Infinity) are rejected — `"NaN".parse::<f64>()`
/// succeeds in Rust and PostgreSQL's `DOUBLE PRECISION` accepts non-finite
/// doubles, so without this guard a malformed upstream response would poison
/// the coordinate columns.
fn parse_float(v: &Value) -> Option<f64> {
    let f = v
        .as_f64()
        .or_else(|| v.as_str()?.trim().parse::<f64>().ok())?;
    f.is_finite().then_some(f)
}

/// Static list of fields [`IpGeoProvider`] can populate. Returned from
/// [`EnrichmentProvider::supported`] so the runner can skip the
/// provider when every supported field is already locked or filled.
const SUPPORTED_FIELDS: &[Field] = &[
    Field::City,
    Field::CountryCode,
    Field::CountryName,
    Field::Latitude,
    Field::Longitude,
    Field::Asn,
    Field::NetworkOperator,
];

#[async_trait]
impl EnrichmentProvider for IpGeoProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn supported(&self) -> &'static [Field] {
        SUPPORTED_FIELDS
    }

    async fn lookup(&self, ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
        let url = self.single_url(ip)?;
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| EnrichmentError::Transient(e.without_url().to_string()))?;
        let status = resp.status().as_u16();
        debug!(%ip, status, "ipgeolocation: response status");
        match status {
            200 => {
                let body: Value = resp
                    .json()
                    .await
                    .map_err(|e| EnrichmentError::Transient(e.without_url().to_string()))?;
                // Diagnose shape mismatches (e.g. "asn" as nested object vs
                // bare integer) without dumping the full payload at debug.
                // Full body is gated on `trace!` for deep investigation.
                let top_keys: Vec<&str> = body
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                debug!(
                    %ip,
                    asn_kind = body["asn"].as_str().map(|_| "string")
                        .or_else(|| body["asn"].as_i64().map(|_| "number"))
                        .or_else(|| body["asn"].as_object().map(|_| "object"))
                        .unwrap_or("absent"),
                    has_organization = body.get("organization").is_some(),
                    has_location = body.get("location").is_some(),
                    ?top_keys,
                    "ipgeolocation: response shape"
                );
                trace!(%ip, body = %body, "ipgeolocation: full response body");
                let mapped = Self::map_single(&body);
                debug!(
                    %ip,
                    mapped_fields = ?mapped.fields.keys().collect::<Vec<_>>(),
                    "ipgeolocation: mapped fields"
                );
                Ok(mapped)
            }
            401 | 403 => Err(EnrichmentError::Unauthorized),
            // TODO(T10): read upstream `Retry-After` header when present and
            // prefer it over DEFAULT_RATE_LIMIT_RETRY_AFTER; the runner's
            // backoff budget will honour a server-supplied hint once wired.
            429 => Err(EnrichmentError::RateLimited {
                retry_after: Some(DEFAULT_RATE_LIMIT_RETRY_AFTER),
            }),
            404 => Err(EnrichmentError::NotFound),
            code if (500..600).contains(&code) => {
                Err(EnrichmentError::Transient(format!("http {code}")))
            }
            code => Err(EnrichmentError::Permanent(format!("http {code}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn maps_legacy_flat_response_with_string_asn() {
        // Legacy/flat ipgeolocation.io response: `asn` is a string like
        // "AS15169" and `organization` is top-level. This was the shape the
        // provider returned when this module was originally written and is
        // still served by many account tiers today.
        let body = serde_json::json!({
            "ip": "8.8.8.8",
            "city": "Mountain View",
            "country_code2": "US",
            "country_name": "United States",
            "latitude": "37.3861",
            "longitude": "-122.0839",
            "asn": "AS15169",
            "organization": "GOOGLE"
        });
        let r = IpGeoProvider::map_single(&body);
        assert!(
            matches!(r.fields.get(&Field::City), Some(FieldValue::Text(c)) if c == "Mountain View")
        );
        assert!(
            matches!(r.fields.get(&Field::Latitude), Some(FieldValue::F64(v)) if (*v - 37.3861).abs() < 1e-6)
        );
        assert!(matches!(
            r.fields.get(&Field::Asn),
            Some(FieldValue::I32(15169))
        ));
        assert!(
            matches!(r.fields.get(&Field::NetworkOperator), Some(FieldValue::Text(n)) if n == "GOOGLE")
        );
    }

    #[test]
    fn maps_v2_nested_response() {
        // v2 response: `asn` is an object containing `as_number` + nested
        // `organization`. Flat top-level `organization` is absent. The
        // parser must walk the nested shape for both ASN and org.
        let body = serde_json::json!({
            "ip": "2400:cb00:2048:1::6810:1b01",
            "location": {
                "city": "San Francisco",
                "country_code2": "US",
                "country_name": "United States",
                "latitude": "37.7749",
                "longitude": "-122.4194",
            },
            "asn": {
                "as_number": "AS13335",
                "organization": "Cloudflare, Inc.",
                "country": "US",
                "type": "BUSINESS",
            },
            "company": {
                "organization": "Cloudflare, Inc.",
                "type": "HOSTING",
            }
        });
        let r = IpGeoProvider::map_single(&body);
        assert!(
            matches!(r.fields.get(&Field::Asn), Some(FieldValue::I32(13335))),
            "v2 nested asn.as_number should parse into i32, got {:?}",
            r.fields.get(&Field::Asn),
        );
        assert!(
            matches!(r.fields.get(&Field::NetworkOperator), Some(FieldValue::Text(n)) if n == "Cloudflare, Inc.")
        );
    }

    #[test]
    fn maps_defensive_bare_int_asn() {
        // Defensive branch: if some future response shape exposes `asn` as
        // a bare integer, the parser must still consume it.
        let body = serde_json::json!({ "asn": 15169 });
        let r = IpGeoProvider::map_single(&body);
        assert!(matches!(
            r.fields.get(&Field::Asn),
            Some(FieldValue::I32(15169))
        ));
    }

    #[test]
    fn parse_as_number_accepts_various_forms() {
        assert_eq!(super::parse_as_number("AS15169"), Some(15169));
        assert_eq!(super::parse_as_number("as15169"), Some(15169));
        assert_eq!(super::parse_as_number(" AS15169 "), Some(15169));
        assert_eq!(super::parse_as_number("15169"), Some(15169));
        assert_eq!(super::parse_as_number("AS"), None);
        assert_eq!(super::parse_as_number("garbage"), None);
        // 4-byte ASN above i32::MAX → drop rather than wrap.
        assert_eq!(super::parse_as_number("AS4294967295"), None);
    }

    #[test]
    fn map_single_accepts_native_float_lat_lon() {
        // Bulk endpoint (v3) may emit latitude/longitude as native JSON
        // floats rather than strings. parse_float() must accept both forms
        // so the bulk fan-out uses the same mapping code path as single
        // lookups without bespoke handling.
        let body = serde_json::json!({
            "latitude": 37.3861,
            "longitude": -122.0839,
        });
        let r = IpGeoProvider::map_single(&body);
        assert!(
            matches!(r.fields.get(&Field::Latitude), Some(FieldValue::F64(v)) if (*v - 37.3861).abs() < 1e-6)
        );
        assert!(
            matches!(r.fields.get(&Field::Longitude), Some(FieldValue::F64(v)) if (*v - -122.0839).abs() < 1e-6)
        );
    }

    #[test]
    fn map_single_partial_body_skips_absent_fields() {
        // A partial response (city only, no country / geo / ASN) must
        // produce a result containing just the City entry — missing keys
        // stay absent from the output HashMap, not present-with-None. The
        // runner's first-writer-wins merge depends on this shape.
        let body = serde_json::json!({
            "city": "Frankfurt am Main"
        });
        let r = IpGeoProvider::map_single(&body);
        assert_eq!(r.fields.len(), 1);
        assert!(
            matches!(r.fields.get(&Field::City), Some(FieldValue::Text(c)) if c == "Frankfurt am Main")
        );
        assert!(!r.fields.contains_key(&Field::CountryCode));
        assert!(!r.fields.contains_key(&Field::Latitude));
        assert!(!r.fields.contains_key(&Field::Asn));
    }

    #[test]
    fn map_single_empty_body_produces_empty_result() {
        // No recognised fields → empty result. any_populated() downstream
        // relies on the map being empty, not populated-with-Nones.
        let r = IpGeoProvider::map_single(&serde_json::json!({}));
        assert!(r.fields.is_empty());
    }

    #[test]
    fn map_single_drops_out_of_range_4byte_asn() {
        // RFC 4893 4-byte ASNs above i32::MAX must not be truncated into a
        // negative value — they are dropped instead. The guard fires on
        // every parse branch (string "AS…", nested `as_number`, and bare
        // int), so covering the bare-int branch is enough to lock the
        // saturation policy.
        let body = serde_json::json!({ "asn": 4_294_967_295_i64 });
        let r = IpGeoProvider::map_single(&body);
        assert!(!r.fields.contains_key(&Field::Asn));
    }

    #[test]
    fn parse_float_rejects_nan_and_infinity() {
        // `"NaN".parse::<f64>()` succeeds in Rust and PostgreSQL's
        // DOUBLE PRECISION accepts non-finite values, so a permissive
        // parser would poison the coordinate columns. Guard `map_single`
        // by asserting non-finite strings never yield an inserted field.
        let nan_body = serde_json::json!({ "latitude": "NaN" });
        let r = IpGeoProvider::map_single(&nan_body);
        assert!(!r.fields.contains_key(&Field::Latitude));

        let inf_body = serde_json::json!({ "longitude": "Infinity" });
        let r = IpGeoProvider::map_single(&inf_body);
        assert!(!r.fields.contains_key(&Field::Longitude));
    }

    #[test]
    fn supported_fields_match_what_map_single_can_write() {
        // Build a body exercising every supported field so map_single's
        // output covers the maximal key set. That set must equal
        // SUPPORTED_FIELDS exactly.
        let body = serde_json::json!({
            "city": "Mountain View",
            "country_code2": "US",
            "country_name": "United States",
            "latitude": "37.3861",
            "longitude": "-122.0839",
            "asn": 15169,
            "organization": "GOOGLE",
        });
        let result = IpGeoProvider::map_single(&body);
        let actually_written: std::collections::HashSet<_> =
            result.fields.keys().copied().collect();
        let advertised: std::collections::HashSet<_> = SUPPORTED_FIELDS.iter().copied().collect();
        assert_eq!(
            actually_written, advertised,
            "SUPPORTED_FIELDS must match what map_single writes with a fully-populated body",
        );
    }

    /// Construct a provider pointed at the given `wiremock` mock server.
    fn provider_for(server: &MockServer) -> IpGeoProvider {
        IpGeoProvider::new("test-key".into())
            .expect("reqwest client")
            .with_endpoint(
                format!("{}/ipgeo", server.uri()),
                format!("{}/bulk", server.uri()),
            )
    }

    #[tokio::test]
    async fn lookup_200_returns_mapped_result() {
        // Successful 200 → the body is parsed by map_single and the
        // resulting EnrichmentResult surfaces at least one field.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ipgeo"))
            .and(query_param("apiKey", "test-key"))
            .and(query_param("ip", "8.8.8.8"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ip": "8.8.8.8",
                "city": "Mountain View",
                "country_code2": "US",
                "asn": 15169,
                "organization": "GOOGLE",
            })))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let result = provider
            .lookup("8.8.8.8".parse().unwrap())
            .await
            .expect("200 should map to Ok");

        assert!(
            matches!(result.fields.get(&Field::City), Some(FieldValue::Text(c)) if c == "Mountain View")
        );
        assert!(matches!(
            result.fields.get(&Field::Asn),
            Some(FieldValue::I32(15169))
        ));
    }

    #[tokio::test]
    async fn lookup_401_maps_to_unauthorized() {
        // 401 → EnrichmentError::Unauthorized. The runner currently only
        // logs this — a future enhancement may disable the provider for
        // the remaining rows of the process.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ipgeo"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let err = provider
            .lookup("8.8.8.8".parse().unwrap())
            .await
            .expect_err("401 must be an error");
        assert!(matches!(err, EnrichmentError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn lookup_404_maps_to_not_found() {
        // 404 → NotFound so the runner stops on this provider but still
        // falls through to the next one in the chain.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ipgeo"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let err = provider
            .lookup("8.8.8.8".parse().unwrap())
            .await
            .expect_err("404 must be an error");
        assert!(matches!(err, EnrichmentError::NotFound), "got {err:?}");
    }

    #[tokio::test]
    async fn lookup_429_maps_to_rate_limited_with_default_retry_after() {
        // 429 → RateLimited with the default 60s retry-after hint.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ipgeo"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let err = provider
            .lookup("8.8.8.8".parse().unwrap())
            .await
            .expect_err("429 must be an error");
        assert!(
            matches!(
                err,
                EnrichmentError::RateLimited { retry_after: Some(d) }
                    if d == DEFAULT_RATE_LIMIT_RETRY_AFTER
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn lookup_500_maps_to_transient() {
        // 5xx → Transient so the runner retries with backoff.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ipgeo"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let err = provider
            .lookup("8.8.8.8".parse().unwrap())
            .await
            .expect_err("500 must be an error");
        assert!(matches!(err, EnrichmentError::Transient(_)), "got {err:?}");
        // Production code formats the 5xx branch as `http {code}` — lock the
        // message prefix so a future refactor that drops the classifier
        // string fails loudly rather than silently changing log shape.
        let EnrichmentError::Transient(msg) = &err else {
            unreachable!()
        };
        // Production code formats the 5xx branch as exactly `http {code}` —
        // lock the full string so a future refactor that appends extra
        // context (or drops the classifier) fails loudly.
        assert_eq!(msg, "http 500");
    }

    #[tokio::test]
    async fn transient_error_does_not_leak_api_key() {
        // Regression guard: reqwest::Error's Display appends
        // " for url ({url})" when the error is tied to a request, and that
        // URL carries the apiKey query parameter. A 200 with a non-JSON body
        // hits the json() decode path in lookup(); without .without_url()
        // applied at the map_err site, the resulting Transient message would
        // embed the full apiKey-bearing URL. Assert the scrub sticks.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ipgeo"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let err = provider
            .lookup("8.8.8.8".parse().unwrap())
            .await
            .expect_err("invalid JSON body must surface as Transient");
        let EnrichmentError::Transient(msg) = &err else {
            panic!("expected Transient, got {err:?}");
        };
        assert!(
            !msg.contains("apiKey"),
            "Transient message must not carry `apiKey=` query param: {msg:?}",
        );
        assert!(
            !msg.contains("test-key"),
            "Transient message must not carry the API key value: {msg:?}",
        );
    }

    #[tokio::test]
    async fn bulk_401_maps_to_unauthorized() {
        // Locks the invariant that bulk() mirrors lookup()'s 5-class status
        // map. Before the mirror, any non-2xx (including 401) was coerced
        // into Transient, hiding permanent credential failures from the
        // runner. One classification is enough to lock the mirror; per-code
        // coverage would duplicate lookup()'s test matrix.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/bulk"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let provider = provider_for(&server);
        let err = provider
            .bulk(&["8.8.8.8".parse().unwrap()])
            .await
            .expect_err("401 must be an error");
        assert!(matches!(err, EnrichmentError::Unauthorized), "got {err:?}");
    }
}
