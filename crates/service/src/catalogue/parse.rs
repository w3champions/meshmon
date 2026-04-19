//! IP-paste tokeniser.
//!
//! Accepts any common delimiter (comma, whitespace, newline) and returns
//! parsed `IpAddr`s plus a vector of rejected tokens with their reason.
//! CIDR ranges wider than `/32` (v4) or `/128` (v6) are rejected — the
//! catalogue is a per-host registry, not a range store.

use sqlx::types::ipnetwork::IpNetwork;
use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;

/// Reason a pasted token was rejected.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseReason {
    /// Token did not parse as an IP address or CIDR literal.
    InvalidIp(String),
    /// Token parsed as a CIDR wider than `/32` (v4) or `/128` (v6).
    CidrNotAllowed {
        /// Prefix length the operator supplied (e.g. `24`).
        prefix_len: u8,
    },
}

/// Outcome of parsing a paste payload.
#[derive(Debug)]
pub struct ParseOutcome {
    /// Unique, order-stable accepted IPs (sorted).
    pub accepted: Vec<IpAddr>,
    /// Rejected raw tokens with their reason.
    pub rejected: Vec<(String, ParseReason)>,
    /// Intra-paste duplicate counts, populated only for IPs that appeared
    /// more than once. The UI renders these as "dupe ×N" badges.
    pub duplicates: Vec<(IpAddr, usize)>,
}

/// Split an operator-pasted blob into accepted IPs and rejection reasons.
pub fn parse_ip_tokens(input: &str) -> ParseOutcome {
    let mut accepted_set: BTreeSet<IpAddr> = BTreeSet::new();
    let mut seen: HashMap<IpAddr, usize> = HashMap::new();
    let mut rejected: Vec<(String, ParseReason)> = Vec::new();

    for raw in input.split(|c: char| c == ',' || c.is_whitespace()) {
        let token = raw.trim();
        if token.is_empty() {
            continue;
        }
        match token.parse::<IpNetwork>() {
            Ok(net) => {
                let host_prefix = match net {
                    IpNetwork::V4(_) => 32,
                    IpNetwork::V6(_) => 128,
                };
                if net.prefix() != host_prefix {
                    rejected.push((
                        token.to_string(),
                        ParseReason::CidrNotAllowed {
                            prefix_len: net.prefix(),
                        },
                    ));
                } else {
                    let ip = net.ip();
                    *seen.entry(ip).or_insert(0) += 1;
                    accepted_set.insert(ip);
                }
            }
            Err(_) => rejected.push((token.to_string(), ParseReason::InvalidIp(token.to_string()))),
        }
    }

    let duplicates = seen
        .into_iter()
        .filter_map(|(ip, n)| if n > 1 { Some((ip, n)) } else { None })
        .collect();

    ParseOutcome {
        accepted: accepted_set.into_iter().collect(),
        rejected,
        duplicates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bare_ips_in_any_delimiter() {
        let out = parse_ip_tokens("1.1.1.1, 2.2.2.2\n3.3.3.3\t4.4.4.4");
        assert_eq!(out.accepted.len(), 4);
        assert!(out.rejected.is_empty());
    }

    #[test]
    fn accepts_host_cidrs_as_bare() {
        let out = parse_ip_tokens("5.6.7.8/32  ::1/128");
        assert_eq!(out.accepted.len(), 2);
        assert!(out.rejected.is_empty());
    }

    #[test]
    fn rejects_wider_cidrs() {
        let out = parse_ip_tokens("10.0.0.0/24");
        assert_eq!(out.accepted.len(), 0);
        assert!(matches!(
            out.rejected[0].1,
            ParseReason::CidrNotAllowed { prefix_len: 24 }
        ));
    }

    #[test]
    fn rejects_garbage() {
        let out = parse_ip_tokens("not-an-ip");
        assert_eq!(out.accepted.len(), 0);
        assert!(matches!(out.rejected[0].1, ParseReason::InvalidIp(_)));
    }

    #[test]
    fn collapses_duplicates_and_reports_counts() {
        let out = parse_ip_tokens("1.1.1.1 1.1.1.1 1.1.1.1 2.2.2.2");
        assert_eq!(out.accepted.len(), 2);
        // Find the dupe entry for 1.1.1.1; order is HashMap-dependent.
        let dup = out
            .duplicates
            .iter()
            .find(|(ip, _)| ip.to_string() == "1.1.1.1")
            .expect("1.1.1.1 should be reported as duplicate");
        assert_eq!(dup.1, 3);
    }

    #[test]
    fn ignores_empty_between_delimiters() {
        let out = parse_ip_tokens(",, ,\n\n1.2.3.4,\t");
        assert_eq!(out.accepted.len(), 1);
        assert!(out.rejected.is_empty());
    }

    #[test]
    fn mixes_accepted_and_rejected() {
        let out = parse_ip_tokens("1.1.1.1 garbage 10.0.0.0/8 ::1");
        assert_eq!(out.accepted.len(), 2);
        assert_eq!(out.rejected.len(), 2);
    }
}
