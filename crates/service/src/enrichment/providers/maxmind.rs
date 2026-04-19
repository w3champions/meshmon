//! MaxMind GeoLite2 enrichment provider (feature-gated).
//!
//! Backs the catalogue runner with local mmdb lookups against the
//! GeoLite2 City and ASN databases. Everything in this module is gated
//! behind the `enrichment-maxmind` Cargo feature so the default build
//! neither pulls in `maxminddb` nor ships the provider.
//!
//! # Database shape
//!
//! - The **City** database populates [`Field::City`], [`Field::CountryCode`],
//!   [`Field::CountryName`], [`Field::Latitude`], and [`Field::Longitude`].
//! - The **ASN** database populates [`Field::Asn`] and
//!   [`Field::NetworkOperator`] (via GeoLite2's
//!   `autonomous_system_organization`).
//!
//! Missing sub-records are tolerated — the provider emits whichever
//! fields the database knows about and leaves the rest for downstream
//! providers to fill. Lookup errors are mapped to [`EnrichmentError`]
//! so the runner's retry/backoff logic keeps working the same way it
//! does for the network-backed providers.
//!
//! # maxminddb 0.27 API note
//!
//! `Reader::lookup` returns a lightweight `LookupResult`; the actual
//! record is fetched via `result.decode::<T>()` which yields
//! `Result<Option<T>, MaxMindDbError>`. `None` means the IP is not
//! covered by the database and is treated as a clean "no data"
//! outcome — we return an empty [`EnrichmentResult`] rather than
//! [`EnrichmentError::NotFound`] so the runner still walks the rest
//! of the chain.

#![cfg(feature = "enrichment-maxmind")]

use crate::catalogue::model::Field;
use crate::enrichment::{EnrichmentError, EnrichmentProvider, EnrichmentResult, FieldValue};
use async_trait::async_trait;
use maxminddb::{geoip2, Reader};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

/// Stable identifier for this provider — appears in logs and metrics
/// labels. Kept as a module-level constant so the
/// [`EnrichmentProvider::id`] return value and any future metric /
/// logging sites cannot drift.
pub(crate) const ID: &str = "maxmind-geolite2";

/// Static list of fields [`MaxmindProvider`] can populate. Returned
/// from [`EnrichmentProvider::supported`] so the runner can skip the
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

/// MaxMind GeoLite2 provider.
///
/// Owns two [`Reader`] instances — one for the City database, one for
/// the ASN database. Both readers hold their mmdb bytes in memory
/// (`Reader<Vec<u8>>`) so lookups are purely CPU-bound and safe to run
/// from an async task without blocking the executor.
pub struct MaxmindProvider {
    /// GeoLite2-City reader, shared behind `Arc` so the provider can
    /// be cloned into multiple runner tasks without reopening the file.
    city: Arc<Reader<Vec<u8>>>,
    /// GeoLite2-ASN reader, shared behind `Arc` for the same reason as
    /// `city`.
    asn: Arc<Reader<Vec<u8>>>,
}

impl MaxmindProvider {
    /// Open both mmdb files and build a ready-to-use provider.
    ///
    /// Each file is read in full (`Reader::open_readfile`) so subsequent
    /// lookups never hit the disk. Returns any I/O or format error from
    /// either open — bubble the failure up to the runner rather than
    /// silently disabling the provider.
    pub fn open(city_path: &Path, asn_path: &Path) -> anyhow::Result<Self> {
        let city = Reader::open_readfile(city_path)?;
        let asn = Reader::open_readfile(asn_path)?;
        Ok(Self {
            city: Arc::new(city),
            asn: Arc::new(asn),
        })
    }
}

#[async_trait]
impl EnrichmentProvider for MaxmindProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn supported(&self) -> &'static [Field] {
        SUPPORTED_FIELDS
    }

    async fn lookup(&self, ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
        let mut out = EnrichmentResult::default();

        // City database: return Ok(empty) if the IP is not covered so the
        // runner still falls through to the next provider. Only surface
        // hard errors (InvalidInput, Decoding, corrupt database) as
        // Permanent — retrying against a local file cannot succeed.
        let city_result = self
            .city
            .lookup(ip)
            .map_err(|e| EnrichmentError::Permanent(format!("maxmind city lookup: {e}")))?;
        if let Some(city) = city_result
            .decode::<geoip2::City<'_>>()
            .map_err(|e| EnrichmentError::Permanent(format!("maxmind city decode: {e}")))?
        {
            if let Some(name) = city.city.names.english {
                out.fields
                    .insert(Field::City, FieldValue::Text(name.to_string()));
            }
            if let Some(code) = city.country.iso_code {
                out.fields
                    .insert(Field::CountryCode, FieldValue::Text(code.to_string()));
            }
            if let Some(name) = city.country.names.english {
                out.fields
                    .insert(Field::CountryName, FieldValue::Text(name.to_string()));
            }
            if let Some(lat) = city.location.latitude {
                out.fields.insert(Field::Latitude, FieldValue::F64(lat));
            }
            if let Some(lon) = city.location.longitude {
                out.fields.insert(Field::Longitude, FieldValue::F64(lon));
            }
        }

        // ASN database: same tolerance for uncovered IPs.
        let asn_result = self
            .asn
            .lookup(ip)
            .map_err(|e| EnrichmentError::Permanent(format!("maxmind asn lookup: {e}")))?;
        if let Some(asn) = asn_result
            .decode::<geoip2::Asn<'_>>()
            .map_err(|e| EnrichmentError::Permanent(format!("maxmind asn decode: {e}")))?
        {
            // 4-byte ASNs above `i32::MAX` are dropped rather than silently
            // wrapped to negative values — mirrors the ipgeolocation provider.
            if let Some(number) = asn
                .autonomous_system_number
                .and_then(|n| i32::try_from(n).ok())
            {
                out.fields.insert(Field::Asn, FieldValue::I32(number));
            }
            if let Some(org) = asn.autonomous_system_organization {
                out.fields
                    .insert(Field::NetworkOperator, FieldValue::Text(org.to_string()));
            }
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_fields_are_the_expected_set() {
        // Lock the advertised field set. Every entry here must also be
        // reachable from `lookup()`'s mapping code — if you widen
        // SUPPORTED_FIELDS you must also extend the match arms above
        // (and vice versa).
        let expected: std::collections::HashSet<_> = [
            Field::City,
            Field::CountryCode,
            Field::CountryName,
            Field::Latitude,
            Field::Longitude,
            Field::Asn,
            Field::NetworkOperator,
        ]
        .into_iter()
        .collect();
        let actual: std::collections::HashSet<_> = SUPPORTED_FIELDS.iter().copied().collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn id_is_stable() {
        // The provider id is the join key for logs and metrics labels —
        // a silent rename would orphan dashboards.
        assert_eq!(ID, "maxmind-geolite2");
    }

    #[test]
    fn open_rejects_missing_files() {
        // The constructor must surface the I/O error rather than
        // panicking when given a non-existent path — the runner depends
        // on this to keep boot failures visible. `MaxmindProvider` is
        // not `Debug`, so unwrap via `match` rather than `expect_err`.
        let result = MaxmindProvider::open(
            Path::new("/nonexistent/maxmind-city.mmdb"),
            Path::new("/nonexistent/maxmind-asn.mmdb"),
        );
        let err = match result {
            Ok(_) => panic!("missing files must error"),
            Err(e) => e,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("No such file")
                || msg.contains("cannot find")
                || msg.contains("not found"),
            "unexpected error message: {msg}"
        );
    }
}
