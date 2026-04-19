//! Pluggable provider chain for IP catalogue enrichment.
//!
//! # Contract
//!
//! - [`EnrichmentProvider`] is the trait every source (IPGeolocation, RDAP,
//!   MaxMind, WHOIS, …) implements. `id()` is a static string that appears
//!   in logs and metrics labels; `supported()` advertises the set of
//!   [`Field`]s the provider may populate; `lookup()` performs the actual
//!   per-IP call and returns [`EnrichmentResult`] or a typed
//!   [`EnrichmentError`].
//! - The runner (introduced in a later task) walks a fixed provider chain
//!   in declared order, merging each [`EnrichmentResult`] into
//!   [`MergedFields`]. Earlier providers win on conflicts (first-writer-
//!   wins) and any field listed in the row's `operator_edited_fields`
//!   lock array is always skipped.
//! - Providers never write to the database directly — they only compute
//!   fields. Persistence is the runner's responsibility so the lock
//!   contract is enforced in one place.
//!
//! Subsequent tasks in this plan add the concrete providers and the
//! runner that composes them.

pub mod providers;
pub mod runner;

use crate::catalogue::model::Field;
use async_trait::async_trait;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

/// Failure modes surfaced by an [`EnrichmentProvider`].
///
/// The runner uses the variant shape to decide retry/backoff:
/// [`Transient`](Self::Transient) and [`RateLimited`](Self::RateLimited)
/// are retryable; [`Unauthorized`](Self::Unauthorized),
/// [`NotFound`](Self::NotFound), and [`Permanent`](Self::Permanent) are
/// not.
#[derive(Debug, Error)]
pub enum EnrichmentError {
    /// Provider signalled quota exhaustion. `retry_after` is populated
    /// when the response carried a server-specified hint.
    #[error("rate limited, retry after {retry_after:?}")]
    RateLimited {
        /// Suggested wait interval parsed from the provider response.
        retry_after: Option<Duration>,
    },
    /// Credentials missing or rejected. The runner logs and skips this
    /// provider for the current row; subsequent rows will retry. A
    /// follow-up TODO may add per-process disable-on-401, but today the
    /// runner does not carry that state.
    #[error("unauthorized — check API key")]
    Unauthorized,
    /// Provider confirmed the IP has no record. Treated as a terminal
    /// outcome for this lookup — further providers may still run.
    #[error("not found")]
    NotFound,
    /// Network hiccup, timeout, or 5xx. Retryable with backoff.
    #[error("transient: {0}")]
    Transient(String),
    /// Non-retryable failure that isn't one of the specific cases above
    /// (e.g. provider returned a malformed response).
    #[error("permanent: {0}")]
    Permanent(String),
}

impl EnrichmentError {
    /// True iff a later sweep tick could plausibly succeed where this
    /// attempt failed. Used by the runner to decide whether a row that
    /// produced zero populated fields should stay `pending` (so the
    /// sweep picks it up again) or transition to terminal `failed`.
    ///
    /// Retryable: [`Self::RateLimited`] (quota window), [`Self::Transient`]
    /// (network hiccup / 5xx).
    /// Terminal: [`Self::Unauthorized`] (credentials are wrong until the
    /// operator fixes config), [`Self::NotFound`] (provider confirmed
    /// the IP has no record — retrying doesn't help), [`Self::Permanent`]
    /// (malformed response / programmer error).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::Transient(_))
    }
}

/// Value a provider reports for a given [`Field`]. The enum keeps the
/// merge layer simple — every catalogue column is expressible as text,
/// `i32`, or `f64`.
#[derive(Debug, Clone)]
pub enum FieldValue {
    /// Human-readable string (city, country name, operator, …).
    Text(String),
    /// Signed 32-bit integer (currently only ASN).
    I32(i32),
    /// Double-precision float (latitude, longitude).
    F64(f64),
}

/// A single provider's output for one IP.
#[derive(Debug, Default)]
pub struct EnrichmentResult {
    /// Per-field values the provider populated. Absent keys mean the
    /// provider had no data for that field and the runner should fall
    /// through to the next provider.
    pub fields: HashMap<Field, FieldValue>,
}

/// Pluggable enrichment source.
///
/// Every provider is `Send + Sync + 'static` so the runner can store
/// them behind `Arc<dyn EnrichmentProvider>` and drive lookups from a
/// tokio task pool.
#[async_trait]
pub trait EnrichmentProvider: Send + Sync + 'static {
    /// Short, stable identifier used in logs and metrics. Must not
    /// change across releases — dashboards join on this value.
    fn id(&self) -> &'static str;

    /// The set of fields this provider may populate. Used by the runner
    /// to short-circuit calls when every supported field is already
    /// filled or locked.
    fn supported(&self) -> &'static [Field];

    /// Perform the per-IP lookup.
    async fn lookup(&self, ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError>;
}

/// Running accumulation of merged provider output for one IP.
///
/// The runner walks the provider chain and calls [`Self::apply`] for
/// each successful result. Earlier providers win — [`Self::apply`]
/// never overwrites a populated field. Fields listed in the row's
/// `operator_edited_fields` lock array are never written, even if no
/// earlier provider populated them.
///
/// Only the enrichable subset of [`Field`] is represented here:
/// `DisplayName`, `Website`, and `Notes` are operator-only and never
/// populated by providers — a provider that inserts them into
/// [`EnrichmentResult::fields`] has them silently dropped at merge
/// time.
#[derive(Debug, Default)]
pub struct MergedFields {
    /// Resolved city name.
    pub city: Option<String>,
    /// Resolved ISO 3166-1 alpha-2 country code.
    pub country_code: Option<String>,
    /// Resolved country human-readable name.
    pub country_name: Option<String>,
    /// Resolved decimal latitude.
    pub latitude: Option<f64>,
    /// Resolved decimal longitude.
    pub longitude: Option<f64>,
    /// Resolved autonomous system number.
    pub asn: Option<i32>,
    /// Resolved network operator / ISP name.
    pub network_operator: Option<String>,
    /// IDs of providers the runner attempted, in call order. Used by
    /// the metrics layer.
    pub providers_tried: Vec<&'static str>,
}

impl MergedFields {
    /// True iff at least one data field was populated by a provider.
    ///
    /// Derived from the current field state so callers can mutate
    /// [`MergedFields`] without worrying about a stale cached flag.
    /// The runner uses this to decide between the `enriched` and
    /// `failed` terminal states.
    pub fn any_populated(&self) -> bool {
        self.city.is_some()
            || self.country_code.is_some()
            || self.country_name.is_some()
            || self.latitude.is_some()
            || self.longitude.is_some()
            || self.asn.is_some()
            || self.network_operator.is_some()
    }

    /// True iff the enrichable slot for `field` has already been filled.
    ///
    /// Operator-only fields (`DisplayName`, `Website`, `Notes`) are never
    /// provider-writable and return `true` so the short-circuit in
    /// [`Self::needs_provider`] ignores them when deciding if a provider
    /// still has useful work to do.
    pub fn contains(&self, field: Field) -> bool {
        match field {
            Field::City => self.city.is_some(),
            Field::CountryCode => self.country_code.is_some(),
            Field::CountryName => self.country_name.is_some(),
            Field::Latitude => self.latitude.is_some(),
            Field::Longitude => self.longitude.is_some(),
            Field::Asn => self.asn.is_some(),
            Field::NetworkOperator => self.network_operator.is_some(),
            Field::DisplayName | Field::Website | Field::Notes => true,
        }
    }

    /// Runner short-circuit: `false` when every field the provider *could*
    /// populate is already filled or locked — in which case calling
    /// `lookup()` would burn a network request and quota for no merge
    /// write. Mirrors the logic in [`Self::apply`]: a field that is
    /// already `Some` (first-writer-wins) or listed in `locked` (operator
    /// override) is never written regardless of the provider's output.
    pub fn needs_provider(&self, supported: &[Field], locked: &[String]) -> bool {
        supported
            .iter()
            .any(|f| !self.contains(*f) && !locked.iter().any(|lf| lf == f.as_str()))
    }

    /// Merge one provider's output. Tracks the provider id, skips any
    /// field that is already populated (first-writer-wins), and skips
    /// any field listed in `locked` (operator / agent override).
    pub fn apply(&mut self, provider: &'static str, result: EnrichmentResult, locked: &[String]) {
        self.providers_tried.push(provider);
        // First-writer-wins + lock-skip: only write when the destination
        // slot is empty AND the field is not in the operator lock array.
        // A debug_assert fires if a provider returns the wrong
        // `FieldValue` variant so tests catch provider bugs; release
        // builds silently ignore the mismatch.
        macro_rules! take {
            ($dst:ident, $field:ident, $pat:pat => $val:expr) => {
                if self.$dst.is_none() && !locked.iter().any(|f| f == Field::$field.as_str()) {
                    match result.fields.get(&Field::$field) {
                        Some($pat) => self.$dst = Some($val),
                        Some(other) => {
                            debug_assert!(
                                false,
                                "provider '{}' returned unexpected FieldValue variant for {}: {:?}",
                                provider,
                                Field::$field.as_str(),
                                other
                            );
                        }
                        None => {}
                    }
                }
            };
        }
        take!(city, City, FieldValue::Text(s) => s.clone());
        take!(country_code, CountryCode, FieldValue::Text(s) => s.clone());
        take!(country_name, CountryName, FieldValue::Text(s) => s.clone());
        take!(latitude, Latitude, FieldValue::F64(v) => *v);
        take!(longitude, Longitude, FieldValue::F64(v) => *v);
        take!(asn, Asn, FieldValue::I32(v) => *v);
        take!(network_operator, NetworkOperator, FieldValue::Text(s) => s.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> FieldValue {
        FieldValue::Text(s.to_string())
    }

    #[test]
    fn merged_skips_locked_fields() {
        // A provider reports City but the operator has already edited
        // City — the lock must win, even though the destination slot
        // is empty.
        let mut merged = MergedFields::default();
        let mut result = EnrichmentResult::default();
        result.fields.insert(Field::City, text("Frankfurt"));
        result.fields.insert(Field::CountryCode, text("DE"));

        let locked = vec!["City".to_string()];
        merged.apply("test-provider", result, &locked);

        assert_eq!(merged.city, None, "locked City must stay empty");
        assert_eq!(
            merged.country_code.as_deref(),
            Some("DE"),
            "non-locked CountryCode must be written"
        );
        assert!(merged.any_populated());
        assert_eq!(merged.providers_tried, vec!["test-provider"]);
    }

    #[test]
    fn first_writer_wins() {
        // Two providers both report City; the earlier apply() must
        // win and the later call must be a no-op for that field.
        let mut merged = MergedFields::default();

        let mut first = EnrichmentResult::default();
        first.fields.insert(Field::City, text("Frankfurt"));
        merged.apply("primary", first, &[]);

        let mut second = EnrichmentResult::default();
        second.fields.insert(Field::City, text("Berlin"));
        second.fields.insert(Field::CountryName, text("Germany"));
        merged.apply("secondary", second, &[]);

        assert_eq!(
            merged.city.as_deref(),
            Some("Frankfurt"),
            "primary provider must win on City"
        );
        assert_eq!(
            merged.country_name.as_deref(),
            Some("Germany"),
            "secondary provider fills unpopulated CountryName"
        );
        assert_eq!(merged.providers_tried, vec!["primary", "secondary"]);
    }

    #[test]
    fn all_locked_leaves_any_populated_false() {
        // Every enrichable field is locked; even though the provider
        // returns a populated result, no destination slot can be
        // written and `any_populated()` must stay `false`.
        let mut merged = MergedFields::default();
        let mut result = EnrichmentResult::default();
        result.fields.insert(Field::City, text("Frankfurt"));

        let locked: Vec<String> = [
            Field::City,
            Field::CountryCode,
            Field::CountryName,
            Field::Latitude,
            Field::Longitude,
            Field::Asn,
            Field::NetworkOperator,
        ]
        .iter()
        .map(|f| f.as_str().to_string())
        .collect();

        merged.apply("p", result, &locked);

        assert!(
            !merged.any_populated(),
            "all-locked merge must leave any_populated() == false"
        );
        assert_eq!(merged.city, None);
        assert_eq!(merged.providers_tried, vec!["p"]);
    }

    #[test]
    fn needs_provider_false_when_every_supported_field_filled() {
        // Short-circuit contract: a provider whose `supported` set is a
        // subset of the already-filled slots has nothing left to do.
        let merged = MergedFields {
            city: Some("Berlin".into()),
            country_code: Some("DE".into()),
            ..Default::default()
        };

        assert!(
            !merged.needs_provider(&[Field::City, Field::CountryCode], &[]),
            "all supported fields already filled → skip provider"
        );
        assert!(
            merged.needs_provider(&[Field::City, Field::Asn], &[]),
            "at least one unfilled field → still call provider"
        );
    }

    #[test]
    fn needs_provider_false_when_every_supported_field_locked() {
        // Locked fields count as "settled" for short-circuit purposes —
        // the runner would not write them even if the provider returned
        // a value, so making the network call is pure waste.
        let merged = MergedFields::default();
        let locked = vec!["City".to_string(), "CountryCode".to_string()];

        assert!(
            !merged.needs_provider(&[Field::City, Field::CountryCode], &locked),
            "all supported fields locked → skip provider"
        );
        assert!(
            merged.needs_provider(&[Field::City, Field::Asn], &locked),
            "unlocked + unfilled field present → still call provider"
        );
    }

    #[test]
    fn contains_treats_operator_only_fields_as_settled() {
        // DisplayName/Website/Notes are never provider-writable — a
        // provider that lists them in `supported()` would incorrectly
        // look permanently-useful without this carve-out.
        let merged = MergedFields::default();
        assert!(merged.contains(Field::DisplayName));
        assert!(merged.contains(Field::Website));
        assert!(merged.contains(Field::Notes));
        assert!(!merged.contains(Field::City));
    }

    #[test]
    fn enrichment_error_is_retryable_matches_variant_semantics() {
        // Retryable variants: RateLimited (quota window), Transient
        // (network / 5xx). Terminal variants: Unauthorized, NotFound,
        // Permanent. The runner keys its `Pending`-vs-`Failed` decision
        // off this classification, so a miscategorization would either
        // strand rows permanently or thrash the sweep on unrecoverable
        // errors.
        use std::time::Duration;
        assert!(EnrichmentError::RateLimited { retry_after: None }.is_retryable());
        assert!(EnrichmentError::RateLimited {
            retry_after: Some(Duration::from_secs(30))
        }
        .is_retryable());
        assert!(EnrichmentError::Transient("5xx".into()).is_retryable());
        assert!(!EnrichmentError::Unauthorized.is_retryable());
        assert!(!EnrichmentError::NotFound.is_retryable());
        assert!(!EnrichmentError::Permanent("malformed".into()).is_retryable());
    }

    #[test]
    fn empty_result_leaves_any_populated_false() {
        // An empty provider result with no locks records the provider
        // id but writes no data; `any_populated()` stays false.
        let mut merged = MergedFields::default();
        merged.apply("p", EnrichmentResult::default(), &[]);

        assert!(
            !merged.any_populated(),
            "empty result must leave any_populated() == false"
        );
        assert_eq!(
            merged.providers_tried,
            vec!["p"],
            "provider id must still be recorded when it returned nothing"
        );
    }
}
