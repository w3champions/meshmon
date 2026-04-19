//! Concrete [`EnrichmentProvider`] implementations.
//!
//! Each submodule hosts one provider. Providers are kept in separate files so
//! they can be compiled behind feature flags and tested in isolation — the
//! runner composes them via [`build_chain`] into a fixed call order.
//!
//! # Chain order
//!
//! [`build_chain`] assembles providers in the following order, skipping any
//! whose `enabled` flag is `false` or whose feature flag is off:
//!
//! 1. [`ipgeolocation::IpGeoProvider`] — paid source with the richest field
//!    coverage; preferred first-writer.
//! 2. [`rdap::RdapProvider`] — free registry metadata; fills registry-level
//!    fields the paid source did not cover.
//! 3. [`maxmind::MaxmindProvider`] (feature `enrichment-maxmind`) — local
//!    mmdb lookups; offline fallback.
//! 4. [`whois::WhoisProvider`] (feature `enrichment-whois`) — last-resort
//!    [`crate::catalogue::model::Field::NetworkOperator`] fallback.
//!
//! The runner (added in Task 10) walks the returned `Vec` with
//! [`crate::enrichment::MergedFields::apply`] so earlier providers win on
//! conflicts.

pub mod ipgeolocation;
pub mod rdap;

#[cfg(feature = "enrichment-maxmind")]
pub mod maxmind;
#[cfg(feature = "enrichment-whois")]
pub mod whois;

use super::EnrichmentProvider;
use crate::config::EnrichmentSection;
use std::sync::Arc;

/// Assemble the enrichment provider chain from the resolved config.
///
/// Providers are pushed in the declared order (ipgeolocation → rdap →
/// maxmind → whois) and each is skipped when its `enabled` flag is
/// `false` or its compile-time feature is off. Returns any permanent
/// construction error (missing API key, feature-flag mismatch, I/O
/// failure opening an mmdb file) so the service aborts boot rather
/// than silently running with a half-configured chain.
pub fn build_chain(cfg: &EnrichmentSection) -> anyhow::Result<Vec<Arc<dyn EnrichmentProvider>>> {
    let mut chain: Vec<Arc<dyn EnrichmentProvider>> = Vec::new();

    if cfg.ipgeolocation.enabled {
        let key = cfg
            .ipgeolocation
            .api_key
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ipgeolocation enabled without api_key"))?;
        let provider = ipgeolocation::IpGeoProvider::new(key)
            .map_err(|e| anyhow::anyhow!("ipgeolocation provider: {e}"))?;
        chain.push(Arc::new(provider));
    }

    if cfg.rdap.enabled {
        let provider =
            rdap::RdapProvider::new().map_err(|e| anyhow::anyhow!("rdap provider: {e}"))?;
        chain.push(Arc::new(provider));
    }

    #[cfg(feature = "enrichment-maxmind")]
    if cfg.maxmind.enabled {
        // Silently skip when paths are missing: the spec treats an
        // enabled-but-unpathed maxmind block as a benign misconfiguration
        // (operator toggled the flag before staging the mmdb files) rather
        // than a boot-blocking error. The provider is only constructed when
        // both paths are present.
        if let (Some(city), Some(asn)) = (&cfg.maxmind.city_mmdb, &cfg.maxmind.asn_mmdb) {
            let provider = maxmind::MaxmindProvider::open(city, asn)
                .map_err(|e| anyhow::anyhow!("maxmind provider: {e}"))?;
            chain.push(Arc::new(provider));
        }
    }

    #[cfg(feature = "enrichment-whois")]
    if cfg.whois.enabled {
        let provider =
            whois::WhoisProvider::new().map_err(|e| anyhow::anyhow!("whois provider: {e}"))?;
        chain.push(Arc::new(provider));
    }

    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        EnrichmentSection, IpGeolocationSection, MaxmindSection, RdapSection, WhoisSection,
    };

    fn section(
        ipgeo_enabled: bool,
        ipgeo_key: Option<&str>,
        rdap_enabled: bool,
    ) -> EnrichmentSection {
        EnrichmentSection {
            ipgeolocation: IpGeolocationSection {
                enabled: ipgeo_enabled,
                api_key: ipgeo_key.map(str::to_string),
                acknowledged_tos: ipgeo_enabled,
            },
            rdap: RdapSection {
                enabled: rdap_enabled,
            },
            maxmind: MaxmindSection::default(),
            whois: WhoisSection::default(),
        }
    }

    #[test]
    fn empty_config_produces_empty_chain() {
        // Every toggle off → no providers constructed. Useful in tests
        // and in deployments where enrichment is fully disabled.
        let cfg = section(false, None, false);
        let chain = build_chain(&cfg).expect("empty chain is infallible");
        assert!(chain.is_empty());
    }

    #[test]
    fn ipgeo_enabled_without_key_errors() {
        // Guard the combined invariant: an enabled provider missing
        // credentials must make boot fail rather than running a
        // chain that will 401 on every call.
        let cfg = section(true, None, false);
        let err = match build_chain(&cfg) {
            Ok(_) => panic!("missing key must error"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("api_key"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn chain_order_is_declared_order() {
        // Runner ordering guarantees first-writer-wins. Explicit test so
        // a refactor that shuffles the `if` blocks in build_chain fails
        // loudly rather than silently changing precedence.
        let cfg = section(true, Some("test-key"), true);
        let chain = build_chain(&cfg).expect("chain builds");
        assert_eq!(chain.len(), 2, "ipgeolocation + rdap expected");
        assert_eq!(chain[0].id(), "ipgeolocation");
        assert_eq!(chain[1].id(), "rdap");
    }

    #[cfg(feature = "enrichment-maxmind")]
    #[test]
    fn maxmind_enabled_without_paths_silently_skips() {
        // Spec contract: an enabled maxmind block with missing mmdb
        // paths is treated as a benign misconfiguration — the branch
        // skips without pushing a provider or returning an error.
        let cfg = EnrichmentSection {
            maxmind: MaxmindSection {
                enabled: true,
                city_mmdb: None,
                asn_mmdb: None,
            },
            ..EnrichmentSection::default()
        };
        let chain = build_chain(&cfg).expect("missing mmdb paths must not error");
        assert!(
            chain.iter().all(|p| p.id() != maxmind::ID),
            "maxmind provider must not be pushed when paths are missing"
        );
    }

    #[cfg(feature = "enrichment-whois")]
    #[test]
    fn whois_enabled_appears_last_in_chain() {
        // The whois provider constructor does no network I/O, so this
        // test exercises both the feature gate and the chain push.
        // Explicitly enable RDAP so this test is stable regardless of
        // the stubbed-RDAP default — the point is to lock whois's
        // ordering relative to RDAP (network operator fallback runs
        // last).
        let cfg = EnrichmentSection {
            rdap: RdapSection { enabled: true },
            whois: WhoisSection { enabled: true },
            ..EnrichmentSection::default()
        };
        let chain = build_chain(&cfg).expect("rdap+whois chain builds");
        assert_eq!(chain.len(), 2, "rdap + whois expected");
        assert_eq!(chain[0].id(), "rdap");
        assert_eq!(chain[1].id(), "whois");
    }
}
