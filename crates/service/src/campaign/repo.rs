//! sqlx-backed CRUD + lifecycle transitions for `measurement_campaigns`
//! and `campaign_pairs`.
//!
//! Every state transition routes through [`transition_state`], which
//! issues an UPDATE gated on the expected current state. A 0-row outcome
//! surfaces as [`RepoError::IllegalTransition`] — handlers turn that
//! into HTTP 409 without a second SELECT.

use super::model::{
    CampaignRow, CampaignState, EvaluationMode, PairResolutionState, PairRow, ProbeProtocol,
};
#[allow(unused_imports)]
use crate::campaign::events::NOTIFY_CHANNEL;
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
    pub loss_threshold_pct: Option<f32>,
    /// Optional RTT-stddev weight for the evaluator.
    pub stddev_weight: Option<f32>,
    /// Optional evaluation strategy.
    pub evaluation_mode: Option<EvaluationMode>,
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
             probe_stagger_ms, force_measurement, loss_threshold_pct, stddev_weight,
             evaluation_mode, created_by)
        VALUES ($1, $2, $3::probe_protocol,
                COALESCE($4, 10::smallint), COALESCE($5, 250::smallint),
                COALESCE($6, 2000), COALESCE($7, 100),
                $8, COALESCE($9, 2.0::real), COALESCE($10, 1.0::real),
                COALESCE($11::evaluation_mode, 'optimization'::evaluation_mode),
                $12)
        RETURNING id, title, notes,
                  state AS "state: CampaignState",
                  protocol AS "protocol: ProbeProtocol",
                  probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                  force_measurement, loss_threshold_pct, stddev_weight,
                  evaluation_mode AS "evaluation_mode: EvaluationMode",
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
        input.loss_threshold_pct,
        input.stddev_weight,
        input.evaluation_mode as _,
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
               force_measurement, loss_threshold_pct, stddev_weight,
               evaluation_mode AS "evaluation_mode: EvaluationMode",
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
               force_measurement, loss_threshold_pct, stddev_weight,
               evaluation_mode AS "evaluation_mode: EvaluationMode",
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
pub async fn patch(
    pool: &PgPool,
    id: Uuid,
    title: Option<&str>,
    notes: Option<&str>,
    loss_threshold_pct: Option<f32>,
    stddev_weight: Option<f32>,
    evaluation_mode: Option<EvaluationMode>,
) -> Result<CampaignRow, RepoError> {
    let raw = sqlx::query_as!(
        CampaignRowRaw,
        r#"
        UPDATE measurement_campaigns
           SET title              = COALESCE($2, title),
               notes              = COALESCE($3, notes),
               loss_threshold_pct = COALESCE($4, loss_threshold_pct),
               stddev_weight      = COALESCE($5, stddev_weight),
               evaluation_mode    = COALESCE($6::evaluation_mode, evaluation_mode)
         WHERE id = $1
         RETURNING id, title, notes,
                   state AS "state: CampaignState",
                   protocol AS "protocol: ProbeProtocol",
                   probe_count, probe_count_detail, timeout_ms, probe_stagger_ms,
                   force_measurement, loss_threshold_pct, stddev_weight,
                   evaluation_mode AS "evaluation_mode: EvaluationMode",
                   created_by, created_at, started_at, stopped_at, completed_at, evaluated_at
        "#,
        id,
        title,
        notes,
        loss_threshold_pct,
        stddev_weight,
        evaluation_mode as Option<EvaluationMode>,
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

    // 2. Remove pairs the operator dropped.
    for (src, dst) in &edit.remove_pairs {
        let dst_net = IpNetwork::from(*dst);
        sqlx::query!(
            "DELETE FROM campaign_pairs
              WHERE campaign_id = $1
                AND source_agent_id = $2
                AND destination_ip = $3",
            id,
            src,
            dst_net
        )
        .execute(&mut *tx)
        .await?;
    }

    // 3. Insert or reset added pairs. A previously-skipped pair that is
    //    re-added resets to `pending` with cleared bookkeeping.
    if !edit.add_pairs.is_empty() {
        let srcs: Vec<&str> = edit.add_pairs.iter().map(|(s, _)| s.as_str()).collect();
        let dsts: Vec<IpNetwork> = edit
            .add_pairs
            .iter()
            .map(|(_, d)| IpNetwork::from(*d))
            .collect();
        sqlx::query!(
            "INSERT INTO campaign_pairs (campaign_id, source_agent_id, destination_ip)
             SELECT $1, src, dst
               FROM UNNEST($2::text[], $3::inet[]) AS p(src, dst)
             ON CONFLICT (campaign_id, source_agent_id, destination_ip) DO UPDATE
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
        sqlx::query!(
            "UPDATE campaign_pairs
                SET resolution_state = 'pending',
                    measurement_id   = NULL,
                    settled_at       = NULL,
                    dispatched_at    = NULL,
                    attempt_count    = 0,
                    last_error       = NULL
              WHERE campaign_id = $1
                AND resolution_state IN ('reused','succeeded','unreachable')",
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

    // TODO(T48): also dismiss any existing campaign_evaluations row for
    // this campaign once that table exists.

    tx.commit().await?;
    Ok(row)
}

/// Reset one specific pair to `pending` (clearing bookkeeping) and
/// transition the campaign back to `running`. Used by the operator's
/// "force this one pair" button on finished campaigns.
pub async fn force_pair(
    _pool: &PgPool,
    _id: Uuid,
    _source_agent_id: &str,
    _destination_ip: IpAddr,
) -> Result<CampaignRow, RepoError> {
    todo!(
        "implement: reset specified pair, transition campaign to running \
         via transition_state(expected=[Completed,Stopped,Evaluated])"
    )
}

/// List pairs for a campaign, optionally filtered to a specific set of
/// resolution states. Results ordered by id, capped at `min(limit, 500)`.
pub async fn list_pairs(
    pool: &PgPool,
    id: Uuid,
    states: &[PairResolutionState],
    limit: i64,
) -> Result<Vec<PairRow>, RepoError> {
    let bounded = limit.clamp(1, 500);
    let raws = sqlx::query_as!(
        PairRowRaw,
        r#"
        SELECT id, campaign_id, source_agent_id, destination_ip,
               resolution_state AS "resolution_state: PairResolutionState",
               measurement_id, dispatched_at, settled_at, attempt_count, last_error
          FROM campaign_pairs
         WHERE campaign_id = $1
           AND (cardinality($2::pair_resolution_state[]) = 0
                OR resolution_state = ANY($2::pair_resolution_state[]))
         ORDER BY id
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

/// Count the total pairs the given sources × destinations would produce,
/// split between ones resolvable from the 24 h reuse window and ones
/// the scheduler would dispatch fresh. Never writes.
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
                   campaign_pairs.last_error
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
            SELECT pair_id, source_agent_id, destination_ip_str
              FROM UNNEST($1::bigint[], $2::text[], $3::text[])
                     AS r(pair_id, source_agent_id, destination_ip_str)
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
                settled_at       = now()
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

/// Atomically flip a `running` campaign to `completed` iff no pair
/// remains in `pending` or `dispatched`. Returns `true` if the flip
/// happened. Safe to call repeatedly.
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

async fn transition_state_in_tx(
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
                 force_measurement, loss_threshold_pct, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
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
                 force_measurement, loss_threshold_pct, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
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
                 force_measurement, loss_threshold_pct, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
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
                 force_measurement, loss_threshold_pct, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
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
                 force_measurement, loss_threshold_pct, stddev_weight,
                 evaluation_mode AS "evaluation_mode: EvaluationMode",
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
         ON CONFLICT (campaign_id, source_agent_id, destination_ip) DO NOTHING",
        campaign_id,
        &expanded_sources as &[&str],
        &expanded_dests as &[IpNetwork],
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// sqlx-derived raw row. [`CampaignRow`] is the domain-layer clone.
#[allow(dead_code)]
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
    loss_threshold_pct: f32,
    stddev_weight: f32,
    evaluation_mode: EvaluationMode,
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
            loss_threshold_pct: r.loss_threshold_pct,
            stddev_weight: r.stddev_weight,
            evaluation_mode: r.evaluation_mode,
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
#[allow(dead_code)]
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
        }
    }
}
