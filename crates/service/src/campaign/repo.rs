//! sqlx-backed CRUD + lifecycle transitions for `measurement_campaigns`
//! and `campaign_pairs`.
//!
//! Every state transition routes through [`transition_state`], which
//! issues an UPDATE gated on the expected current state. A 0-row outcome
//! surfaces as [`RepoError::IllegalTransition`] — handlers turn that
//! into HTTP 409 without a second SELECT.

use super::model::{
    CampaignRow, CampaignState, EvaluationMode, MeasurementKind, PairResolutionState, PairRow,
    ProbeProtocol,
};
use chrono::{DateTime, Utc};
use sqlx::{types::ipnetwork::IpNetwork, PgPool, Postgres, Transaction};
use std::net::IpAddr;
use uuid::Uuid;

/// Domain-level error enriched with sqlx error source.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// Underlying sqlx failure (connection, deadlock, constraint, etc.).
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Lifecycle transition rejected because the campaign's current state
    /// is not in `expected`. Handlers map this to HTTP 409.
    #[error(
        "illegal state transition for campaign {campaign_id}: from {from:?} (expected {expected:?})"
    )]
    IllegalTransition {
        /// The campaign that failed the gate.
        campaign_id: Uuid,
        /// The state observed on the row, if the campaign exists.
        from: Option<CampaignState>,
        /// The states the caller asserted the row had to be in.
        expected: Vec<CampaignState>,
    },
    /// No row with the given id exists. Handlers map this to HTTP 404.
    #[error("campaign {0} not found")]
    NotFound(Uuid),
    /// Persistence for `EvaluationMode::EdgeCandidate` is not yet wired
    /// up — the writer-side schema + insert path lands in T56 Phase G.
    /// Handlers map this to HTTP 501 so a `/evaluate` against an
    /// edge_candidate campaign surfaces the gap loudly instead of
    /// silently no-oping.
    #[error("EdgeCandidate persistence not yet implemented for campaign {campaign_id}")]
    EdgeCandidatePersistenceUnimplemented {
        /// The campaign that triggered the unimplemented branch.
        campaign_id: Uuid,
    },
}

/// Body for `POST /api/campaigns` (minus `created_by`, injected by the
/// handler from the session).
#[derive(Debug, Clone)]
pub struct CreateInput {
    /// Operator-facing title.
    pub title: String,
    /// Free-form notes.
    pub notes: String,
    /// Probe protocol for every pair in the campaign.
    pub protocol: ProbeProtocol,
    /// Source agent ids that will probe.
    pub source_agent_ids: Vec<String>,
    /// Destination IPs to probe.
    pub destination_ips: Vec<IpAddr>,
    /// When `true`, the scheduler ignores the 24 h reuse cache.
    pub force_measurement: bool,
    /// Optional probe count override (campaign rounds).
    pub probe_count: Option<i16>,
    /// Optional detail-probe count override (UI re-runs).
    pub probe_count_detail: Option<i16>,
    /// Optional per-probe timeout (ms).
    pub timeout_ms: Option<i32>,
    /// Optional inter-probe stagger (ms).
    pub probe_stagger_ms: Option<i32>,
    /// Optional loss-rate threshold for the evaluator.
    pub loss_threshold_ratio: Option<f32>,
    /// Optional RTT-stddev weight for the evaluator.
    pub stddev_weight: Option<f32>,
    /// Optional evaluation strategy.
    pub evaluation_mode: Option<EvaluationMode>,
    /// Optional eligibility cap on composed transit RTT (ms).
    pub max_transit_rtt_ms: Option<f64>,
    /// Optional eligibility cap on composed transit RTT stddev (ms).
    pub max_transit_stddev_ms: Option<f64>,
    /// Optional storage floor on absolute improvement (ms).
    pub min_improvement_ms: Option<f64>,
    /// Optional storage floor on relative improvement (fraction 0.0–1.0).
    pub min_improvement_ratio: Option<f64>,
    /// Optional RTT threshold (ms) for edge_candidate useful-route
    /// qualification. `None` disables the filter.
    pub useful_latency_ms: Option<f32>,
    /// Maximum transit hops for edge_candidate mode. Defaults to 2
    /// when the request omits the field.
    pub max_hops: Option<i16>,
    /// VictoriaMetrics look-back window (minutes) for edge_candidate mode.
    /// Defaults to 15 when the request omits the field.
    pub vm_lookback_minutes: Option<i32>,
    /// Session principal that created the row; audit-only.
    pub created_by: Option<String>,
}

/// Result of [`preview_dispatch_count`]. Matches the DTO exactly.
#[derive(Debug, Clone, Copy)]
pub struct PreviewCounts {
    /// Total number of (source, destination) pairs that would be created.
    pub total: i64,
    /// Pairs resolvable from the 24 h reuse window.
    pub reusable: i64,
    /// Pairs the scheduler would dispatch fresh (`total - reusable`).
    pub fresh: i64,
}

/// Delta payload for `POST /api/campaigns/:id/edit`.
#[derive(Debug, Clone, Default)]
pub struct EditInput {
    /// Pairs to add (or reset to `pending` if they already exist).
    pub add_pairs: Vec<(String, IpAddr)>,
    /// Pairs to remove entirely.
    pub remove_pairs: Vec<(String, IpAddr)>,
    /// When `Some(true)`, flips the sticky `force_measurement` flag and
    /// re-runs every non-delta pair.
    pub force_measurement: Option<bool>,
}

// ----- CRUD + lifecycle -------------------------------------------------

/// Insert a new campaign in the `draft` state and seed its
/// `(sources × destinations)` pair rows in the same transaction.
pub async fn create(pool: &PgPool, input: CreateInput) -> Result<CampaignRow, RepoError> {
    let mut tx = pool.begin().await?;
    let row: CampaignRow = sqlx::query_as!(
        CampaignRowRaw,
        r#"
        INSERT INTO measurement_campaigns
            (title, notes, protocol, probe_count, probe_count_detail, timeout_ms,
             probe_stagger_ms, force_measurement, loss_threshold_ratio, stddev_weight,
             evaluation_mode, max_transit_rtt_ms, max_transit_stddev_ms,
             min_improvement_ms, min_improvement_ratio,
             useful_latency_ms, max_hops, vm_lookback_minutes,
             created_by)
        VALUES ($1, $2, $3::probe_protocol,
                COALESCE($4, 10::smallint), COALESCE($5, 250::smallint),
                COALESCE($6, 2000), COALESCE($7, 100),
                $8, COALESCE($9, 0.02::real), COALESCE($10, 1.0::real),
                COALESCE($11::evaluation_mode, 'optimization'::evaluation_mode),
                $12, $13, $14, $15,
                $16, COALESCE($17, 2::smallint), COALESCE($18, 15),
                $19)
        RETURNING id, title, notes,
                  state AS "state: CampaignState",
                  protocol AS "protocol: ProbeProtocol",
                  probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                  force_measurement, loss_threshold_ratio, stddev_weight,
                  evaluation_mode AS "evaluation_mode: EvaluationMode",
                  max_transit_rtt_ms, max_transit_stddev_ms,
                  min_improvement_ms, min_improvement_ratio,
                  useful_latency_ms, max_hops, vm_lookback_minutes,
                  created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
        "#,
        input.title,
        input.notes,
        input.protocol as ProbeProtocol,
        input.probe_count,
        input.probe_count_detail,
        input.timeout_ms,
        input.probe_stagger_ms,
        input.force_measurement,
        input.loss_threshold_ratio,
        input.stddev_weight,
        input.evaluation_mode as _,
        input.max_transit_rtt_ms,
        input.max_transit_stddev_ms,
        input.min_improvement_ms,
        input.min_improvement_ratio,
        input.useful_latency_ms,
        input.max_hops,
        input.vm_lookback_minutes,
        input.created_by.as_deref(),
    )
    .fetch_one(&mut *tx)
    .await?
    .into();

    // Seed campaign_pairs from the (sources × destinations) cross product.
    if !input.source_agent_ids.is_empty() && !input.destination_ips.is_empty() {
        insert_pairs_in_tx(
            &mut tx,
            row.id,
            &input.source_agent_ids,
            &input.destination_ips,
        )
        .await?;
    }

    tx.commit().await?;
    Ok(row)
}

/// Fetch a single campaign by id. Returns `Ok(None)` when the id is unknown.
pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<CampaignRow>, RepoError> {
    let raw = sqlx::query_as!(
        CampaignRowRaw,
        r#"
        SELECT id, title, notes,
               state AS "state: CampaignState",
               protocol AS "protocol: ProbeProtocol",
               probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
               force_measurement, loss_threshold_ratio, stddev_weight,
               evaluation_mode AS "evaluation_mode: EvaluationMode",
               max_transit_rtt_ms, max_transit_stddev_ms,
               min_improvement_ms, min_improvement_ratio,
               useful_latency_ms, max_hops, vm_lookback_minutes,
               created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
          FROM measurement_campaigns
         WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    Ok(raw.map(Into::into))
}

/// List campaigns filtered by a substring match on title/notes, state,
/// and/or `created_by`. Results are ordered by `created_at` DESC and
/// capped at `min(limit, 500)`.
pub async fn list(
    pool: &PgPool,
    q: Option<&str>,
    state: Option<CampaignState>,
    created_by: Option<&str>,
    limit: i64,
) -> Result<Vec<CampaignRow>, RepoError> {
    // Static SQL for compile-time checking. Each filter becomes
    // "arg IS NULL OR column matches" so absent filters are inert.
    let q_like = q.map(|s| format!("%{s}%"));
    let bounded = limit.clamp(1, 500);
    let raws = sqlx::query_as!(
        CampaignRowRaw,
        r#"
        SELECT id, title, notes,
               state AS "state: CampaignState",
               protocol AS "protocol: ProbeProtocol",
               probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
               force_measurement, loss_threshold_ratio, stddev_weight,
               evaluation_mode AS "evaluation_mode: EvaluationMode",
               max_transit_rtt_ms, max_transit_stddev_ms,
               min_improvement_ms, min_improvement_ratio,
               useful_latency_ms, max_hops, vm_lookback_minutes,
               created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
          FROM measurement_campaigns
         WHERE ($1::text IS NULL OR title ILIKE $1 OR notes ILIKE $1)
           AND ($2::campaign_state IS NULL OR state = $2)
           AND ($3::text IS NULL OR created_by = $3)
         ORDER BY created_at DESC
         LIMIT $4
        "#,
        q_like.as_deref(),
        state as Option<CampaignState>,
        created_by,
        bounded,
    )
    .fetch_all(pool)
    .await?;
    Ok(raws.into_iter().map(Into::into).collect())
}

/// Partially update an editable campaign. `None`-valued arguments leave
/// the existing column untouched. Returns [`RepoError::NotFound`] if the
/// id is unknown.
#[allow(clippy::too_many_arguments)]
pub async fn patch(
    pool: &PgPool,
    id: Uuid,
    title: Option<&str>,
    notes: Option<&str>,
    loss_threshold_ratio: Option<f32>,
    stddev_weight: Option<f32>,
    evaluation_mode: Option<EvaluationMode>,
    max_transit_rtt_ms: Option<f64>,
    max_transit_stddev_ms: Option<f64>,
    min_improvement_ms: Option<f64>,
    min_improvement_ratio: Option<f64>,
    useful_latency_ms: Option<f32>,
    max_hops: Option<i16>,
    vm_lookback_minutes: Option<i32>,
) -> Result<CampaignRow, RepoError> {
    let raw = sqlx::query_as!(
        CampaignRowRaw,
        r#"
        UPDATE measurement_campaigns
           SET title                  = COALESCE($2, title),
               notes                  = COALESCE($3, notes),
               loss_threshold_ratio   = COALESCE($4, loss_threshold_ratio),
               stddev_weight          = COALESCE($5, stddev_weight),
               evaluation_mode        = COALESCE($6::evaluation_mode, evaluation_mode),
               max_transit_rtt_ms     = COALESCE($7, max_transit_rtt_ms),
               max_transit_stddev_ms  = COALESCE($8, max_transit_stddev_ms),
               min_improvement_ms     = COALESCE($9, min_improvement_ms),
               min_improvement_ratio  = COALESCE($10, min_improvement_ratio),
               useful_latency_ms      = COALESCE($11, useful_latency_ms),
               max_hops               = COALESCE($12, max_hops),
               vm_lookback_minutes    = COALESCE($13, vm_lookback_minutes)
         WHERE id = $1
         RETURNING id, title, notes,
                   state AS "state: CampaignState",
                   protocol AS "protocol: ProbeProtocol",
                   probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                   force_measurement, loss_threshold_ratio, stddev_weight,
                   evaluation_mode AS "evaluation_mode: EvaluationMode",
                   max_transit_rtt_ms, max_transit_stddev_ms,
                   min_improvement_ms, min_improvement_ratio,
                   useful_latency_ms, max_hops, vm_lookback_minutes,
                   created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
        "#,
        id,
        title,
        notes,
        loss_threshold_ratio,
        stddev_weight,
        evaluation_mode as Option<EvaluationMode>,
        max_transit_rtt_ms,
        max_transit_stddev_ms,
        min_improvement_ms,
        min_improvement_ratio,
        useful_latency_ms,
        max_hops,
        vm_lookback_minutes,
    )
    .fetch_optional(pool)
    .await?;
    match raw {
        Some(r) => Ok(r.into()),
        None => Err(RepoError::NotFound(id)),
    }
}

/// Delete a campaign. Cascades to `campaign_pairs`. Returns `true` if a
/// row was removed.
pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool, RepoError> {
    let rows = sqlx::query!("DELETE FROM measurement_campaigns WHERE id = $1", id)
        .execute(pool)
        .await?
        .rows_affected();
    Ok(rows > 0)
}

/// Transition a campaign from `draft` to `running` and stamp `started_at`.
pub async fn start(pool: &PgPool, id: Uuid) -> Result<CampaignRow, RepoError> {
    transition_state(
        pool,
        id,
        &[CampaignState::Draft],
        CampaignState::Running,
        Some("started_at"),
    )
    .await
}

/// Transition a campaign from `running` to `stopped` and flip every
/// `pending` pair to `skipped` in the same transaction. In-flight
/// `dispatched` pairs are left alone; T45's writer settles them.
pub async fn stop(pool: &PgPool, id: Uuid) -> Result<CampaignRow, RepoError> {
    let mut tx = pool.begin().await?;
    let row = transition_state_in_tx(
        &mut tx,
        id,
        &[CampaignState::Running],
        CampaignState::Stopped,
        Some("stopped_at"),
    )
    .await?;
    sqlx::query!(
        "UPDATE campaign_pairs
            SET resolution_state='skipped', last_error='campaign_stopped', settled_at=now()
          WHERE campaign_id=$1 AND resolution_state='pending'",
        id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// Apply an edit delta to a finished campaign (`completed`, `stopped`,
/// or `evaluated`) and transition it back to `running`. Adds/removes
/// pairs; when `force_measurement` is set, every non-delta pair is
/// reset so the whole campaign re-runs.
pub async fn apply_edit(
    pool: &PgPool,
    id: Uuid,
    edit: EditInput,
) -> Result<CampaignRow, RepoError> {
    let mut tx = pool.begin().await?;

    // 1. Lock the row so concurrent completion/evaluation can't race.
    let current_state: Option<CampaignState> = sqlx::query_scalar!(
        r#"SELECT state AS "state: CampaignState"
             FROM measurement_campaigns
            WHERE id = $1
            FOR UPDATE"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current_state) = current_state else {
        return Err(RepoError::NotFound(id));
    };
    let allowed = [
        CampaignState::Completed,
        CampaignState::Stopped,
        CampaignState::Evaluated,
    ];
    if !allowed.contains(&current_state) {
        return Err(RepoError::IllegalTransition {
            campaign_id: id,
            from: Some(current_state),
            expected: allowed.to_vec(),
        });
    }

    // 2. Remove baseline pairs the operator dropped.
    //    Detail rows (kind in {detail_ping, detail_mtr}) are operator-
    //    triggered ephemera for the same (source, destination) tuple; a
    //    /edit payload should not silently cancel them. Scope the DELETE
    //    to kind='campaign' so detail measurements survive the edit.
    for (src, dst) in &edit.remove_pairs {
        let dst_net = IpNetwork::from(*dst);
        sqlx::query!(
            "DELETE FROM campaign_pairs
              WHERE campaign_id = $1
                AND source_agent_id = $2
                AND destination_ip = $3
                AND kind = 'campaign'",
            id,
            src,
            dst_net
        )
        .execute(&mut *tx)
        .await?;
    }

    // 3. Insert or reset added pairs. A previously-skipped pair that is
    //    re-added resets to `pending` with cleared bookkeeping.
    //
    //    Duplicates are collapsed client-side first: Postgres's
    //    `INSERT ... ON CONFLICT DO UPDATE` refuses to target the same
    //    row twice in one statement (error 21000 — "cannot affect row a
    //    second time"), so a payload with repeated `(agent, ip)` pairs
    //    would otherwise surface as a 500.
    if !edit.add_pairs.is_empty() {
        let mut seen: std::collections::HashSet<(&str, IpAddr)> =
            std::collections::HashSet::with_capacity(edit.add_pairs.len());
        let mut srcs: Vec<&str> = Vec::with_capacity(edit.add_pairs.len());
        let mut dsts: Vec<IpNetwork> = Vec::with_capacity(edit.add_pairs.len());
        for (s, d) in &edit.add_pairs {
            if seen.insert((s.as_str(), *d)) {
                srcs.push(s.as_str());
                dsts.push(IpNetwork::from(*d));
            }
        }
        sqlx::query!(
            "INSERT INTO campaign_pairs (campaign_id, source_agent_id, destination_ip)
             SELECT $1, src, dst
               FROM UNNEST($2::text[], $3::inet[]) AS p(src, dst)
             ON CONFLICT (campaign_id, source_agent_id, destination_ip, kind) DO UPDATE
                SET resolution_state = 'pending',
                    settled_at       = NULL,
                    dispatched_at    = NULL,
                    attempt_count    = 0,
                    last_error       = NULL,
                    measurement_id   = NULL",
            id,
            &srcs as &[&str],
            &dsts as &[IpNetwork],
        )
        .execute(&mut *tx)
        .await?;
    }

    // 4. If force_measurement, flip the sticky flag and reset every
    //    non-delta pair so the whole campaign re-runs.
    if edit.force_measurement.unwrap_or(false) {
        sqlx::query!(
            "UPDATE measurement_campaigns SET force_measurement = TRUE WHERE id = $1",
            id
        )
        .execute(&mut *tx)
        .await?;
        // Reset every non-pending pair. `stop()` keeps `dispatched` rows
        // as-is (they may still settle from an in-flight agent call),
        // so force_measurement after stop must include `dispatched` or
        // those pairs stay stuck once the campaign re-enters running.
        // A late settle response arriving after this reset finds
        // `pending` and updates the row — the worst case is a slightly
        // stale reading, not a stuck campaign.
        // Reset baseline pairs only. Detail rows are independently
        // triggered and must not re-run just because the operator asked
        // to re-run the campaign's baseline measurements.
        sqlx::query!(
            "UPDATE campaign_pairs
                SET resolution_state = 'pending',
                    measurement_id   = NULL,
                    settled_at       = NULL,
                    dispatched_at    = NULL,
                    attempt_count    = 0,
                    last_error       = NULL
              WHERE campaign_id = $1
                AND kind = 'campaign'
                AND resolution_state IN ('dispatched','reused','succeeded','unreachable','skipped')",
            id
        )
        .execute(&mut *tx)
        .await?;
    }

    // 5. Transition back to Running and bump started_at.
    let row = transition_state_in_tx(
        &mut tx,
        id,
        &allowed,
        CampaignState::Running,
        Some("started_at"),
    )
    .await?;

    // NOTE: `campaign_evaluations` rows intentionally persist across
    // edit-delta re-runs. The operator presses Evaluate to refresh once
    // the re-dispatched measurements settle — an auto-dismiss here
    // would wipe the last-known analysis every time a pair is nudged.

    tx.commit().await?;
    Ok(row)
}

/// Reset one specific pair to `pending` (clearing bookkeeping) and
/// transition the campaign back to `running`. Used by the operator's
/// "force this one pair" button on finished campaigns.
pub async fn force_pair(
    pool: &PgPool,
    id: Uuid,
    source_agent_id: &str,
    destination_ip: IpAddr,
) -> Result<CampaignRow, RepoError> {
    let mut tx = pool.begin().await?;

    // Lock the campaign row first to match the order `apply_edit` takes
    // (measurement_campaigns then campaign_pairs). Swapping the order
    // opens an AB-BA deadlock window: a concurrent `apply_edit` that
    // already holds the campaign lock and is about to lock pairs would
    // deadlock with a `force_pair` that already holds a pair and is
    // about to lock the campaign. Postgres detects this and aborts one
    // of the transactions, which would surface as a 500 to the
    // operator; locking in the same order avoids the race entirely.
    //
    // Decide whether to stamp started_at: only when the parent campaign
    // is NOT already Running. Finished campaigns re-enter rotation and
    // need a fresh started_at so the scheduler picks them up; a Running
    // campaign's rotation order is started_at-anchored (see
    // `active_campaigns`), and force_pair must not disturb it.
    let current: Option<CampaignState> = sqlx::query_scalar!(
        r#"SELECT state AS "state: CampaignState"
             FROM measurement_campaigns WHERE id = $1 FOR UPDATE"#,
        id
    )
    .fetch_optional(&mut *tx)
    .await?;
    if current.is_none() {
        return Err(RepoError::NotFound(id));
    }
    let set_ts = match current {
        Some(CampaignState::Running) => None,
        _ => Some("started_at"),
    };

    // Scope to `kind='campaign'`: after the T5 widening the 4-column
    // UNIQUE key can legitimately hold `campaign + detail_ping +
    // detail_mtr` rows on the same tuple, so an unfiltered UPDATE would
    // RETURN multiple rows (sqlx's `fetch_optional` treats that as an
    // error) and would also silently reset the detail rows — `/detail`
    // runs their own lifecycle. Force-pair is a baseline-only operator
    // action; detail rows can be re-triggered via `/detail` instead.
    let dst_net = IpNetwork::from(destination_ip);
    let matched = sqlx::query_scalar!(
        "UPDATE campaign_pairs
            SET resolution_state = 'pending',
                measurement_id   = NULL,
                settled_at       = NULL,
                dispatched_at    = NULL,
                attempt_count    = 0,
                last_error       = NULL
          WHERE campaign_id = $1
            AND source_agent_id = $2
            AND destination_ip = $3
            AND kind = 'campaign'
         RETURNING id",
        id,
        source_agent_id,
        dst_net,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if matched.is_none() {
        return Err(RepoError::NotFound(id));
    }

    let row = transition_state_in_tx(
        &mut tx,
        id,
        &[
            CampaignState::Running,
            CampaignState::Completed,
            CampaignState::Stopped,
            CampaignState::Evaluated,
        ],
        CampaignState::Running,
        set_ts,
    )
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// List baseline (`kind='campaign'`) pairs for a campaign, optionally
/// filtered to a specific set of resolution states. Results ordered by
/// id, capped at `min(limit, 5000)`.
///
/// Detail rows (`kind ∈ {detail_ping, detail_mtr}`) are excluded
/// structurally — they have their own lifecycle and are surfaced via
/// `/detail`-scoped endpoints, not the generic campaign pair list.
/// Without this filter, `PairDto` consumers (which don't surface `kind`)
/// would see duplicate-looking rows on the same `(source, destination)`
/// tuple once an operator has triggered `/detail`.
pub async fn list_pairs(
    pool: &PgPool,
    id: Uuid,
    states: &[PairResolutionState],
    limit: i64,
) -> Result<Vec<PairRow>, RepoError> {
    let bounded = limit.clamp(1, 5000);
    let raws = sqlx::query_as!(
        PairRowRaw,
        r#"
        SELECT cp.id                     AS "id!",
               cp.campaign_id            AS "campaign_id!",
               cp.source_agent_id        AS "source_agent_id!",
               cp.destination_ip         AS "destination_ip!",
               cp.resolution_state       AS "resolution_state!: PairResolutionState",
               cp.measurement_id         AS "measurement_id?",
               cp.dispatched_at          AS "dispatched_at?",
               cp.settled_at             AS "settled_at?",
               cp.attempt_count          AS "attempt_count!",
               cp.last_error             AS "last_error?",
               cp.kind                   AS "kind!: MeasurementKind"
          FROM campaign_pairs cp
         WHERE cp.campaign_id = $1
           AND cp.kind = 'campaign'
           AND (cardinality($2::pair_resolution_state[]) = 0
                OR cp.resolution_state = ANY($2::pair_resolution_state[]))
         ORDER BY cp.id
         LIMIT $3
        "#,
        id,
        states as &[PairResolutionState],
        bounded,
    )
    .fetch_all(pool)
    .await?;
    Ok(raws.into_iter().map(Into::into).collect())
}

/// Count the total, reusable, and fresh dispatches for a campaign's
/// current `campaign_pairs` row set, using the 24 h reuse window.
///
/// Operates on the actual `(source_agent_id, destination_ip)` pairs
/// stored for the campaign — not a cross-product derived from the
/// unique sources and destinations. Sparse pair sets (e.g. after an
/// edit-delta removed specific pairs) preview correctly.
///
/// When `force_measurement` is true, reuse is disabled — mirrors the
/// scheduler's behavior in [`crate::campaign::scheduler`] so the
/// preview `reusable`/`fresh` split agrees with what a subsequent
/// start/edit would actually dispatch.
pub async fn preview_dispatch_count_for_campaign(
    pool: &PgPool,
    id: Uuid,
    protocol: ProbeProtocol,
    force_measurement: bool,
) -> Result<PreviewCounts, RepoError> {
    if force_measurement {
        let total: i64 = sqlx::query_scalar!(
            "SELECT COUNT(*) AS \"total!\" FROM campaign_pairs \
              WHERE campaign_id = $1 AND kind = 'campaign'",
            id
        )
        .fetch_one(pool)
        .await?;
        return Ok(PreviewCounts {
            total,
            reusable: 0,
            fresh: total,
        });
    }
    // Baseline-only: the preview reports how many campaign-kind pairs
    // would dispatch on start/edit. Detail rows are independently
    // triggered via `/detail` and must not inflate the preview.
    let row = sqlx::query!(
        r#"
        WITH pairs AS (
            SELECT source_agent_id, destination_ip
              FROM campaign_pairs
             WHERE campaign_id = $1
               AND kind = 'campaign'
        ),
        reusable AS (
            SELECT DISTINCT ON (p.source_agent_id, p.destination_ip)
                   p.source_agent_id, p.destination_ip
              FROM pairs p
              JOIN measurements m
                ON m.source_agent_id = p.source_agent_id
               AND m.destination_ip  = p.destination_ip
               AND m.protocol        = $2::probe_protocol
               AND m.measured_at     > now() - interval '24 hours'
             ORDER BY p.source_agent_id, p.destination_ip,
                      m.probe_count DESC, m.measured_at DESC
        )
        SELECT
            (SELECT COUNT(*) FROM pairs)    AS "total!",
            (SELECT COUNT(*) FROM reusable) AS "reusable!"
        "#,
        id,
        protocol as ProbeProtocol,
    )
    .fetch_one(pool)
    .await?;
    let total = row.total;
    let reusable = row.reusable;
    Ok(PreviewCounts {
        total,
        reusable,
        fresh: total - reusable,
    })
}

/// Count the total pairs the given sources × destinations would produce,
/// split between ones resolvable from the 24 h reuse window and ones
/// the scheduler would dispatch fresh. Never writes.
///
/// Used by pre-save preview paths where no `campaign_pairs` rows exist
/// yet. For an existing campaign, use
/// [`preview_dispatch_count_for_campaign`] instead — it operates on the
/// actual pair set and avoids the Cartesian explosion.
pub async fn preview_dispatch_count(
    pool: &PgPool,
    protocol: ProbeProtocol,
    sources: &[String],
    destinations: &[IpAddr],
) -> Result<PreviewCounts, RepoError> {
    // Expand the cross product client-side so we feed two parallel
    // arrays to the single UNNEST query the design note spells out.
    // `destination_ip::text` sidesteps IpNetwork's canonical form,
    // which would carry a trailing `/32` and defeat equality against
    // the IpAddr strings we receive here.
    let n = sources.len() * destinations.len();
    let mut expanded_sources: Vec<&str> = Vec::with_capacity(n);
    let mut expanded_dests: Vec<String> = Vec::with_capacity(n);
    for s in sources {
        for d in destinations {
            expanded_sources.push(s.as_str());
            expanded_dests.push(d.to_string());
        }
    }

    let row = sqlx::query!(
        r#"
        WITH pairs AS (
            SELECT src AS source_agent_id,
                   dst AS destination_ip_str
              FROM UNNEST($1::text[], $2::text[]) AS p(src, dst)
        ),
        reusable AS (
            SELECT DISTINCT ON (p.source_agent_id, p.destination_ip_str)
                   p.source_agent_id, p.destination_ip_str
              FROM pairs p
              JOIN measurements m
                ON m.source_agent_id = p.source_agent_id
               AND m.destination_ip = p.destination_ip_str::inet
               AND m.protocol = $3::probe_protocol
               AND m.measured_at > now() - interval '24 hours'
             ORDER BY p.source_agent_id, p.destination_ip_str,
                      m.probe_count DESC, m.measured_at DESC
        )
        SELECT
            (SELECT COUNT(*) FROM pairs)    AS "total!",
            (SELECT COUNT(*) FROM reusable) AS "reusable!"
        "#,
        &expanded_sources as &[&str],
        &expanded_dests as &[String],
        protocol as ProbeProtocol,
    )
    .fetch_one(pool)
    .await?;

    let total = row.total;
    let reusable = row.reusable;
    Ok(PreviewCounts {
        total,
        reusable,
        fresh: total - reusable,
    })
}

// ----- Scheduler-facing repo helpers ------------------------------------

/// Fetch a single campaign row for the scheduler's dispatch path.
///
/// Mirrors [`get`] but exists as a distinct name so scheduler-origin
/// reads stand out in traces; the compiler de-duplicates the SQL.
pub async fn get_raw_for_scheduler(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<CampaignRow>, RepoError> {
    Ok(sqlx::query_as!(
        CampaignRowRaw,
        r#"
        SELECT id, title, notes,
               state AS "state: CampaignState",
               protocol AS "protocol: ProbeProtocol",
               probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
               force_measurement, loss_threshold_ratio, stddev_weight,
               evaluation_mode AS "evaluation_mode: EvaluationMode",
               max_transit_rtt_ms, max_transit_stddev_ms,
               min_improvement_ms, min_improvement_ratio,
               useful_latency_ms, max_hops, vm_lookback_minutes,
               created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
          FROM measurement_campaigns
         WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?
    .map(Into::into))
}

/// Snapshot of campaigns currently in `running` state, ordered by
/// `started_at` ASC (stable rotation order for the scheduler).
pub async fn active_campaigns(pool: &PgPool) -> Result<Vec<Uuid>, RepoError> {
    let ids = sqlx::query_scalar!(
        "SELECT id FROM measurement_campaigns WHERE state='running' ORDER BY started_at ASC"
    )
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Atomically claim up to `chunk_size` pending pairs for a given
/// `(campaign, source_agent)` pair, flipping them to `dispatched` and
/// incrementing `attempt_count`. Uses `SELECT ... FOR UPDATE SKIP LOCKED`
/// so concurrent tick paths cannot double-claim a row.
pub async fn take_pending_batch(
    pool: &PgPool,
    campaign_id: Uuid,
    source_agent_id: &str,
    chunk_size: i64,
) -> Result<Vec<PairRow>, RepoError> {
    let bounded = chunk_size.clamp(1, 10_000);
    let raws = sqlx::query_as!(
        PairRowRaw,
        r#"
        WITH chosen AS (
            SELECT id
              FROM campaign_pairs
             WHERE campaign_id = $1
               AND source_agent_id = $2
               AND resolution_state = 'pending'
             ORDER BY id
             LIMIT $3
             FOR UPDATE SKIP LOCKED
        )
        UPDATE campaign_pairs
           SET resolution_state = 'dispatched',
               dispatched_at    = now(),
               attempt_count    = campaign_pairs.attempt_count + 1
          FROM chosen
         WHERE campaign_pairs.id = chosen.id
         RETURNING campaign_pairs.id,
                   campaign_pairs.campaign_id,
                   campaign_pairs.source_agent_id,
                   campaign_pairs.destination_ip,
                   campaign_pairs.resolution_state AS "resolution_state: PairResolutionState",
                   campaign_pairs.measurement_id,
                   campaign_pairs.dispatched_at,
                   campaign_pairs.settled_at,
                   campaign_pairs.attempt_count,
                   campaign_pairs.last_error,
                   campaign_pairs.kind AS "kind: MeasurementKind"
        "#,
        campaign_id,
        source_agent_id,
        bounded,
    )
    .fetch_all(pool)
    .await?;
    Ok(raws.into_iter().map(Into::into).collect())
}

/// Look up each pair in the 24 h reuse window. Returns the pairs that
/// have a reuse match as `(pair_id, measurement_id)`; unmatched pairs
/// are absent from the result and must be dispatched fresh.
pub async fn resolve_reuse(
    pool: &PgPool,
    pairs: &[PairRow],
    protocol: ProbeProtocol,
) -> Result<Vec<(i64, i64)>, RepoError> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let pair_ids: Vec<i64> = pairs.iter().map(|p| p.id).collect();
    let sources: Vec<&str> = pairs.iter().map(|p| p.source_agent_id.as_str()).collect();
    let dests: Vec<String> = pairs
        .iter()
        .map(|p| p.destination_ip.ip().to_string())
        .collect();

    let rows = sqlx::query!(
        r#"
        WITH requested AS (
            SELECT r.pair_id, r.source_agent_id, r.destination_ip_str
              FROM UNNEST($1::bigint[], $2::text[], $3::text[])
                     AS r(pair_id, source_agent_id, destination_ip_str)
              JOIN campaign_pairs cp ON cp.id = r.pair_id
             WHERE cp.kind = 'campaign'
        ),
        latest AS (
            SELECT DISTINCT ON (r.source_agent_id, r.destination_ip_str)
                   r.pair_id, m.id AS measurement_id
              FROM requested r
              JOIN measurements m
                ON m.source_agent_id = r.source_agent_id
               AND m.destination_ip = r.destination_ip_str::inet
               AND m.protocol = $4::probe_protocol
               AND m.measured_at > now() - interval '24 hours'
               -- Reuse requires usable baseline data. `detail_mtr` rows
               -- (and any future kind that omits latency) carry
               -- `latency_avg_ms IS NULL` by design; binding one to a
               -- baseline pair would leave the evaluator unable to
               -- score against it (see `eval::evaluate`'s inner
               -- `Some(direct_rtt)` gate) and could falsely trigger
               -- `no_baseline_pairs` on low-probe campaigns.
               AND m.latency_avg_ms IS NOT NULL
             ORDER BY r.source_agent_id, r.destination_ip_str,
                      m.probe_count DESC, m.measured_at DESC
        )
        SELECT pair_id AS "pair_id!", measurement_id AS "measurement_id!"
          FROM latest
        "#,
        &pair_ids as &[i64],
        &sources as &[&str],
        &dests as &[String],
        protocol as ProbeProtocol,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| (r.pair_id, r.measurement_id))
        .collect())
}

/// Mark each `(pair_id, measurement_id)` pair as `reused`.
///
/// Reused pairs never actually reached an agent, so this also clears
/// the `dispatched_at` stamp and rolls back the `attempt_count` bump
/// that `take_pending_batch` applied while claiming the row. Otherwise
/// the API and operator tooling would report the pair as having been
/// dispatched, which is false.
pub async fn apply_reuse(pool: &PgPool, decisions: &[(i64, i64)]) -> Result<(), RepoError> {
    if decisions.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = decisions.iter().map(|(p, _)| *p).collect();
    let measurement_ids: Vec<i64> = decisions.iter().map(|(_, m)| *m).collect();
    sqlx::query!(
        "UPDATE campaign_pairs AS cp
            SET resolution_state = 'reused',
                measurement_id   = d.measurement_id,
                settled_at       = now(),
                dispatched_at    = NULL,
                attempt_count    = GREATEST(0, cp.attempt_count - 1)
           FROM UNNEST($1::bigint[], $2::bigint[]) AS d(pair_id, measurement_id)
          WHERE cp.id = d.pair_id",
        &ids as &[i64],
        &measurement_ids as &[i64],
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Safety-net sweep that flips any `pending` pair with
/// `attempt_count >= max_attempts` to `skipped` with
/// `last_error = 'max_attempts_exceeded'`. Returns rows affected.
pub async fn expire_stale_attempts(pool: &PgPool, max_attempts: i16) -> Result<u64, RepoError> {
    let affected = sqlx::query!(
        "UPDATE campaign_pairs
            SET resolution_state = 'skipped',
                last_error       = 'max_attempts_exceeded',
                settled_at       = now()
          WHERE resolution_state = 'pending'
            AND attempt_count >= $1",
        max_attempts
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected)
}

/// Skip `pending` pairs whose `source_agent_id` is not in the given
/// active-agent set, scoped to the supplied campaigns. Without this
/// sweep, a campaign targeting an offline agent would keep those
/// pairs in `pending` forever — the scheduler only iterates active
/// agents, and `expire_stale_attempts` only bumps pairs that actually
/// get claimed for dispatch.
///
/// The agent-activity window (see `CampaignsSection` /
/// `registry_active_window`) is already on the order of minutes, so
/// any agent handed to this sweep has already been silent long enough
/// to be considered truly offline; the sweep is not aggressive
/// relative to that window.
///
/// `last_error` is set to `agent_offline` so operator tooling can
/// distinguish this case from `max_attempts_exceeded`.
pub async fn skip_pending_for_inactive_sources(
    pool: &PgPool,
    active_agent_ids: &[String],
    campaign_ids: &[Uuid],
) -> Result<u64, RepoError> {
    if campaign_ids.is_empty() {
        return Ok(0);
    }
    let affected = sqlx::query!(
        "UPDATE campaign_pairs
            SET resolution_state = 'skipped',
                last_error       = 'agent_offline',
                settled_at       = now()
          WHERE resolution_state = 'pending'
            AND campaign_id = ANY($1::uuid[])
            AND source_agent_id <> ALL($2::text[])",
        campaign_ids,
        active_agent_ids as &[String],
    )
    .execute(pool)
    .await?
    .rows_affected();
    Ok(affected)
}

/// Summary statistics for scheduler self-metrics.
///
/// Returns counts per campaign state, counts per pair resolution state,
/// and the fraction of terminal pairs resolved via the 24 h reuse window.
#[derive(Debug, Default, Clone)]
pub struct MetricsSnapshot {
    /// `(state, count)` for every distinct `measurement_campaigns.state`.
    pub campaigns: Vec<(CampaignState, i64)>,
    /// `(state, count)` for every distinct `campaign_pairs.resolution_state`.
    pub pairs: Vec<(PairResolutionState, i64)>,
    /// Fraction of terminal pairs resolved via reuse; `0.0` when there
    /// are no terminal pairs yet (SQL returns NULL in that case).
    pub reuse_ratio: f64,
}

/// Aggregate the scheduler self-metrics into one [`MetricsSnapshot`].
///
/// Three aggregates over small indexed tables executed sequentially on
/// the shared pool. Called once per scheduler tick — the cost is
/// dominated by round-trip latency, not scan work. Uses runtime
/// [`sqlx::query_as`] (typed tuple) rather than `query_as!` so the
/// `.sqlx/` offline cache does not need regeneration.
pub async fn metrics_snapshot(pool: &PgPool) -> Result<MetricsSnapshot, RepoError> {
    let campaigns: Vec<(CampaignState, i64)> = sqlx::query_as::<_, (CampaignState, i64)>(
        "SELECT state, COUNT(*) FROM measurement_campaigns GROUP BY 1",
    )
    .fetch_all(pool)
    .await?;

    let pairs: Vec<(PairResolutionState, i64)> = sqlx::query_as::<_, (PairResolutionState, i64)>(
        "SELECT resolution_state, COUNT(*) FROM campaign_pairs GROUP BY 1",
    )
    .fetch_all(pool)
    .await?;

    // Baseline-only: `reuse_ratio` reports dispatch-efficiency for
    // campaign-kind pairs. `/detail` rows dispatch with
    // `force_measurement=true` and can never `reuse`, so including
    // them would pull the metric toward zero whenever detail traffic
    // runs — an artefact of operator action, not a real reuse regression.
    let reuse_ratio: Option<f64> = sqlx::query_scalar(
        "SELECT CASE WHEN COUNT(*) = 0 THEN NULL \
                ELSE COUNT(*) FILTER (WHERE resolution_state='reused')::float8 \
                     / COUNT(*)::float8 \
              END \
           FROM campaign_pairs \
          WHERE kind = 'campaign' \
            AND resolution_state IN ('reused','succeeded','unreachable','skipped')",
    )
    .fetch_one(pool)
    .await?;

    Ok(MetricsSnapshot {
        campaigns,
        pairs,
        reuse_ratio: reuse_ratio.unwrap_or(0.0),
    })
}

/// Atomically flip a `running` campaign to `completed` iff no pair
/// remains in `pending` or `dispatched`. Returns `true` if the flip
/// happened. Safe to call repeatedly.
///
/// The `NOT EXISTS` guard is intentionally kind-agnostic — detail rows
/// (kind ∈ {detail_ping, detail_mtr}) block completion just like
/// baseline pairs. Rationale: `insert_detail_pairs` transitions the
/// campaign back to `running` when detail work lands, and the
/// scheduler only ticks `running` campaigns; if the guard excluded
/// detail rows, `maybe_complete` would flip the campaign back to
/// `completed` between ticks and the scheduler would stop picking up
/// pending detail pairs. Keeping detail rows in the guard is what
/// makes the "campaign re-enters running until all detail drains"
/// contract work with the current single-scheduler-state model.
pub async fn maybe_complete(pool: &PgPool, campaign_id: Uuid) -> Result<bool, RepoError> {
    let updated = sqlx::query_scalar!(
        "UPDATE measurement_campaigns
            SET state = 'completed', completed_at = now()
          WHERE id = $1
            AND state = 'running'
            AND NOT EXISTS (
                SELECT 1 FROM campaign_pairs
                 WHERE campaign_id = $1
                   AND resolution_state IN ('pending','dispatched')
            )
         RETURNING id",
        campaign_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(updated.is_some())
}

// ----- Helpers ----------------------------------------------------------

/// Transition a campaign's `state` column from one of `expected` to `to`,
/// optionally stamping one of the lifecycle timestamps. Uses its own
/// transaction; the `_in_tx` variant exists for callers that need to
/// bundle the transition with extra writes.
pub async fn transition_state(
    pool: &PgPool,
    id: Uuid,
    expected: &[CampaignState],
    to: CampaignState,
    set_timestamp_column: Option<&str>,
) -> Result<CampaignRow, RepoError> {
    let mut tx = pool.begin().await?;
    let row = transition_state_in_tx(&mut tx, id, expected, to, set_timestamp_column).await?;
    tx.commit().await?;
    Ok(row)
}

pub(crate) async fn transition_state_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    expected: &[CampaignState],
    to: CampaignState,
    set_timestamp_column: Option<&str>,
) -> Result<CampaignRow, RepoError> {
    // The timestamp column must be one of a closed set — `None`,
    // `started_at`, `stopped_at`, `completed_at`, or `evaluated_at`.
    // Anything else is a code-level bug.
    let column = set_timestamp_column.unwrap_or("");
    let row_opt = match column {
        "" => {
            sqlx::query_as!(
                CampaignRowRaw,
                r#"
            UPDATE measurement_campaigns
               SET state = $2
             WHERE id = $1 AND state = ANY($3::campaign_state[])
             RETURNING id, title, notes,
                 state AS "state: CampaignState",
                 protocol AS "protocol: ProbeProtocol",
                 probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                 force_measurement, loss_threshold_ratio, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
                 max_transit_rtt_ms, max_transit_stddev_ms,
                 min_improvement_ms, min_improvement_ratio,
                 useful_latency_ms, max_hops, vm_lookback_minutes,
                 created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
            "#,
                id,
                to as CampaignState,
                expected as &[CampaignState],
            )
            .fetch_optional(&mut **tx)
            .await?
        }
        "started_at" => {
            sqlx::query_as!(
                CampaignRowRaw,
                r#"
            UPDATE measurement_campaigns SET state = $2, started_at = now()
             WHERE id = $1 AND state = ANY($3::campaign_state[])
             RETURNING id, title, notes,
                 state AS "state: CampaignState",
                 protocol AS "protocol: ProbeProtocol",
                 probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                 force_measurement, loss_threshold_ratio, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
                 max_transit_rtt_ms, max_transit_stddev_ms,
                 min_improvement_ms, min_improvement_ratio,
                 useful_latency_ms, max_hops, vm_lookback_minutes,
                 created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
            "#,
                id,
                to as CampaignState,
                expected as &[CampaignState],
            )
            .fetch_optional(&mut **tx)
            .await?
        }
        "stopped_at" => {
            sqlx::query_as!(
                CampaignRowRaw,
                r#"
            UPDATE measurement_campaigns SET state = $2, stopped_at = now()
             WHERE id = $1 AND state = ANY($3::campaign_state[])
             RETURNING id, title, notes,
                 state AS "state: CampaignState",
                 protocol AS "protocol: ProbeProtocol",
                 probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                 force_measurement, loss_threshold_ratio, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
                 max_transit_rtt_ms, max_transit_stddev_ms,
                 min_improvement_ms, min_improvement_ratio,
                 useful_latency_ms, max_hops, vm_lookback_minutes,
                 created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
            "#,
                id,
                to as CampaignState,
                expected as &[CampaignState],
            )
            .fetch_optional(&mut **tx)
            .await?
        }
        "completed_at" => {
            sqlx::query_as!(
                CampaignRowRaw,
                r#"
            UPDATE measurement_campaigns SET state = $2, completed_at = now()
             WHERE id = $1 AND state = ANY($3::campaign_state[])
             RETURNING id, title, notes,
                 state AS "state: CampaignState",
                 protocol AS "protocol: ProbeProtocol",
                 probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                 force_measurement, loss_threshold_ratio, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
                 max_transit_rtt_ms, max_transit_stddev_ms,
                 min_improvement_ms, min_improvement_ratio,
                 useful_latency_ms, max_hops, vm_lookback_minutes,
                 created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
            "#,
                id,
                to as CampaignState,
                expected as &[CampaignState],
            )
            .fetch_optional(&mut **tx)
            .await?
        }
        "evaluated_at" => {
            sqlx::query_as!(
                CampaignRowRaw,
                r#"
            UPDATE measurement_campaigns SET state = $2, evaluated_at = now()
             WHERE id = $1 AND state = ANY($3::campaign_state[])
             RETURNING id, title, notes,
                 state AS "state: CampaignState",
                 protocol AS "protocol: ProbeProtocol",
                 probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                 force_measurement, loss_threshold_ratio, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
                 max_transit_rtt_ms, max_transit_stddev_ms,
                 min_improvement_ms, min_improvement_ratio,
                 useful_latency_ms, max_hops, vm_lookback_minutes,
                 created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
            "#,
                id,
                to as CampaignState,
                expected as &[CampaignState],
            )
            .fetch_optional(&mut **tx)
            .await?
        }
        other => panic!("transition_state: unsupported timestamp column {other:?}"),
    };

    match row_opt {
        Some(raw) => Ok(raw.into()),
        None => {
            // No row updated: either the campaign doesn't exist, or the
            // precondition failed. Disambiguate with a SELECT so handlers
            // return 404 vs 409 correctly.
            let current: Option<CampaignState> = sqlx::query_scalar!(
                r#"SELECT state AS "state: CampaignState" FROM measurement_campaigns WHERE id = $1"#,
                id
            )
            .fetch_optional(&mut **tx)
            .await?;
            match current {
                None => Err(RepoError::NotFound(id)),
                Some(from) => Err(RepoError::IllegalTransition {
                    campaign_id: id,
                    from: Some(from),
                    expected: expected.to_vec(),
                }),
            }
        }
    }
}

async fn insert_pairs_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    campaign_id: Uuid,
    source_agent_ids: &[String],
    destination_ips: &[IpAddr],
) -> Result<(), sqlx::Error> {
    let n_sources = source_agent_ids.len();
    let n_dests = destination_ips.len();
    let total = n_sources * n_dests;
    let mut expanded_sources: Vec<&str> = Vec::with_capacity(total);
    let mut expanded_dests: Vec<IpNetwork> = Vec::with_capacity(total);
    for s in source_agent_ids {
        for d in destination_ips {
            expanded_sources.push(s);
            expanded_dests.push(IpNetwork::from(*d));
        }
    }
    sqlx::query!(
        "INSERT INTO campaign_pairs (campaign_id, source_agent_id, destination_ip)
         SELECT $1, src, dst
           FROM UNNEST($2::text[], $3::inet[]) AS p(src, dst)
         ON CONFLICT (campaign_id, source_agent_id, destination_ip, kind) DO NOTHING",
        campaign_id,
        &expanded_sources as &[&str],
        &expanded_dests as &[IpNetwork],
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// sqlx-derived raw row. [`CampaignRow`] is the domain-layer clone.
struct CampaignRowRaw {
    id: Uuid,
    title: String,
    notes: String,
    state: CampaignState,
    protocol: ProbeProtocol,
    probe_count: i16,
    probe_count_detail: i16,
    timeout_ms: i32,
    probe_stagger_ms: i32,
    force_measurement: bool,
    loss_threshold_ratio: f32,
    stddev_weight: f32,
    evaluation_mode: EvaluationMode,
    max_transit_rtt_ms: Option<f64>,
    max_transit_stddev_ms: Option<f64>,
    min_improvement_ms: Option<f64>,
    min_improvement_ratio: Option<f64>,
    useful_latency_ms: Option<f32>,
    max_hops: i16,
    vm_lookback_minutes: i32,
    created_by: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    stopped_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    evaluated_at: Option<DateTime<Utc>>,
}

impl From<CampaignRowRaw> for CampaignRow {
    fn from(r: CampaignRowRaw) -> Self {
        Self {
            id: r.id,
            title: r.title,
            notes: r.notes,
            state: r.state,
            protocol: r.protocol,
            probe_count: r.probe_count,
            probe_count_detail: r.probe_count_detail,
            timeout_ms: r.timeout_ms,
            probe_stagger_ms: r.probe_stagger_ms,
            force_measurement: r.force_measurement,
            loss_threshold_ratio: r.loss_threshold_ratio,
            stddev_weight: r.stddev_weight,
            evaluation_mode: r.evaluation_mode,
            max_transit_rtt_ms: r.max_transit_rtt_ms,
            max_transit_stddev_ms: r.max_transit_stddev_ms,
            min_improvement_ms: r.min_improvement_ms,
            min_improvement_ratio: r.min_improvement_ratio,
            useful_latency_ms: r.useful_latency_ms,
            max_hops: r.max_hops,
            vm_lookback_minutes: r.vm_lookback_minutes,
            created_by: r.created_by,
            created_at: r.created_at,
            started_at: r.started_at,
            stopped_at: r.stopped_at,
            completed_at: r.completed_at,
            evaluated_at: r.evaluated_at,
        }
    }
}

/// sqlx-derived raw row for [`PairRow`].
struct PairRowRaw {
    id: i64,
    campaign_id: Uuid,
    source_agent_id: String,
    destination_ip: IpNetwork,
    resolution_state: PairResolutionState,
    measurement_id: Option<i64>,
    dispatched_at: Option<DateTime<Utc>>,
    settled_at: Option<DateTime<Utc>>,
    attempt_count: i16,
    last_error: Option<String>,
    kind: MeasurementKind,
}

impl From<PairRowRaw> for PairRow {
    fn from(r: PairRowRaw) -> Self {
        Self {
            id: r.id,
            campaign_id: r.campaign_id,
            source_agent_id: r.source_agent_id,
            destination_ip: r.destination_ip,
            resolution_state: r.resolution_state,
            measurement_id: r.measurement_id,
            dispatched_at: r.dispatched_at,
            settled_at: r.settled_at,
            attempt_count: r.attempt_count,
            last_error: r.last_error,
            kind: r.kind,
        }
    }
}

// ----- Evaluation persistence (T48) -------------------------------------

use crate::campaign::eval::{
    AgentRow as EvalAgentRow, AttributedMeasurement, CatalogueLookup, EvaluationInputs,
};
use crate::campaign::model::DirectSource;
use std::collections::HashMap;

/// Agent roster for a campaign.
///
/// Returns every agent whose ID is a source in the campaign's
/// `campaign_pairs` rows **and** every agent whose IP is one of the
/// campaign's destinations (so agents-as-destinations are part of the
/// baseline lookup).
///
/// Read-only; shares the same source-of-truth (`agents_with_catalogue`)
/// as the evaluator's own agent pull so the two sets agree.
pub async fn agents_for_campaign(
    pool: &PgPool,
    campaign_id: Uuid,
) -> Result<Vec<EvalAgentRow>, RepoError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT agent_id AS "agent_id!", ip AS "ip!"
             FROM agents_with_catalogue
            WHERE ip IN (
                SELECT destination_ip FROM campaign_pairs WHERE campaign_id = $1
            )
               OR agent_id IN (
                SELECT source_agent_id FROM campaign_pairs WHERE campaign_id = $1
            )"#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| EvalAgentRow {
            agent_id: r.agent_id,
            ip: r.ip.ip(),
            // Hostname is stamped on the wire DTO post-evaluator via
            // the bulk hostname-cache pipeline; the agent roster used
            // by the evaluator itself doesn't surface hostnames.
            hostname: None,
        })
        .collect())
}

/// Builds the pure-function evaluator's inputs from DB state.
///
/// - Reads only `campaign_pairs.kind='campaign'` rows — detail rows
///   never feed the baseline/candidate matrix.
/// - Joins `measurements` via `campaign_pairs.measurement_id`.
/// - Pulls agents + enrichment from `agents_with_catalogue` +
///   `ip_catalogue` in one pass so the evaluator stays pure.
pub async fn measurements_for_campaign(
    pool: &PgPool,
    campaign_id: Uuid,
) -> Result<EvaluationInputs, RepoError> {
    let campaign = sqlx::query!(
        r#"SELECT loss_threshold_ratio, stddev_weight,
                  evaluation_mode AS "evaluation_mode: EvaluationMode",
                  max_transit_rtt_ms, max_transit_stddev_ms,
                  min_improvement_ms, min_improvement_ratio,
                  useful_latency_ms, max_hops
             FROM measurement_campaigns WHERE id = $1"#,
        campaign_id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or(RepoError::NotFound(campaign_id))?;

    // Baseline-only: `cp.kind='campaign'` is the load-bearing filter. We
    // deliberately do NOT filter `m.kind` because `resolve_reuse` can
    // legitimately bind a `detail_ping`-kind measurement from the 24 h
    // reuse window to a baseline pair — excluding those would undercount
    // the baseline whenever an operator runs `/detail` before the next
    // campaign covering the same (source, destination, protocol) tuple.
    let rows = sqlx::query!(
        r#"SELECT m.source_agent_id,
                  m.destination_ip,
                  m.latency_avg_ms,
                  m.latency_stddev_ms,
                  m.loss_ratio,
                  m.id AS measurement_id,
                  m.mtr_id
             FROM measurements m
             JOIN campaign_pairs cp ON cp.measurement_id = m.id
            WHERE cp.campaign_id = $1
              AND cp.kind = 'campaign'"#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;

    // `mtr_measurement_id` is the DTO's documented FK to `measurements.id`
    // (see `EvaluationPairDetailDto::mtr_measurement_id_ax` / `_xb`),
    // populated only for legs whose backing measurement carries an MTR
    // trace. Surfacing `m.mtr_id` (which FKs to `mtr_traces.id`) would
    // have pointed clients at the wrong table entirely.
    let measurements: Vec<AttributedMeasurement> = rows
        .into_iter()
        .map(|r| AttributedMeasurement {
            source_agent_id: r.source_agent_id,
            destination_ip: r.destination_ip.ip(),
            latency_avg_ms: r.latency_avg_ms,
            latency_stddev_ms: r.latency_stddev_ms,
            loss_ratio: r.loss_ratio,
            mtr_measurement_id: r.mtr_id.map(|_| r.measurement_id),
            // Every row here joins through `campaign_pairs.measurement_id`
            // into the `measurements` table — by construction an active-
            // probe settlement. The T54-03 handler layers VM-continuous
            // rows on top in-memory; those never reach this loader.
            direct_source: DirectSource::ActiveProbe,
        })
        .collect();

    let agent_rows = sqlx::query!(
        r#"SELECT DISTINCT agent_id AS "agent_id!", ip AS "ip!"
             FROM agents_with_catalogue
            WHERE ip IN (
                SELECT destination_ip FROM campaign_pairs WHERE campaign_id = $1
            )
               OR agent_id IN (
                SELECT source_agent_id FROM campaign_pairs WHERE campaign_id = $1
            )"#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;

    let agents: Vec<EvalAgentRow> = agent_rows
        .into_iter()
        .map(|r| EvalAgentRow {
            agent_id: r.agent_id,
            ip: r.ip.ip(),
            // Hostname stamping happens in the handler via the bulk
            // hostname-cache lookup (Phase H wires it for
            // edge_candidate). Diversity / Optimization don't read this
            // field; the EdgeCandidate arm tolerates `None`.
            hostname: None,
        })
        .collect();

    let enr_rows = sqlx::query!(
        r#"SELECT c.ip,
                  c.display_name,
                  c.city,
                  c.country_code,
                  c.asn,
                  c.network_operator
             FROM ip_catalogue c
            WHERE c.ip IN (
                SELECT DISTINCT destination_ip FROM campaign_pairs WHERE campaign_id = $1
            )"#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;

    let enrichment: HashMap<IpAddr, CatalogueLookup> = enr_rows
        .into_iter()
        .map(|r| {
            (
                r.ip.ip(),
                CatalogueLookup {
                    display_name: r.display_name,
                    city: r.city,
                    country_code: r.country_code,
                    asn: r.asn.map(|v| v as i64),
                    network_operator: r.network_operator,
                    // The diversity / optimization queries don't pull
                    // these fields today; the EdgeCandidate handler
                    // adds them in Phase H. Defaulting to `None` here
                    // keeps the loader compatible with both arms.
                    website: None,
                    notes: None,
                    hostname: None,
                },
            )
        })
        .collect();

    // EdgeCandidate iterates over the campaign's full destination set
    // (including IPs that have no outgoing measurement — they appear
    // as unreachable rows). Diversity / Optimization don't read this
    // field — they enumerate candidates implicitly from the
    // measurement set — but we populate it consistently for both
    // modes so a future refactor doesn't surprise either arm.
    let candidate_rows = sqlx::query!(
        r#"SELECT DISTINCT destination_ip FROM campaign_pairs
            WHERE campaign_id = $1
              AND kind = 'campaign'"#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;
    let candidate_ips: Vec<IpAddr> = candidate_rows
        .into_iter()
        .map(|r| r.destination_ip.ip())
        .collect();

    Ok(EvaluationInputs {
        measurements,
        agents,
        candidate_ips,
        enrichment,
        loss_threshold_ratio: campaign.loss_threshold_ratio,
        stddev_weight: campaign.stddev_weight,
        mode: campaign.evaluation_mode,
        max_transit_rtt_ms: campaign.max_transit_rtt_ms,
        max_transit_stddev_ms: campaign.max_transit_stddev_ms,
        min_improvement_ms: campaign.min_improvement_ms,
        min_improvement_ratio: campaign.min_improvement_ratio,
        useful_latency_ms: campaign.useful_latency_ms,
        // `max_hops` is stored as `SMALLINT NOT NULL` with a 0..=2
        // schema constraint; surfacing the raw `i16` keeps the type
        // identical to the validation surface and the wire DTO.
        max_hops: campaign.max_hops,
    })
}

/// Returns every (source_agent_id, destination_ip) tuple for baseline
/// pairs that the campaign settled — used by the `POST /detail` handler's
/// `all` scope. Detail pairs are excluded structurally via `kind='campaign'`.
pub async fn settled_campaign_pairs(
    pool: &PgPool,
    campaign_id: Uuid,
) -> Result<Vec<(String, IpAddr)>, RepoError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT source_agent_id, destination_ip
             FROM campaign_pairs
            WHERE campaign_id = $1
              AND kind = 'campaign'
              AND resolution_state IN ('succeeded', 'reused')"#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| (r.source_agent_id, r.destination_ip.ip()))
        .collect())
}

/// Inserts detail-pair rows for the `POST /detail` handler and flips
/// the campaign state back to `running`. Returns
/// `(inserted, post_state)`:
///
/// - `inserted` — number of rows **actually inserted** (not requested);
///   duplicates skip silently via `ON CONFLICT DO NOTHING`.
/// - `post_state` — the campaign's state at commit time, read back
///   under the same transaction lock. The caller compares this to the
///   pre-call state to decide whether to publish a state-changed event
///   and to populate the `DetailResponse.campaign_state` field with
///   the actual post-insert state (not the potentially-stale pre-read).
///
/// Each requested `(source, destination)` tuple produces TWO rows:
/// one `kind='detail_ping'`, one `kind='detail_mtr'`. Dispatch (T45)
/// will treat each as a separate measurement; the `resolve_reuse`
/// filter (T1) structurally skips them regardless of the campaign's
/// `force_measurement` flag. The widened 4-column UNIQUE constraint
/// (`campaign_id, source_agent_id, destination_ip, kind`) makes each
/// kind a distinct row under conflict resolution.
///
/// The state transition routes through [`transition_state_in_tx`] (the
/// canonical Completed|Evaluated → Running flip, mirroring `force_pair`):
/// it restamps `started_at` so the scheduler's rotation key advances,
/// and leaves the historical `completed_at` / `evaluated_at`
/// breadcrumbs intact.
pub async fn insert_detail_pairs(
    pool: &PgPool,
    campaign_id: Uuid,
    pairs: &[(String, IpAddr)],
) -> Result<(i32, CampaignState), RepoError> {
    let mut tx = pool.begin().await?;
    let mut inserted = 0i32;
    for (source_agent_id, destination_ip) in pairs {
        for kind in ["detail_ping", "detail_mtr"] {
            let result = sqlx::query(
                r#"INSERT INTO campaign_pairs
                      (campaign_id, source_agent_id, destination_ip,
                       resolution_state, kind)
                   VALUES ($1, $2, $3, 'pending', $4::measurement_kind)
                   ON CONFLICT (campaign_id, source_agent_id, destination_ip, kind)
                     DO NOTHING"#,
            )
            .bind(campaign_id)
            .bind(source_agent_id)
            .bind(IpNetwork::from(*destination_ip))
            .bind(kind)
            .execute(&mut *tx)
            .await?;
            inserted += result.rows_affected() as i32;
        }
    }

    // Only attempt the state flip when we actually queued new work.
    // A repeat `/detail` call where every requested row already exists
    // (inserted == 0) must not restart a completed campaign — that
    // would be spurious state churn visible in the API + SSE stream.
    let post_state = if inserted == 0 {
        // No transition attempted — read the current state under the
        // transaction lock so the caller sees the actual post-commit
        // state rather than its stale pre-read.
        sqlx::query_scalar!(
            r#"SELECT state AS "state: CampaignState"
                 FROM measurement_campaigns
                WHERE id = $1
                  FOR UPDATE"#,
            campaign_id,
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RepoError::NotFound(campaign_id))?
    } else {
        match transition_state_in_tx(
            &mut tx,
            campaign_id,
            &[CampaignState::Completed, CampaignState::Evaluated],
            CampaignState::Running,
            Some("started_at"),
        )
        .await
        {
            Ok(row) => row.state,
            // Benign race: a concurrent writer already flipped the row to
            // `running` (e.g. another `/detail` or `force_pair`). The
            // detail pairs we inserted still land; we just don't need to
            // flip state again. State is known: `Running`.
            Err(RepoError::IllegalTransition {
                from: Some(CampaignState::Running),
                ..
            }) => CampaignState::Running,
            // Any other observed state (Draft, Stopped, or row absent) is
            // a real precondition mismatch; propagate so the transaction
            // rolls back rather than leaving orphan detail rows behind.
            Err(e) => return Err(e),
        }
    };

    tx.commit().await?;
    Ok((inserted, post_state))
}

// ----- Symmetry-fallback reverse query (T56-C2) -------------------------

/// Fetch reverse-direction measurements scoped to a campaign's evaluator
/// universe.
///
/// For each `(source_agent_id, destination_ip)` pair attributed to the
/// campaign, fetches any matching `(source = destination_ip's agent_id,
/// destination_ip = source_agent_id's agent ip)` row from `measurements` of
/// the **same protocol** as the campaign. Used by the symmetry-fallback
/// substitution (spec §3.1).
///
/// Returns the same [`AttributedMeasurement`] shape. Because every row in
/// `measurements` originates from an active probe, `direct_source` is always
/// [`DirectSource::ActiveProbe`].
pub async fn reverse_direction_measurements_for_campaign(
    pool: &PgPool,
    campaign_id: Uuid,
) -> Result<Vec<AttributedMeasurement>, RepoError> {
    let rows = sqlx::query!(
        r#"
        WITH camp AS (
            SELECT id, protocol FROM measurement_campaigns WHERE id = $1
        ),
        agent_pairs AS (
            SELECT DISTINCT cp.source_agent_id AS a_id, ag.ip AS a_ip,
                            cp.destination_ip AS dst_ip
            FROM campaign_pairs cp
            JOIN agents ag ON ag.id = cp.source_agent_id
            WHERE cp.campaign_id = $1
              AND cp.kind = 'campaign'
        )
        SELECT m.source_agent_id, m.destination_ip,
               m.latency_avg_ms, m.latency_stddev_ms,
               m.loss_ratio, m.mtr_id AS mtr_measurement_id,
               m.id AS measurement_id
        FROM camp c
        JOIN agent_pairs ap ON true
        JOIN agents src ON src.ip = ap.dst_ip
        JOIN measurements m
          ON m.source_agent_id = src.id
         AND m.destination_ip = ap.a_ip
         AND m.protocol = c.protocol
         AND m.measured_at > now() - interval '24 hours'
         AND m.latency_avg_ms IS NOT NULL
        "#,
        campaign_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| AttributedMeasurement {
            source_agent_id: r.source_agent_id,
            destination_ip: r.destination_ip.ip(),
            latency_avg_ms: r.latency_avg_ms,
            latency_stddev_ms: r.latency_stddev_ms,
            loss_ratio: r.loss_ratio,
            // `mtr_measurement_id` is the DTO FK to `measurements.id` for
            // the MTR-bearing row; present only when mtr_id is set on the
            // underlying measurement.
            mtr_measurement_id: r.mtr_measurement_id.map(|_| r.measurement_id),
            // All rows in `measurements` originate from active probes.
            direct_source: DirectSource::ActiveProbe,
        })
        .collect())
}
