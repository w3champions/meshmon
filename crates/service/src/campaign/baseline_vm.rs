//! Fetch-and-archive agent-to-agent baselines from VictoriaMetrics.
//!
//! The evaluator no longer requires active campaign probes between
//! agents. Before every `/evaluate` call, the handler calls
//! [`fetch_and_archive_vm_baselines`] which:
//!
//! 1. Queries VM for the campaign's agent-mesh baselines over a short
//!    lookback window (see [`DEFAULT_BASELINE_LOOKBACK`]).
//! 2. Archives every fetched `(source, target)` sample as a
//!    `measurements` row tagged `source = 'archived_vm_continuous'`.
//! 3. Upserts the matching `campaign_pairs` row so the evaluator's
//!    existing join-by-measurement-id machinery surfaces the baseline.
//!
//! Re-evaluation is idempotent: the upsert replaces the previous
//! archival row's `measurement_id` pointer so the evaluator sees the
//! freshest baseline.

use super::eval::AgentRow;
use super::events::PAIR_SETTLED_CHANNEL;
use super::model::ProbeProtocol;
use crate::vm_query::{self, AgentBaselineSample, VmQueryError};
use sqlx::{types::ipnetwork::IpNetwork, PgPool};
use std::collections::HashSet;
use std::net::IpAddr;
use std::time::Duration;
use uuid::Uuid;

/// Default lookback window for the VM baseline fetch. 15 minutes is long
/// enough to absorb the continuous prober's 60 s round cadence and any
/// brief mesh-side blips, short enough to stay responsive when an
/// operator reshapes the agent fleet.
pub const DEFAULT_BASELINE_LOOKBACK: Duration = Duration::from_secs(15 * 60);

/// Outcome counters surfaced to the handler for logging.
#[derive(Debug, Clone, Copy, Default)]
pub struct BaselineArchiveOutcome {
    /// Number of `(source, target)` samples VM returned (after merging
    /// per-metric result sets and dropping self-pairs).
    pub pairs_fetched: usize,
    /// Number of rows the archive path actually persisted. Equals
    /// `pairs_fetched` minus any pairs whose source/target didn't map
    /// back to an agent in the roster or lacked a usable RTT value.
    pub pairs_archived: usize,
    /// Pairs VM surfaced that we skipped because the roster didn't
    /// include their source or target agent (shouldn't usually happen,
    /// but VM can carry stale series).
    pub pairs_skipped_no_vm_data: usize,
}

/// Errors surfaced by [`fetch_and_archive_vm_baselines`].
#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    /// Upstream VM read failed. The handler maps this to 503 so
    /// operators know the fetch didn't complete.
    #[error("VictoriaMetrics baseline fetch failed: {0}")]
    Vm(#[from] VmQueryError),
    /// Underlying sqlx failure (connection, deadlock, constraint, etc.).
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Short label used by the evaluator's [`ProbeProtocol`] for the
/// `protocol` metric label.
fn protocol_label(p: ProbeProtocol) -> &'static str {
    match p {
        ProbeProtocol::Icmp => "icmp",
        ProbeProtocol::Tcp => "tcp",
        ProbeProtocol::Udp => "udp",
    }
}

/// Fetch agent-to-agent baselines from VictoriaMetrics and archive them
/// into `measurements` (source = 'archived_vm_continuous'). Upserts
/// `campaign_pairs` entries for each fetched pair so the evaluator's
/// existing join-by-measurement-id machinery finds the baseline rows.
///
/// Called from the evaluate endpoint before `measurements_for_campaign`
/// so the evaluator input naturally includes baselines alongside
/// active-probe measurements.
///
/// All writes happen in one transaction. Partial success is never
/// persisted; a failure rolls the whole fetch back so the evaluator
/// either sees a consistent baseline set or none at all.
pub async fn fetch_and_archive_vm_baselines(
    pool: &PgPool,
    vm_url: &str,
    campaign_id: Uuid,
    campaign_protocol: ProbeProtocol,
    agents: &[AgentRow],
    lookback: Duration,
) -> Result<BaselineArchiveOutcome, BaselineError> {
    let agent_ids: Vec<String> = agents.iter().map(|a| a.agent_id.clone()).collect();
    if agent_ids.len() < 2 {
        // A mesh with 0 or 1 agent has no agent-to-agent pairs by
        // definition; skip the VM round-trip entirely.
        return Ok(BaselineArchiveOutcome::default());
    }

    let samples = vm_query::fetch_agent_baselines(
        vm_url,
        &agent_ids,
        protocol_label(campaign_protocol),
        lookback,
    )
    .await?;

    archive_samples(pool, campaign_id, campaign_protocol, agents, &samples).await
}

/// Archive an already-fetched sample set. Split out from the VM fetch
/// so unit tests can cover the DB side without a live VM (see module
/// tests).
pub async fn archive_samples(
    pool: &PgPool,
    campaign_id: Uuid,
    campaign_protocol: ProbeProtocol,
    agents: &[AgentRow],
    samples: &[AgentBaselineSample],
) -> Result<BaselineArchiveOutcome, BaselineError> {
    let mut outcome = BaselineArchiveOutcome {
        pairs_fetched: samples.len(),
        ..Default::default()
    };

    if samples.is_empty() {
        return Ok(outcome);
    }

    let mut ip_by_id = std::collections::HashMap::with_capacity(agents.len());
    for a in agents {
        ip_by_id.insert(a.agent_id.as_str(), a.ip);
    }

    let mut tx = pool.begin().await?;
    let mut notified_campaigns: HashSet<Uuid> = HashSet::new();

    for sample in samples {
        let Some(rtt_ms) = sample.latency_avg_ms else {
            outcome.pairs_skipped_no_vm_data += 1;
            continue;
        };
        let Some(source_ip) = ip_by_id.get(sample.source_agent_id.as_str()).copied() else {
            outcome.pairs_skipped_no_vm_data += 1;
            continue;
        };
        let Some(target_ip) = ip_by_id.get(sample.target_agent_id.as_str()).copied() else {
            outcome.pairs_skipped_no_vm_data += 1;
            continue;
        };
        if source_ip == target_ip {
            // Self-loop guard — shouldn't happen (vm_query drops these)
            // but stay defensive.
            outcome.pairs_skipped_no_vm_data += 1;
            continue;
        }

        let m_id = insert_archival_measurement(
            &mut tx,
            &sample.source_agent_id,
            target_ip,
            campaign_protocol,
            rtt_ms,
            sample.latency_stddev_ms,
            sample.loss_ratio.unwrap_or(0.0),
        )
        .await?;

        upsert_campaign_pair(
            &mut tx,
            campaign_id,
            &sample.source_agent_id,
            target_ip,
            m_id,
        )
        .await?;

        notified_campaigns.insert(campaign_id);
        outcome.pairs_archived += 1;
    }

    // Fire the `campaign_pair_settled` NOTIFY inside the same
    // transaction so the scheduler's LISTEN loop picks up the new
    // archival rows consistently with how real settles signal.
    for cid in notified_campaigns {
        sqlx::query!(
            "SELECT pg_notify($1, $2::text) AS _notified",
            PAIR_SETTLED_CHANNEL,
            cid.to_string(),
        )
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(outcome)
}

/// Insert one archival `measurements` row. Returns the new row id.
async fn insert_archival_measurement(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    source_agent_id: &str,
    destination_ip: IpAddr,
    protocol: ProbeProtocol,
    latency_avg_ms: f32,
    latency_stddev_ms: Option<f32>,
    loss_ratio: f32,
) -> Result<i64, sqlx::Error> {
    let destination = IpNetwork::from(destination_ip);
    // `probe_count = 0` because VM data is aggregated over continuous
    // mesh probing — no single discrete probe count applies. The reuse
    // lookup in `resolve_reuse` orders by `probe_count DESC` and
    // filters on `source = 'active_probe'` (not source here — see the
    // lookup's `latency_avg_ms IS NOT NULL` gate), so a 0 here does
    // not undercut reuse.
    sqlx::query_scalar!(
        r#"
        INSERT INTO measurements
            (source_agent_id, destination_ip, protocol, probe_count,
             latency_avg_ms, latency_stddev_ms, loss_ratio,
             kind, source)
        VALUES ($1, $2, $3::probe_protocol, 0,
                $4, $5, $6,
                'campaign'::measurement_kind,
                'archived_vm_continuous'::measurement_source)
        RETURNING id
        "#,
        source_agent_id,
        destination,
        protocol as ProbeProtocol,
        latency_avg_ms,
        latency_stddev_ms,
        loss_ratio,
    )
    .fetch_one(&mut **tx)
    .await
}

/// Upsert the `campaign_pairs` row pointing at the new archival
/// measurement. The uniqueness key is `(campaign_id, source_agent_id,
/// destination_ip, kind)`, so the upsert replaces only the baseline
/// (`kind='campaign'`) row — detail rows stay untouched.
async fn upsert_campaign_pair(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    campaign_id: Uuid,
    source_agent_id: &str,
    destination_ip: IpAddr,
    measurement_id: i64,
) -> Result<(), sqlx::Error> {
    let destination = IpNetwork::from(destination_ip);
    sqlx::query!(
        r#"
        INSERT INTO campaign_pairs
            (campaign_id, source_agent_id, destination_ip,
             resolution_state, measurement_id, kind,
             settled_at, dispatched_at, attempt_count)
        VALUES ($1, $2, $3,
                'succeeded'::pair_resolution_state, $4,
                'campaign'::measurement_kind,
                now(), NULL, 0)
        ON CONFLICT (campaign_id, source_agent_id, destination_ip, kind)
        DO UPDATE SET
            resolution_state = 'succeeded'::pair_resolution_state,
            measurement_id   = EXCLUDED.measurement_id,
            settled_at       = now(),
            dispatched_at    = NULL,
            last_error       = NULL
        "#,
        campaign_id,
        source_agent_id,
        destination,
        measurement_id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_label_matches_ingestion_convention() {
        // These strings must match `ingestion::mod::protocol_label` — VM
        // timeseries carry the `protocol` label value, and the baseline
        // query's `protocol="<X>"` selector needs to line up.
        assert_eq!(protocol_label(ProbeProtocol::Icmp), "icmp");
        assert_eq!(protocol_label(ProbeProtocol::Tcp), "tcp");
        assert_eq!(protocol_label(ProbeProtocol::Udp), "udp");
    }

    #[test]
    fn empty_roster_is_a_noop() {
        // No agents → no pairs → no VM round-trip. The caller
        // shouldn't race an upstream fetch just to find this out.
        let outcome = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                // Pass a bogus URL — a non-empty agent roster would
                // actually hit it; an empty one should short-circuit.
                let pool = sqlx::PgPool::connect_lazy("postgres://ignored@h/d").unwrap();
                fetch_and_archive_vm_baselines(
                    &pool,
                    "http://unreachable.invalid",
                    Uuid::nil(),
                    ProbeProtocol::Icmp,
                    &[],
                    Duration::from_secs(900),
                )
                .await
                .unwrap()
            });
        assert_eq!(outcome.pairs_fetched, 0);
        assert_eq!(outcome.pairs_archived, 0);
    }
}
