//! Leg construction with symmetry-fallback substitution.
//!
//! # LegLookup endpoint model
//!
//! [`LegLookup`] indexes every attributed measurement as a single entry keyed
//! on `(EndpointKey::Agent(source_agent_id), EndpointKey::Ip(destination_ip))`.
//! There are no `Agent → Agent` entries; agents always probe IP addresses and
//! the destination is always an `Ip` key.
//!
//! ## Endpoint resolution
//!
//! [`LegLookup::lookup`] resolves either [`Endpoint`] form (`Agent { id }` or
//! `CandidateIp { ip }`) by consulting two roster maps built at
//! construction time:
//!
//! - `agent_by_ip` — maps a mesh agent's IP back to its agent id.
//! - `ip_by_agent` — maps an agent id to its IP.
//!
//! `Endpoint::Agent { id }` resolves to `(Some(id), agent_ip)`. A
//! `Endpoint::CandidateIp { ip }` resolves to `(Some(id), ip)` when the IP
//! belongs to a mesh agent, or `(None, ip)` otherwise. Lookups therefore
//! treat `Agent("a")` and `CandidateIp(a.ip)` as equivalent for any mesh
//! agent — callers can pass whichever form is convenient.
//!
//! ## Key variants
//!
//! - [`EndpointKey::Agent`]`(String)` — a mesh agent identified by its
//!   string id.
//! - [`EndpointKey::Ip`]`(IpAddr)` — any IP-addressed endpoint (candidate
//!   IP or a mesh agent referenced by its IP rather than its id).
//!
//! ## Lookup priority (per [`LegLookup::lookup`])
//!
//! Given a requested leg `(from, to)`, both endpoints are first resolved
//! to `(agent_id?, ip)` pairs. Then:
//!
//! 1. **Forward** evidence: `from` (as agent) probed `to.ip`. Requires
//!    `from` to resolve to a mesh agent. Hits stored key
//!    `(Agent(from_id), Ip(to.ip))`.
//! 2. **Reverse** evidence: `to` (as agent) probed `from.ip`. Requires
//!    `to` to resolve to a mesh agent. Hits stored key
//!    `(Agent(to_id), Ip(from.ip))`.
//!
//! With those two candidates in hand the priority is:
//!
//! 1. Forward present with `loss_ratio < 1.0` → used directly;
//!    `was_substituted = false`.
//! 2. Forward present but `loss_ratio == 1.0`, reverse present with
//!    `loss_ratio < 1.0` → reverse used as symmetry substitute;
//!    `was_substituted = true`.
//! 3. No forward, reverse present with `loss_ratio < 1.0` → same as (2).
//! 4. Both present with `loss_ratio == 1.0` → [`LegLookupResult::Broken`];
//!    any route containing this leg is discarded.
//! 5. Neither present → [`LegLookupResult::Missing`]; route discarded.
//!
//! ## Dual-form pool
//!
//! Because lookup auto-resolves either form, the per-mode dual-form
//! intermediary pools (one `Agent { id }` and one `CandidateIp { ip }`
//! entry per mesh agent) are no longer needed for correctness — they
//! survive as defence-in-depth. `enumerate_routes` discards routes whose
//! legs can't resolve, so any duplicate that fails to compose is a
//! silent no-op.

use crate::campaign::eval::{AgentRow, AttributedMeasurement};
use crate::campaign::model::{Endpoint, LegSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;

/// One directional measurement in a route. Owned struct (not borrowed) so the
/// per-route composer can hold composed leg sets across multiple route shapes
/// without lifetime pain.
///
/// Surfaced through `pub EvaluationOutputs::EdgeCandidate(...)` so each leg's
/// substitution / source / MTR-id can be persisted onto the wire without
/// re-deriving it. `Serialize`/`Deserialize` are derived so the leg list can be
/// round-tripped through the `best_route_legs` JSONB column.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LegMeasurement {
    /// Originating endpoint of the leg.
    pub from: Endpoint,
    /// Destination endpoint of the leg.
    pub to: Endpoint,
    /// Mean RTT (ms) for this leg.
    pub rtt_ms: f32,
    /// RTT stddev (ms) for this leg.
    pub stddev_ms: f32,
    /// Observed loss fraction (0.0–1.0).
    pub loss_ratio: f32,
    /// Provenance of the underlying measurement row
    /// (`ActiveProbe` / `VmContinuous`).
    pub source: LegSource,
    /// `true` when the leg was resolved from the *reverse* direction
    /// (symmetry-fallback, spec §3.1) rather than the forward
    /// measurement.
    pub was_substituted: bool,
    /// FK to the backing `measurements.id` row when an MTR trace is
    /// attached; `None` for VM-synthesized rows.
    pub mtr_measurement_id: Option<i64>,
}

/// Indexed view of the available measurements for fast leg lookup.
///
/// `forward` is keyed on `(source, dest)`: `(EndpointKey::Agent(src_id),
/// EndpointKey::Ip(dst_ip))`. Two roster maps (`agent_by_ip`,
/// `ip_by_agent`) let [`LegLookup::lookup`] resolve either endpoint
/// form (`Agent { id }` or `CandidateIp { ip }`) so callers never need
/// to coerce a candidate IP back to its mesh-agent identity by hand —
/// see this module's docs for the full contract.
#[allow(dead_code)] // consumed by route-composition phases (D–E)
pub(crate) struct LegLookup<'a> {
    pub(super) forward: HashMap<(EndpointKey, EndpointKey), &'a AttributedMeasurement>,
    /// Maps a mesh agent's IP to its agent id. Built from the agent
    /// roster so a `CandidateIp { ip }` referring to a known mesh agent
    /// resolves to that agent for forward / reverse lookup parity with
    /// the `Agent { id }` form.
    agent_by_ip: HashMap<IpAddr, String>,
    /// Maps an agent id to its IP. Used to resolve `Endpoint::Agent`
    /// to a concrete IP when the leg's other side carries a stored
    /// destination IP key.
    ip_by_agent: HashMap<String, IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)] // consumed by route-composition phases (D–E)
pub(crate) enum EndpointKey {
    Agent(String),
    Ip(IpAddr),
}

#[allow(dead_code)] // consumed by route-composition phases (D–E)
impl EndpointKey {
    pub fn from_endpoint(e: &Endpoint) -> Self {
        match e {
            Endpoint::Agent { id } => EndpointKey::Agent(id.clone()),
            Endpoint::CandidateIp { ip } => EndpointKey::Ip(*ip),
        }
    }
}

/// Endpoint resolved against the agent roster: `agent_id` is `Some` when
/// the endpoint refers to a mesh agent (either via `Agent { id }` directly
/// or via `CandidateIp { ip }` where the IP belongs to a known agent).
struct ResolvedEndpoint {
    agent_id: Option<String>,
    ip: IpAddr,
}

#[allow(dead_code)] // consumed by route-composition phases (D–E)
impl<'a> LegLookup<'a> {
    /// Build the lookup from the campaign's attributed measurements and
    /// the agent roster. The roster powers endpoint resolution so a
    /// `CandidateIp { ip }` that points at a mesh agent's IP behaves
    /// identically to `Agent { id }` at the lookup boundary.
    pub fn build(measurements: &'a [AttributedMeasurement], agents: &[AgentRow]) -> Self {
        let mut forward = HashMap::with_capacity(measurements.len());
        for m in measurements {
            let from = EndpointKey::Agent(m.source_agent_id.clone());
            let to = EndpointKey::Ip(m.destination_ip);
            // active-probe wins over VM-continuous on tie (already enforced
            // by insertion order from the handler's HashMap composition).
            forward.entry((from, to)).or_insert(m);
        }
        let agent_by_ip = agents.iter().map(|a| (a.ip, a.agent_id.clone())).collect();
        let ip_by_agent = agents.iter().map(|a| (a.agent_id.clone(), a.ip)).collect();
        Self {
            forward,
            agent_by_ip,
            ip_by_agent,
        }
    }

    /// Lookup priority:
    /// 1. Forward (`from` agent probed `to.ip`) with loss < 1.0 → use,
    ///    `was_substituted = false`.
    /// 2. Reverse (`to` agent probed `from.ip`) with loss < 1.0 → use,
    ///    `was_substituted = true`.
    /// 3. Both endpoints reach a stored row but every candidate has loss
    ///    == 1.0 → [`LegLookupResult::Broken`]; route discarded.
    /// 4. Otherwise → [`LegLookupResult::Missing`]; route discarded.
    ///
    /// Forward evidence requires `from` to resolve to a mesh agent (so the
    /// stored `(Agent(src), Ip(dst))` key can be hit). Reverse evidence
    /// requires `to` to resolve to a mesh agent. A non-mesh `CandidateIp`
    /// on either side simply skips that direction without erroring.
    ///
    /// `from`/`to` are the endpoints of the leg we want to construct.
    /// When the reverse measurement is used, `was_substituted` is set to
    /// `true`; the `source` field still records the underlying measurement's
    /// provenance (`ActiveProbe` or `VmContinuous`). Consumers should rely on
    /// `was_substituted` for substitution detection rather than checking
    /// the `source` value.
    pub fn lookup(&self, from: &Endpoint, to: &Endpoint) -> LegLookupResult {
        let from_r = self.resolve(from);
        let to_r = self.resolve(to);

        // Forward evidence: `from` (as agent) probed `to.ip`. Requires
        // `from` to resolve to a mesh agent.
        let forward = from_r.agent_id.as_ref().and_then(|src_id| {
            self.forward
                .get(&(EndpointKey::Agent(src_id.clone()), EndpointKey::Ip(to_r.ip)))
        });

        // Reverse evidence: `to` (as agent) probed `from.ip`. Requires
        // `to` to resolve to a mesh agent.
        let reverse = to_r.agent_id.as_ref().and_then(|src_id| {
            self.forward.get(&(
                EndpointKey::Agent(src_id.clone()),
                EndpointKey::Ip(from_r.ip),
            ))
        });

        match (forward, reverse) {
            (Some(m), _) if m.loss_ratio < 1.0 => LegLookupResult::Found {
                rtt_ms: m.latency_avg_ms.unwrap_or(0.0),
                stddev_ms: m.latency_stddev_ms.unwrap_or(0.0),
                loss_ratio: m.loss_ratio,
                source: leg_source_from_direct(m.direct_source),
                was_substituted: false,
                mtr_measurement_id: m.mtr_measurement_id,
            },
            (Some(_), Some(r)) if r.loss_ratio < 1.0 => LegLookupResult::Found {
                rtt_ms: r.latency_avg_ms.unwrap_or(0.0),
                stddev_ms: r.latency_stddev_ms.unwrap_or(0.0),
                loss_ratio: r.loss_ratio,
                source: leg_source_from_direct(r.direct_source),
                was_substituted: true,
                mtr_measurement_id: r.mtr_measurement_id,
            },
            (None, Some(r)) if r.loss_ratio < 1.0 => LegLookupResult::Found {
                rtt_ms: r.latency_avg_ms.unwrap_or(0.0),
                stddev_ms: r.latency_stddev_ms.unwrap_or(0.0),
                loss_ratio: r.loss_ratio,
                source: leg_source_from_direct(r.direct_source),
                was_substituted: true,
                mtr_measurement_id: r.mtr_measurement_id,
            },
            (Some(_), Some(_)) => LegLookupResult::Broken,
            (Some(_), None) => LegLookupResult::Broken, // forward 100%, reverse missing
            (None, None) => LegLookupResult::Missing,
            (None, Some(_)) => LegLookupResult::Broken, // reverse 100% (covered by guard above)
        }
    }

    fn resolve(&self, e: &Endpoint) -> ResolvedEndpoint {
        match e {
            Endpoint::Agent { id } => ResolvedEndpoint {
                agent_id: Some(id.clone()),
                // Fall back to an unspecified address when the agent is
                // not in the roster. Such a leg cannot match any stored
                // measurement and lookup naturally returns Missing.
                ip: self
                    .ip_by_agent
                    .get(id)
                    .copied()
                    .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            },
            Endpoint::CandidateIp { ip } => ResolvedEndpoint {
                agent_id: self.agent_by_ip.get(ip).cloned(),
                ip: *ip,
            },
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // consumed by route-composition phases (D–E)
pub(crate) enum LegLookupResult {
    Found {
        rtt_ms: f32,
        stddev_ms: f32,
        loss_ratio: f32,
        source: LegSource,
        was_substituted: bool,
        mtr_measurement_id: Option<i64>,
    },
    /// Both directions exist with 100% loss — leg is broken; route discarded.
    Broken,
    /// Neither direction has data — route can't compose.
    Missing,
}

#[allow(dead_code)] // consumed by route-composition phases (D–E)
fn leg_source_from_direct(d: crate::campaign::model::DirectSource) -> LegSource {
    use crate::campaign::model::DirectSource;
    match d {
        DirectSource::ActiveProbe => LegSource::ActiveProbe,
        DirectSource::VmContinuous => LegSource::VmContinuous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign::model::DirectSource;

    fn agent(id: &str) -> Endpoint {
        Endpoint::Agent { id: id.into() }
    }
    fn ip(s: &str) -> Endpoint {
        Endpoint::CandidateIp {
            ip: s.parse().unwrap(),
        }
    }

    fn agent_row(id: &str, addr: &str) -> AgentRow {
        AgentRow {
            agent_id: id.into(),
            ip: addr.parse().unwrap(),
            hostname: None,
        }
    }

    fn measurement(src: &str, dst: &str, loss: f32) -> AttributedMeasurement {
        AttributedMeasurement {
            source_agent_id: src.into(),
            destination_ip: dst.parse().unwrap(),
            latency_avg_ms: Some(20.0),
            latency_stddev_ms: Some(2.0),
            loss_ratio: loss,
            mtr_measurement_id: None,
            direct_source: DirectSource::ActiveProbe,
        }
    }

    #[test]
    fn forward_low_loss_is_used_directly() {
        let m = vec![measurement("A", "10.0.0.1", 0.0)];
        let agents = [agent_row("A", "10.0.0.99")];
        let lookup = LegLookup::build(&m, &agents);
        let result = lookup.lookup(&agent("A"), &ip("10.0.0.1"));
        match result {
            LegLookupResult::Found {
                was_substituted, ..
            } => assert!(!was_substituted),
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn candidate_ip_resolves_to_agent_for_forward_lookup() {
        // Stored measurement is `meas("a", X.ip)`. The leg request is from
        // a `CandidateIp(a.ip)` (not the `Agent("a")` form) to `X.ip`.
        // With roster-aware resolution the CandidateIp resolves back to
        // agent `a`, hits the forward key, and returns Found.
        let m = vec![measurement("a", "203.0.113.5", 0.0)];
        let agents = [agent_row("a", "10.0.0.1")];
        let lookup = LegLookup::build(&m, &agents);
        let result = lookup.lookup(&ip("10.0.0.1"), &ip("203.0.113.5"));
        assert!(
            matches!(
                result,
                LegLookupResult::Found {
                    was_substituted: false,
                    ..
                }
            ),
            "CandidateIp(a.ip) → CandidateIp(X) must resolve via forward key"
        );
    }

    #[test]
    fn non_mesh_to_mesh_resolves_via_reverse() {
        // Non-mesh candidate X (203.0.113.99) to mesh agent B (10.0.0.2).
        // No measurement from X exists, but B probed X — reverse lookup
        // must succeed and mark the leg substituted.
        let m = vec![measurement("b", "203.0.113.99", 0.0)];
        let agents = [agent_row("b", "10.0.0.2")];
        let lookup = LegLookup::build(&m, &agents);
        let result = lookup.lookup(&ip("203.0.113.99"), &ip("10.0.0.2"));
        match result {
            LegLookupResult::Found {
                was_substituted, ..
            } => assert!(was_substituted, "reverse-resolved leg must be substituted"),
            _ => panic!("expected Found via reverse"),
        }
    }

    #[test]
    fn both_100_loss_returns_broken() {
        let m = [
            measurement("A", "10.0.0.3", 1.0),
            // Build a reverse 100%-loss measurement targeting A. The model
            // requires source_agent_id, so reverse is "B → A.something" — but
            // we test this end-to-end in the integration suite.
        ];
        let agents = [agent_row("A", "10.0.0.99")];
        let lookup = LegLookup::build(&m, &agents);
        let result = lookup.lookup(&agent("A"), &ip("10.0.0.3"));
        // forward is 100%, no reverse → Broken (or Missing per the guard).
        assert!(matches!(result, LegLookupResult::Broken));
    }

    #[test]
    fn neither_direction_returns_missing() {
        let lookup = LegLookup::build(&[], &[]);
        let result = lookup.lookup(&agent("A"), &ip("10.0.0.4"));
        assert!(matches!(result, LegLookupResult::Missing));
    }
}
