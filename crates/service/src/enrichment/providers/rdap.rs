//! RDAP enrichment provider.
//!
//! Uses [`icann-rdap-client`](icann_rdap_client) to query the IANA-bootstrapped
//! RDAP tree for an IP and extract the subset of fields the catalogue cares
//! about (ASN, network operator, country code).
//!
//! # Split between `map_record` and `lookup`
//!
//! The provider separates *record-shape mapping* from *network I/O*:
//!
//! - [`RdapProvider::map_record`] is a pure function — it takes an
//!   [`RdapRecordFixture`] (provider-neutral shape with the four RDAP fields
//!   we consume) and produces an [`EnrichmentResult`]. Unit tests exercise
//!   this directly; no HTTP or bootstrap store involved.
//! - [`RdapProvider::lookup`] is the async trait method that talks to the
//!   network. The end-to-end path (IANA bootstrap + HTTP fetch + response
//!   parsing) is deferred to Task 10's integration test. Until Task 10
//!   wires the real RDAP call, the method returns
//!   [`EnrichmentError::Permanent`] so the runner does not schedule
//!   retries against a permanently-stubbed provider — it simply falls
//!   through to later providers in the chain. Task 10 replaces the body
//!   with the actual network call.
//!
//! This split matches the plan: the critical scope for Task 7 is the field
//! mapping contract + the provider skeleton; wiring the real RDAP call
//! happens once Task 10's fixtures and test harness are in place.

use crate::catalogue::model::Field;
use crate::enrichment::{EnrichmentError, EnrichmentProvider, EnrichmentResult, FieldValue};
use async_trait::async_trait;
use icann_rdap_client::http::{create_client, Client, ClientConfig};
use icann_rdap_client::iana::MemoryBootstrapStore;
use std::net::IpAddr;

/// Stable identifier for this provider — appears in logs and metrics
/// labels. Kept as a module-level constant so the `EnrichmentProvider::id`
/// return value and any future metrics/logging sites cannot drift.
pub(crate) const ID: &str = "rdap";

/// Provider-neutral view of the RDAP fields the catalogue consumes.
///
/// The real `icann-rdap-client` response type is a large enum tree of
/// RFC 9083 object classes; extracting ASN / organisation from an IP
/// `Network` response requires walking entity `vcard` arrays, which is
/// inherently lossy and noisy. To keep the mapping logic unit-testable
/// (and to give Task 10's integration test a single shape to adapt the
/// real response into), we pre-destructure into this fixture.
#[derive(Debug, Clone, Default)]
pub(crate) struct RdapRecordFixture {
    /// Autonomous System number, if the response carried one.
    pub(crate) asn: Option<i32>,
    /// RDAP `name` field on the IP network object — typically an IRR
    /// net-name like `"CLOUDFLARENET"`.
    pub(crate) net_name: Option<String>,
    /// ISO 3166-1 alpha-2 country code from the IP network object.
    pub(crate) country: Option<String>,
    /// Human-readable organisation name extracted from the network's
    /// primary `registrant` or `abuse` entity, if present. Preferred
    /// over [`Self::net_name`] for the `NetworkOperator` field because
    /// operators recognise "Cloudflare, Inc." more readily than
    /// "CLOUDFLARENET".
    pub(crate) organisation: Option<String>,
}

/// RDAP provider — looks up IP ownership via the IANA-bootstrapped
/// RDAP tree and produces ASN / network-operator / country-code hints.
pub struct RdapProvider {
    /// The wrapped reqwest client used for RDAP requests. Held so that
    /// connection pools are reused across lookups. Currently unused
    /// because [`Self::lookup`] is a `TODO(T10)` stub, but kept on the
    /// struct so the field-level lifecycle (one client per process)
    /// survives the Task-10 rewrite.
    #[allow(dead_code)]
    client: Client,
    /// IANA bootstrap registry cache. Populated lazily on the first
    /// lookup against a given registry (ARIN, RIPE, APNIC, …) so cold
    /// start does not hit IANA five times in a row.
    #[allow(dead_code)]
    bootstrap: MemoryBootstrapStore,
    /// Test-only override: when `Some`, the real Task-10 `lookup`
    /// implementation will skip the IANA bootstrap fetch and issue a
    /// direct `rdap_url_request` against `{base_url}/ip/{ip}`. Lets
    /// the integration tests point the provider at a `wiremock`
    /// [`MockServer`](wiremock::MockServer) without standing up a fake
    /// bootstrap endpoint too. `None` in production paths — the
    /// Task-10 `lookup` body routes through
    /// [`icann_rdap_client::rdap::rdap_bootstrapped_request`] in that
    /// case.
    #[allow(dead_code)]
    base_url_override: Option<String>,
}

impl RdapProvider {
    /// Construct a new provider.
    ///
    /// Only builds the HTTP client — IANA bootstrap fetches happen
    /// lazily on the first `lookup` call, so construction is cheap and
    /// does not require network access. Returns
    /// [`EnrichmentError::Permanent`] if the underlying `reqwest`
    /// client cannot be built (TLS initialisation failure and similar
    /// non-retryable conditions).
    pub fn new() -> Result<Self, EnrichmentError> {
        let config = ClientConfig::default();
        let client = create_client(&config)
            .map_err(|e| EnrichmentError::Permanent(format!("rdap client init failed: {e}")))?;
        Ok(Self {
            client,
            bootstrap: MemoryBootstrapStore::new(),
            base_url_override: None,
        })
    }

    /// Test-only constructor that bypasses the IANA bootstrap step.
    ///
    /// The real production path resolves the RIR RDAP base URL via
    /// [`icann_rdap_client::rdap::rdap_bootstrapped_request`], which
    /// would require standing up a fake IANA bootstrap registry on top
    /// of the mock server. Instead, tests pass the `wiremock`
    /// [`MockServer`](wiremock::MockServer) base URL here and the
    /// Task-10 `lookup` implementation short-circuits the bootstrap,
    /// issuing `GET {base_url}/ip/{ip}` directly.
    ///
    /// The returned client is configured with `https_only(false)` so
    /// the `http://` URL wiremock hands out is accepted — the default
    /// `ClientConfig` refuses non-HTTPS requests.
    // `dead_code` allow: only called from the in-file test module; no
    // production caller today (Task 2 will leave this as a test-only
    // seam even after wiring the real `lookup` body).
    #[allow(dead_code)]
    pub(crate) fn new_with_bootstrap_override(
        base_url: impl Into<String>,
    ) -> Result<Self, EnrichmentError> {
        let config = ClientConfig::builder().https_only(false).build();
        let client = create_client(&config)
            .map_err(|e| EnrichmentError::Permanent(format!("rdap client init failed: {e}")))?;
        Ok(Self {
            client,
            bootstrap: MemoryBootstrapStore::new(),
            base_url_override: Some(base_url.into()),
        })
    }

    /// Pure mapping from the provider-neutral fixture to the runner's
    /// [`EnrichmentResult`] shape.
    ///
    /// Fields absent from the fixture are not inserted — the runner's
    /// first-writer-wins merge relies on `None` slots being missing
    /// from the output `HashMap`, not present-with-empty-value.
    ///
    /// For `NetworkOperator` we prefer `organisation` over `net_name`
    /// when both are present: operators recognise "Cloudflare, Inc."
    /// more readily than "CLOUDFLARENET".
    // `dead_code` allow: only called from unit tests today; Task 10's
    // `lookup` implementation will invoke it once it destructures the
    // real `RdapResponse::Network` into an `RdapRecordFixture`.
    #[allow(dead_code)]
    pub(crate) fn map_record(r: RdapRecordFixture) -> EnrichmentResult {
        let mut out = EnrichmentResult::default();
        if let Some(asn) = r.asn {
            out.fields.insert(Field::Asn, FieldValue::I32(asn));
        }
        if let Some(cc) = r.country {
            out.fields.insert(Field::CountryCode, FieldValue::Text(cc));
        }
        if let Some(netop) = r.organisation.or(r.net_name) {
            out.fields
                .insert(Field::NetworkOperator, FieldValue::Text(netop));
        }
        out
    }
}

/// Static list of fields [`RdapProvider`] can populate. Returned from
/// [`EnrichmentProvider::supported`] so the runner can skip the
/// provider when every supported field is already locked or filled.
const SUPPORTED_FIELDS: &[Field] = &[Field::Asn, Field::NetworkOperator, Field::CountryCode];

#[async_trait]
impl EnrichmentProvider for RdapProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn supported(&self) -> &'static [Field] {
        SUPPORTED_FIELDS
    }

    async fn lookup(&self, _ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
        // TODO(T10): wire the real `rdap_bootstrapped_request` call and
        // destructure the `RdapResponse::Network` into an
        // `RdapRecordFixture` before handing off to `map_record`. The
        // end-to-end RDAP path (IANA bootstrap + HTTP + response parse)
        // is covered by Task 10's integration test; until that fixture
        // exists the call would be untested. Until Task 10 wires the
        // real RDAP call, this returns `Permanent` so the runner does
        // not schedule retries against a permanently-stubbed provider
        // (a `Transient` classification would trigger backoff retries
        // and burn work every cycle). Task 10 replaces this body with
        // the actual network call.
        Err(EnrichmentError::Permanent(
            "rdap lookup not yet wired — TODO(T10)".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Minimal RFC-9083 `ip network` fixture for an IPv4 range.
    ///
    /// Trimmed to the fields [`RdapProvider::map_record`] consumes (ASN
    /// via a network-object extension / name / country / an `entities[0]`
    /// with a vCard `org` for the organisation). Task 2's real
    /// `lookup` body destructures this shape into an
    /// [`RdapRecordFixture`] before handing off to `map_record`. Kept
    /// inline so the fixture stays colocated with the assertions — an
    /// external JSON file would force a second lookup at review time to
    /// confirm the contract.
    fn ipv4_fixture_1_1_1_1() -> serde_json::Value {
        json!({
            "rdapConformance": ["rdap_level_0"],
            "objectClassName": "ip network",
            "handle": "NET-1-1-1-0-1",
            "startAddress": "1.1.1.0",
            "endAddress": "1.1.1.255",
            "ipVersion": "v4",
            "name": "CLOUDFLARENET",
            "type": "DIRECT ALLOCATION",
            "country": "US",
            "status": ["active"],
            "arin_originas0_originautnums": [13335],
            "entities": [
                {
                    "objectClassName": "entity",
                    "handle": "CLOUD14",
                    "roles": ["registrant"],
                    "vcardArray": [
                        "vcard",
                        [
                            ["version", {}, "text", "4.0"],
                            ["fn", {}, "text", "Cloudflare, Inc."],
                            ["kind", {}, "text", "org"],
                            ["org", {"type": "work"}, "text", "Cloudflare, Inc."]
                        ]
                    ]
                }
            ]
        })
    }

    /// Minimal RFC-9083 `ip network` fixture for an IPv6 range.
    ///
    /// Same mapping contract as the IPv4 fixture — we just swap the
    /// address range and `ipVersion` so Task 2's implementation cannot
    /// hard-code an IPv4-only path and still pass both tests.
    fn ipv6_fixture_2606_4700() -> serde_json::Value {
        json!({
            "rdapConformance": ["rdap_level_0"],
            "objectClassName": "ip network",
            "handle": "NET6-2606-4700-1",
            "startAddress": "2606:4700::",
            "endAddress": "2606:4700:ffff:ffff:ffff:ffff:ffff:ffff",
            "ipVersion": "v6",
            "name": "CLOUDFLARENET6",
            "type": "DIRECT ALLOCATION",
            "country": "US",
            "status": ["active"],
            "arin_originas0_originautnums": [13335],
            "entities": [
                {
                    "objectClassName": "entity",
                    "handle": "CLOUD14",
                    "roles": ["registrant"],
                    "vcardArray": [
                        "vcard",
                        [
                            ["version", {}, "text", "4.0"],
                            ["fn", {}, "text", "Cloudflare, Inc."],
                            ["kind", {}, "text", "org"],
                            ["org", {"type": "work"}, "text", "Cloudflare, Inc."]
                        ]
                    ]
                }
            ]
        })
    }

    /// Stand up a `wiremock` server that answers `GET /ip/{ip}` with
    /// the supplied RDAP JSON. Returns the running server so the test
    /// can read its `.uri()` and keep it alive for the duration of the
    /// assertions — dropping the handle would tear the mock down.
    ///
    /// We only stub the single `/ip/{ip}` path the provider is
    /// expected to hit; any other request 404s by default, which makes
    /// bootstrap-path regressions in Task 2 obvious (the test would
    /// start failing with a "No match found" wiremock message rather
    /// than silently succeeding against a permissive stub).
    async fn start_rdap_fixture(ip: &str, body: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/ip/{ip}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/rdap+json")
                    .set_body_json(body),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn lookup_populates_fields_from_canned_rdap_response() {
        // End-to-end contract: given a canned IPv4 RDAP response at a
        // mock RIR endpoint, `lookup()` must parse the body and return
        // ASN/NetworkOperator/CountryCode. This is the failing red
        // test for Task 2 — the current stub returns `Permanent`, so
        // the `.unwrap()` below will panic until Task 2 wires the real
        // `rdap_url_request` call.
        let server = start_rdap_fixture("1.1.1.1", ipv4_fixture_1_1_1_1()).await;
        let provider = RdapProvider::new_with_bootstrap_override(server.uri())
            .expect("rdap provider builds with bootstrap override");

        let result = provider
            .lookup("1.1.1.1".parse().unwrap())
            .await
            .expect("lookup should return Ok once Task 2 wires the real call");

        assert!(
            matches!(result.fields.get(&Field::Asn), Some(FieldValue::I32(13335))),
            "Asn expected 13335, got {:?}",
            result.fields.get(&Field::Asn),
        );
        assert!(
            matches!(
                result.fields.get(&Field::NetworkOperator),
                Some(FieldValue::Text(s)) if s.contains("Cloudflare"),
            ),
            "NetworkOperator expected to contain 'Cloudflare', got {:?}",
            result.fields.get(&Field::NetworkOperator),
        );
        assert!(
            matches!(
                result.fields.get(&Field::CountryCode),
                Some(FieldValue::Text(s)) if s == "US",
            ),
            "CountryCode expected 'US', got {:?}",
            result.fields.get(&Field::CountryCode),
        );
    }

    #[tokio::test]
    async fn lookup_populates_fields_from_canned_rdap_response_ipv6() {
        // IPv6 mirror — guards against Task 2 hard-coding IPv4 in the
        // URL builder or only handling `v4` `ipVersion` values. Same
        // assertions as the IPv4 test so a single mapping bug fails
        // both cases loudly.
        let server = start_rdap_fixture("2606:4700::1111", ipv6_fixture_2606_4700()).await;
        let provider = RdapProvider::new_with_bootstrap_override(server.uri())
            .expect("rdap provider builds with bootstrap override");

        let result = provider
            .lookup("2606:4700::1111".parse().unwrap())
            .await
            .expect("lookup should return Ok once Task 2 wires the real call");

        assert!(
            matches!(result.fields.get(&Field::Asn), Some(FieldValue::I32(13335))),
            "Asn expected 13335, got {:?}",
            result.fields.get(&Field::Asn),
        );
        assert!(
            matches!(
                result.fields.get(&Field::NetworkOperator),
                Some(FieldValue::Text(s)) if s.contains("Cloudflare"),
            ),
            "NetworkOperator expected to contain 'Cloudflare', got {:?}",
            result.fields.get(&Field::NetworkOperator),
        );
        assert!(
            matches!(
                result.fields.get(&Field::CountryCode),
                Some(FieldValue::Text(s)) if s == "US",
            ),
            "CountryCode expected 'US', got {:?}",
            result.fields.get(&Field::CountryCode),
        );
    }

    #[test]
    fn maps_rdap_record_to_enrichment_result() {
        // Given an RDAP record carrying the full set of consumable
        // fields (Cloudflare's 1.1.1.1 record is a convenient shape),
        // map_record must produce exactly three enrichment entries:
        // Asn, CountryCode, and NetworkOperator (preferring
        // `organisation` over `net_name`).
        let fixture = RdapRecordFixture {
            asn: Some(13335),
            net_name: Some("CLOUDFLARENET".into()),
            country: Some("US".into()),
            organisation: Some("Cloudflare, Inc.".into()),
        };

        let r = RdapProvider::map_record(fixture);

        assert_eq!(r.fields.len(), 3);
        assert!(matches!(
            r.fields.get(&Field::Asn),
            Some(FieldValue::I32(13335))
        ));
        assert!(matches!(
            r.fields.get(&Field::CountryCode),
            Some(FieldValue::Text(s)) if s == "US"
        ));
        assert!(matches!(
            r.fields.get(&Field::NetworkOperator),
            Some(FieldValue::Text(s)) if s == "Cloudflare, Inc."
        ));
    }

    #[test]
    fn map_record_falls_back_to_net_name_when_organisation_missing() {
        // The plan spec: `out.fields.insert(..., organisation.or(net_name))`.
        // Verify the fallback actually fires so NetworkOperator is
        // populated even for records without a registrant entity.
        let fixture = RdapRecordFixture {
            asn: None,
            net_name: Some("CLOUDFLARENET".into()),
            country: None,
            organisation: None,
        };

        let r = RdapProvider::map_record(fixture);

        assert_eq!(r.fields.len(), 1);
        assert!(matches!(
            r.fields.get(&Field::NetworkOperator),
            Some(FieldValue::Text(s)) if s == "CLOUDFLARENET"
        ));
    }

    #[test]
    fn map_record_empty_fixture_produces_empty_result() {
        // No RDAP data available → the merge step in the runner must
        // see an empty `fields` map (absent keys, not present-with-
        // None). any_populated() downstream relies on this.
        let r = RdapProvider::map_record(RdapRecordFixture::default());
        assert!(r.fields.is_empty());
    }

    #[test]
    fn supported_fields_match_what_map_record_can_write() {
        // Build a fixture where every *input* field is populated, so
        // map_record's output is the maximal set of keys this provider
        // emits. That set must equal `supported()` exactly — otherwise a
        // provider either advertises a field it never populates, or
        // populates a field the runner does not expect.
        let fixture = RdapRecordFixture {
            asn: Some(13335),
            net_name: Some("CLOUDFLARENET".into()),
            country: Some("US".into()),
            organisation: Some("Cloudflare, Inc.".into()),
        };
        let result = RdapProvider::map_record(fixture);
        let actually_written: std::collections::HashSet<_> =
            result.fields.keys().copied().collect();
        let advertised: std::collections::HashSet<_> = SUPPORTED_FIELDS.iter().copied().collect();
        assert_eq!(
            actually_written, advertised,
            "SUPPORTED_FIELDS must match what map_record writes with a fully-populated fixture",
        );
    }
}
