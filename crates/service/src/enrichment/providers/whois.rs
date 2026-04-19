//! WHOIS enrichment provider (feature-gated).
//!
//! Hits a WHOIS server — ARIN by default, which auto-redirects to the
//! right RIR via the `whois_rust` follow mechanism — and extracts the
//! network operator / organisation name from the free-form response.
//! Everything in this module is gated behind the `enrichment-whois`
//! Cargo feature so the default build neither pulls in `whois-rust`
//! nor ships the provider.
//!
//! # Scope
//!
//! Per the catalogue plan (§3.2 #4) WHOIS is used as a fallback for
//! [`Field::NetworkOperator`] only — the full response contains more
//! data but the output shape is inconsistent across registries and
//! parsing the rest reliably would require per-RIR templates we do
//! not want to maintain. Other fields are left to the geo / ASN
//! providers.
//!
//! # whois-rust 1.6 API note
//!
//! The 1.6 release of `whois-rust` does not expose a bundled servers
//! JSON (no `WHOIS_SERVERS_JSON` constant), and its `lookup` method is
//! blocking (TCP sockets from `std::net`). We therefore:
//!
//! - Construct the handle via [`whois_rust::WhoIs::from_host`] pointing
//!   at `whois.arin.net`. The inner `lookup_inner` function follows
//!   `ReferralServer` / `Whois Server` response headers up to two hops
//!   by default, which is enough for ARIN to hand off to RIPE / APNIC /
//!   LACNIC / AFRINIC.
//! - Wrap each lookup in [`tokio::task::spawn_blocking`] so the async
//!   runtime never stalls on the blocking socket.
//!
//! # Blocking thread budget
//!
//! `whois_rust` 1.6 applies its socket timeout per TCP hop, and the
//! default referral follow count is 2 — so a pathological lookup hits
//! `3 × timeout` (one initial server + up to two referrals) before the
//! blocking task returns. We override the library's 60s default with
//! [`WHOIS_SOCKET_TIMEOUT`] so the worst case stays within a reasonable
//! budget for a background enrichment call.

#![cfg(feature = "enrichment-whois")]

use crate::catalogue::model::Field;
use crate::enrichment::{EnrichmentError, EnrichmentProvider, EnrichmentResult, FieldValue};
use async_trait::async_trait;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use whois_rust::{WhoIs, WhoIsLookupOptions};

/// Stable identifier for this provider — appears in logs and metrics
/// labels. Kept as a module-level constant so the
/// [`EnrichmentProvider::id`] return value and any future metric /
/// logging sites cannot drift.
pub(crate) const ID: &str = "whois";

/// Root WHOIS server used when the operator hasn't provided a server
/// list. ARIN is the canonical starting point for IPv4/IPv6 lookups —
/// other RIRs are reached via `ReferralServer` / `Whois Server`
/// response headers, which `whois_rust` follows automatically.
const DEFAULT_WHOIS_HOST: &str = "whois.arin.net";

/// Per-hop socket timeout applied to each WHOIS TCP connection.
///
/// `whois_rust` 1.6 defaults to 60s per hop and will follow up to two
/// `ReferralServer` redirects, so an unresponsive chain can stall the
/// blocking task for ~180s before it returns. We clamp the per-hop
/// timeout to 10s, which still gives a healthy registry plenty of time
/// to answer while keeping the worst case (`3 × WHOIS_SOCKET_TIMEOUT`)
/// to ~30s of blocking-pool occupancy.
const WHOIS_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Static list of fields [`WhoisProvider`] can populate. WHOIS is the
/// fallback for [`Field::NetworkOperator`] only — the rest of the
/// response shape is not reliably parsed across registries.
const SUPPORTED_FIELDS: &[Field] = &[Field::NetworkOperator];

/// WHOIS provider.
///
/// Wraps a single [`whois_rust::WhoIs`] handle shared across lookups.
/// The handle itself is cheap (a `HashMap<String, WhoIsServerValue>`
/// plus one default server value) so cloning it into each
/// [`tokio::task::spawn_blocking`] call is fine. The `Arc` keeps the
/// provider `Send + Sync + 'static` as required by the trait.
pub struct WhoisProvider {
    /// Shared WHOIS client. Cloned on every blocking call so the closure
    /// is `'static`.
    inner: Arc<WhoIs>,
}

impl WhoisProvider {
    /// Build a provider rooted at [`DEFAULT_WHOIS_HOST`].
    ///
    /// The 1.6 release of `whois-rust` does not ship a bundled TLD→server
    /// map, but we only query IPs here, so a single seed server is
    /// sufficient — the library follows `ReferralServer` /
    /// `Whois Server` hints on the response automatically.
    pub fn new() -> Result<Self, EnrichmentError> {
        let inner = WhoIs::from_host(DEFAULT_WHOIS_HOST)
            .map_err(|e| EnrichmentError::Permanent(format!("whois handle init failed: {e}")))?;
        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

#[async_trait]
impl EnrichmentProvider for WhoisProvider {
    fn id(&self) -> &'static str {
        ID
    }

    fn supported(&self) -> &'static [Field] {
        SUPPORTED_FIELDS
    }

    async fn lookup(&self, ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
        // `whois_rust::WhoIs::lookup` uses std::net sockets synchronously —
        // run it on the blocking pool so we don't stall the async runtime.
        let client = Arc::clone(&self.inner);
        let raw = tokio::task::spawn_blocking(move || {
            let mut options = WhoIsLookupOptions::from_string(ip.to_string()).map_err(|e| {
                EnrichmentError::Permanent(format!("invalid whois lookup target: {e}"))
            })?;
            // Override the library's 60s-per-hop default so a stalled
            // referral chain cannot pin a blocking thread for ~3 minutes.
            options.timeout = Some(WHOIS_SOCKET_TIMEOUT);
            client.lookup(options).map_err(|e| match e {
                // TCP-level failures (connect timeout, connection reset,
                // DNS resolution hiccup) are worth retrying — the
                // registry might just be flaky.
                whois_rust::WhoIsError::IOError(_) => EnrichmentError::Transient(e.to_string()),
                // SerdeJsonError (malformed bundled server list),
                // HostError (host-string parse failure), and MapError
                // (internal library invariant violation) are all
                // structural — retrying won't make the response
                // well-formed.
                whois_rust::WhoIsError::SerdeJsonError(_)
                | whois_rust::WhoIsError::HostError(_)
                | whois_rust::WhoIsError::MapError(_) => EnrichmentError::Permanent(e.to_string()),
            })
        })
        .await
        .map_err(|e| EnrichmentError::Transient(format!("whois join error: {e}")))??;

        let mut out = EnrichmentResult::default();
        if let Some(name) = extract_org(&raw) {
            out.fields
                .insert(Field::NetworkOperator, FieldValue::Text(name));
        }
        Ok(out)
    }
}

/// Extract the organisation / network operator from a WHOIS response.
///
/// Walks line-by-line looking for the registry-specific organisation
/// prefixes (`OrgName:` for ARIN, `org-name:` for RIPE and APNIC, and
/// `organization:` which shows up in a handful of combined deployments).
/// Returns the first match — the response is usually ordered with the
/// most specific allocation first, so first-wins is correct.
fn extract_org(raw: &str) -> Option<String> {
    const PREFIXES: [&str; 3] = ["org-name:", "orgname:", "organization:"];
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let lower = trimmed.to_ascii_lowercase();
        for prefix in PREFIXES {
            if let Some(rest) = lower.strip_prefix(prefix) {
                // Slice the trimmed line at the same byte offset as the
                // lowercase copy so we preserve the original casing in
                // the returned value (registries often capitalise
                // "Example Networks, Inc.").
                let value_start = trimmed.len() - rest.len();
                let value = trimmed[value_start..].trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_arin_orgname() {
        // ARIN responses use `OrgName:` — case-insensitive match on the
        // prefix, but the original casing survives in the value.
        let raw = "# comment\nOrgName:      Example Networks, Inc.\n";
        assert_eq!(extract_org(raw).as_deref(), Some("Example Networks, Inc."));
    }

    #[test]
    fn extracts_ripe_org_name() {
        // RIPE / APNIC use `org-name:` (hyphen). Also exercises the
        // "first match wins" path — the later `organization:` line is
        // ignored.
        let raw = "\
% This is the RIPE Database query service.
inetnum:        192.0.2.0 - 192.0.2.255
org-name:       RIPE Example Network
organization:   Second Org
";
        assert_eq!(extract_org(raw).as_deref(), Some("RIPE Example Network"));
    }

    #[test]
    fn extracts_organization_fallback() {
        // Some combined / forwarding deployments only surface the
        // `organization:` key. Make sure the fallback still fires.
        let raw = "organization: Hybrid Example GmbH\n";
        assert_eq!(extract_org(raw).as_deref(), Some("Hybrid Example GmbH"));
    }

    #[test]
    fn returns_none_when_no_org_prefix_present() {
        // No recognised prefix → None so the runner falls through to
        // the next provider rather than poisoning the field.
        let raw = "netname: UNKNOWN\ncidr: 203.0.113.0/24\n";
        assert!(extract_org(raw).is_none());
    }

    #[test]
    fn empty_value_yields_none() {
        // An empty value is a registry quirk (e.g. placeholder line) —
        // treat it as "not populated" rather than inserting an empty
        // string.
        let raw = "OrgName:\norganization: \n";
        assert!(extract_org(raw).is_none());
    }

    #[test]
    fn id_is_stable() {
        // The provider id is the join key for logs and metrics labels —
        // a silent rename would orphan dashboards.
        assert_eq!(ID, "whois");
    }

    #[test]
    fn supported_fields_network_operator_only() {
        // WHOIS is the fallback for NetworkOperator only. Keeping the
        // assertion explicit makes an accidental widening of the
        // advertised field set fail loudly.
        assert_eq!(SUPPORTED_FIELDS, &[Field::NetworkOperator]);
    }
}
