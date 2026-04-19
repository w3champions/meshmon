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
    _pool: &PgPool,
    _id: Uuid,
    _title: Option<&str>,
    _notes: Option<&str>,
    _loss_threshold_pct: Option<f32>,
    _stddev_weight: Option<f32>,
    _evaluation_mode: Option<EvaluationMode>,
) -> Result<CampaignRow, RepoError> {
    todo!("implement partial update; use CASE/COALESCE per column")
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
    _pool: &PgPool,
    _id: Uuid,
    _edit: EditInput,
) -> Result<CampaignRow, RepoError> {
    todo!(
        "implement delta edit; lock row FOR UPDATE, apply adds/removes, \
         reset non-delta pairs if force_measurement, transition to running"
    )
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
    _pool: &PgPool,
    _id: Uuid,
    _states: &[PairResolutionState],
    _limit: i64,
) -> Result<Vec<PairRow>, RepoError> {
    todo!("implement filtered pair list; see spec 02 §7")
}

/// Count the total pairs the given sources × destinations would produce,
/// split between ones resolvable from the 24 h reuse window and ones
/// the scheduler would dispatch fresh. Never writes.
pub async fn preview_dispatch_count(
    _pool: &PgPool,
    _protocol: ProbeProtocol,
    _sources: &[String],
    _destinations: &[IpAddr],
) -> Result<PreviewCounts, RepoError> {
    todo!("implement size-preview query; see design notes above")
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
    _pool: &PgPool,
    _campaign_id: Uuid,
    _source_agent_id: &str,
    _chunk_size: i64,
) -> Result<Vec<PairRow>, RepoError> {
    todo!("implement WITH chosen AS (... FOR UPDATE SKIP LOCKED) UPDATE ... RETURNING ...")
}

/// Look up each pair in the 24 h reuse window. Returns the pairs that
/// have a reuse match as `(pair_id, measurement_id)`; unmatched pairs
/// are absent from the result and must be dispatched fresh.
pub async fn resolve_reuse(
    _pool: &PgPool,
    _pairs: &[PairRow],
    _protocol: ProbeProtocol,
) -> Result<Vec<(i64, i64)>, RepoError> {
    todo!("implement batched reuse lookup per §5.2 design notes")
}

/// Mark each `(pair_id, measurement_id)` pair as `reused`.
pub async fn apply_reuse(_pool: &PgPool, _decisions: &[(i64, i64)]) -> Result<(), RepoError> {
    todo!("UPDATE campaign_pairs SET resolution_state='reused', measurement_id=$2 via UNNEST")
}

/// Safety-net sweep that flips any `pending` pair with
/// `attempt_count >= max_attempts` to `skipped` with
/// `last_error = 'max_attempts_exceeded'`. Returns rows affected.
pub async fn expire_stale_attempts(_pool: &PgPool, _max_attempts: i16) -> Result<u64, RepoError> {
    todo!(
        "UPDATE campaign_pairs SET resolution_state='skipped', last_error='max_attempts_exceeded' \
         WHERE resolution_state='pending' AND attempt_count >= $1"
    )
}

/// Atomically flip a `running` campaign to `completed` iff no pair
/// remains in `pending` or `dispatched`. Returns `true` if the flip
/// happened. Safe to call repeatedly.
pub async fn maybe_complete(_pool: &PgPool, _campaign_id: Uuid) -> Result<bool, RepoError> {
    todo!(
        "atomically check and flip using WHERE NOT EXISTS \
         (SELECT 1 FROM campaign_pairs WHERE campaign_id=$1 \
          AND resolution_state IN ('pending','dispatched'))"
    )
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
