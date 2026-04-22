//! Reverse-DNS resolver abstraction plus the production `hickory` backend.
//!
//! The [`ResolverBackend`] trait exists so tests can plug in deterministic
//! fakes without standing up real DNS. [`HickoryBackend`] is the production
//! implementation: it wraps [`hickory_resolver::TokioResolver`] and maps
//! hickory's error taxonomy onto the three cache-relevant outcomes the rest
//! of the service cares about.
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use hickory_resolver::config::{ResolverConfig, ServerGroup};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::proto::rr::RData;
use hickory_resolver::TokioResolver;

/// Outcome of a single reverse-DNS lookup, collapsed to the three shapes the
/// cache distinguishes between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupOutcome {
    /// A PTR record was returned — inner value is the hostname with the
    /// trailing dot stripped.
    Positive(String),
    /// The authoritative answer was NXDOMAIN or "no records found" — cache
    /// as a confirmed negative so we don't re-query for 90 days.
    NegativeNxDomain,
    /// Timeout, SERVFAIL, connection failure, or any other error that
    /// should be retried on the next resolution attempt. Includes
    /// `NetError::Timeout`, `NoConnections`, and transport-layer errors.
    /// Never written to the cache.
    Transient(String),
}

/// Abstraction over reverse-DNS resolution so the resolver task can be
/// exercised with deterministic fakes in tests.
#[async_trait]
pub trait ResolverBackend: Send + Sync + 'static {
    /// Issue a reverse-DNS (PTR) lookup for `ip`. Returns a `LookupOutcome`
    /// indicating whether a hostname was found, the IP is confirmed
    /// unresolvable (NXDOMAIN / no records), or the lookup should be
    /// retried (transient failure).
    async fn reverse_lookup(&self, ip: IpAddr) -> LookupOutcome;
}

/// Production reverse-DNS backend backed by `hickory-resolver`.
pub struct HickoryBackend {
    inner: TokioResolver,
}

impl HickoryBackend {
    /// Construct a backend using the supplied upstream DNS servers. If
    /// `upstreams` is empty the system resolver configuration
    /// (`/etc/resolv.conf` on Unix) is used.
    pub fn new(upstreams: &[IpAddr], timeout: Duration) -> anyhow::Result<Arc<Self>> {
        let mut builder = if upstreams.is_empty() {
            TokioResolver::builder_tokio()?
        } else {
            // server_name is SNI for DNS-over-TLS; ignored for UDP/TCP which is
            // what we use. If DoT upstreams are ever needed, promote to a
            // parameter.
            let group = ServerGroup {
                ips: upstreams,
                server_name: "meshmon-hostname",
                path: "/",
            };
            let config = ResolverConfig::udp_and_tcp(&group);
            TokioResolver::builder_with_config(config, TokioRuntimeProvider::default())
        };

        // ResolverOpts is #[non_exhaustive] — mutate via options_mut().
        let opts = builder.options_mut();
        opts.timeout = timeout;
        opts.attempts = 2;
        opts.edns0 = true;

        let inner = builder.build()?;
        Ok(Arc::new(Self { inner }))
    }
}

#[async_trait]
impl ResolverBackend for HickoryBackend {
    async fn reverse_lookup(&self, ip: IpAddr) -> LookupOutcome {
        match self.inner.reverse_lookup(ip).await {
            Ok(lookup) => {
                for record in lookup.answers() {
                    if let RData::PTR(ptr) = &record.data {
                        // DNS allows multiple PTR records per IP; the first is taken as the
                        // canonical hostname (same behaviour as `dig` and most resolvers).
                        let raw = ptr.0.to_string();
                        let trimmed = raw.trim_end_matches('.');
                        if trimmed.is_empty() {
                            // A PTR record whose name is only `.` (DNS root) trims to empty;
                            // treat as a confirmed negative — this is structurally equivalent
                            // to "no usable hostname".
                            return LookupOutcome::NegativeNxDomain;
                        }
                        return LookupOutcome::Positive(trimmed.to_string());
                    }
                }
                LookupOutcome::NegativeNxDomain
            }
            Err(err) => {
                if err.is_nx_domain() || err.is_no_records_found() {
                    LookupOutcome::NegativeNxDomain
                } else {
                    LookupOutcome::Transient(err.to_string())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hickory_backend_constructs_against_system_config() {
        let backend = HickoryBackend::new(&[], Duration::from_secs(2));
        assert!(
            backend.is_ok(),
            "system resolver config should build: {:?}",
            backend.err()
        );
    }

    #[tokio::test]
    async fn hickory_backend_constructs_with_custom_upstreams() {
        let upstreams = vec![
            IpAddr::V4(std::net::Ipv4Addr::new(1, 1, 1, 1)),
            IpAddr::V4(std::net::Ipv4Addr::new(8, 8, 8, 8)),
        ];
        let backend = HickoryBackend::new(&upstreams, Duration::from_secs(2));
        assert!(
            backend.is_ok(),
            "custom-upstream config should build: {:?}",
            backend.err()
        );
    }
}
