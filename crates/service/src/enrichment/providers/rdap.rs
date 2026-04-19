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
