//! Typed representation of a catalogue row plus the `Field` enum used by
//! `operator_edited_fields` bookkeeping.
//!
//! `operator_edited_fields` is the only override mechanism: every enrichment
//! provider must consult [`CatalogueEntry::is_locked`] before writing to a
//! column. The textual encoding in the DB column is the `Display` form of
//! [`Field`] (PascalCase), and [`Field::as_str`] is the single source of
//! truth for that encoding.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::str::FromStr;
use utoipa::ToSchema;
use uuid::Uuid;

/// Current status of the enrichment pipeline for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "enrichment_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum EnrichmentStatus {
    /// Row exists but no provider has run yet.
    Pending,
    /// At least one provider succeeded.
    Enriched,
    /// Every configured provider failed (terminal).
    Failed,
}

/// Where a catalogue row originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[sqlx(type_name = "catalogue_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CatalogueSource {
    /// Added by an operator (paste flow or single-IP form).
    Operator,
    /// Created implicitly when an agent registered with this IP.
    AgentRegistration,
}

/// Every enrichable catalogue field.
///
/// The variants' [`Field::as_str`] rendering is the single authority for
/// values stored in the `operator_edited_fields` column. Keep this in
/// PascalCase and do not derive serde or strum — the canonical encoding
/// must not depend on feature flags or third-party rename rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Field {
    /// City name (e.g. "Frankfurt am Main").
    City,
    /// ISO 3166-1 alpha-2 country code.
    CountryCode,
    /// Country human-readable name.
    CountryName,
    /// Decimal latitude, −90..=90.
    Latitude,
    /// Decimal longitude, −180..=180.
    Longitude,
    /// Autonomous system number (BGP).
    Asn,
    /// Network operator / ISP name.
    NetworkOperator,
    /// Operator-facing display label.
    DisplayName,
    /// Optional external link (e.g. operator status page).
    Website,
    /// Free-form operator notes.
    Notes,
}

impl Field {
    /// Every enumerable [`Field`] variant. Enables exhaustive iteration in
    /// tests and callers that need to enumerate the catalogue's enrichable
    /// columns without hand-maintaining a parallel list.
    ///
    /// Adding a variant to [`Field`] without updating this slice (and the
    /// [`Field::as_str`] / [`FromStr`] matches) will fail the round-trip
    /// test in `#[cfg(test)]` below.
    pub const ALL: &'static [Field] = &[
        Field::City,
        Field::CountryCode,
        Field::CountryName,
        Field::Latitude,
        Field::Longitude,
        Field::Asn,
        Field::NetworkOperator,
        Field::DisplayName,
        Field::Website,
        Field::Notes,
    ];

    /// Canonical PascalCase rendering stored in `operator_edited_fields`.
    pub fn as_str(self) -> &'static str {
        match self {
            Field::City => "City",
            Field::CountryCode => "CountryCode",
            Field::CountryName => "CountryName",
            Field::Latitude => "Latitude",
            Field::Longitude => "Longitude",
            Field::Asn => "Asn",
            Field::NetworkOperator => "NetworkOperator",
            Field::DisplayName => "DisplayName",
            Field::Website => "Website",
            Field::Notes => "Notes",
        }
    }
}

/// Parse the canonical PascalCase rendering back into a [`Field`].
///
/// The inverse of [`Field::as_str`]: unknown strings return `Err(())` so
/// callers (e.g. the PATCH handler) can silently drop unrecognised field
/// names. Keep this match mirrored with [`Field::as_str`] — the
/// `as_str_and_from_str_are_inverses_for_all_variants` test fails loudly
/// if a variant is added or removed without updating both sides.
impl FromStr for Field {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "City" => Ok(Field::City),
            "CountryCode" => Ok(Field::CountryCode),
            "CountryName" => Ok(Field::CountryName),
            "Latitude" => Ok(Field::Latitude),
            "Longitude" => Ok(Field::Longitude),
            "Asn" => Ok(Field::Asn),
            "NetworkOperator" => Ok(Field::NetworkOperator),
            "DisplayName" => Ok(Field::DisplayName),
            "Website" => Ok(Field::Website),
            "Notes" => Ok(Field::Notes),
            _ => Err(()),
        }
    }
}

/// In-process representation of one `ip_catalogue` row.
#[derive(Debug, Clone)]
pub struct CatalogueEntry {
    /// Primary key.
    pub id: Uuid,
    /// Catalogued IP (host address; never a wider CIDR).
    pub ip: IpAddr,
    /// Operator-facing label; falls back to enrichment output when unset.
    pub display_name: Option<String>,
    /// City name.
    pub city: Option<String>,
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: Option<String>,
    /// Country human-readable name.
    pub country_name: Option<String>,
    /// Decimal latitude (-90..=90).
    pub latitude: Option<f64>,
    /// Decimal longitude (-180..=180).
    pub longitude: Option<f64>,
    /// Autonomous system number.
    pub asn: Option<i32>,
    /// Network operator / ISP name.
    pub network_operator: Option<String>,
    /// Optional external link.
    pub website: Option<String>,
    /// Free-form operator notes.
    pub notes: Option<String>,
    /// Current enrichment pipeline status.
    pub enrichment_status: EnrichmentStatus,
    /// Timestamp of the most recent successful enrichment run.
    pub enriched_at: Option<DateTime<Utc>>,
    /// Fields manually edited by operators / self-reported by agents;
    /// providers must skip any field listed here. Values are PascalCase
    /// [`Field::as_str`] renderings.
    pub operator_edited_fields: Vec<String>,
    /// Where this row originated.
    pub source: CatalogueSource,
    /// Row creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Optional principal string (session username) that created the row.
    pub created_by: Option<String>,
}

impl CatalogueEntry {
    /// True iff `field` is listed in `operator_edited_fields` — the
    /// enrichment runner must skip locked fields.
    pub fn is_locked(&self, field: Field) -> bool {
        self.operator_edited_fields
            .iter()
            .any(|f| f == field.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_names_match_pascal_case() {
        // The Field::as_str strings are the only authority for
        // `operator_edited_fields`. Any divergence silently breaks the
        // override rule.
        assert_eq!(Field::Latitude.as_str(), "Latitude");
        assert_eq!(Field::NetworkOperator.as_str(), "NetworkOperator");
        assert_eq!(Field::CountryCode.as_str(), "CountryCode");
        assert_eq!(Field::Asn.as_str(), "Asn");
        assert_eq!(Field::DisplayName.as_str(), "DisplayName");
    }

    #[test]
    fn as_str_and_from_str_are_inverses_for_all_variants() {
        use std::str::FromStr;
        for field in Field::ALL {
            let parsed = Field::from_str(field.as_str()).unwrap_or_else(|_| {
                panic!(
                    "as_str '{}' must parse back to the same variant",
                    field.as_str()
                )
            });
            assert_eq!(parsed, *field);
        }
        assert!(Field::from_str("Unknown").is_err());
    }

    #[test]
    fn is_locked_reads_operator_edited_fields() {
        let mut e = minimal();
        e.operator_edited_fields = vec!["Latitude".into(), "Longitude".into()];
        assert!(e.is_locked(Field::Latitude));
        assert!(e.is_locked(Field::Longitude));
        assert!(!e.is_locked(Field::City));
        e.operator_edited_fields.clear();
        assert!(!e.is_locked(Field::Latitude));
    }

    #[test]
    fn is_locked_is_case_sensitive() {
        // Accidental casing drift in the DB column must not be treated as
        // a lock — divergence is a bug we want surfaced, not silently
        // tolerated.
        let mut e = minimal();
        e.operator_edited_fields = vec!["latitude".into()];
        assert!(!e.is_locked(Field::Latitude));
    }

    fn minimal() -> CatalogueEntry {
        CatalogueEntry {
            id: Uuid::nil(),
            ip: "127.0.0.1".parse().unwrap(),
            display_name: None,
            city: None,
            country_code: None,
            country_name: None,
            latitude: None,
            longitude: None,
            asn: None,
            network_operator: None,
            website: None,
            notes: None,
            enrichment_status: EnrichmentStatus::Pending,
            enriched_at: None,
            operator_edited_fields: vec![],
            source: CatalogueSource::Operator,
            created_at: Utc::now(),
            created_by: None,
        }
    }
}
