//! Relational persistence for campaign evaluations.
//!
//! The evaluator's output fans out across five tables:
//!
//! - `campaign_evaluations` — parent row with summary counters and
//!   evaluation-time knob snapshots.
//! - `campaign_evaluation_candidates` — one row per transit destination
//!   (both Triple and EdgeCandidate modes).
//! - `campaign_evaluation_pair_details` — per-baseline-pair detail for
//!   Triple modes, stamped with `direct_source` provenance and
//!   substitution flags.
//! - `campaign_evaluation_edge_pair_details` — per-(X, B) best-route row
//!   for EdgeCandidate mode.
//! - `campaign_evaluation_unqualified_reasons` — reason map for
//!   destinations that never produced a qualifying pair detail.
//!
//! Writes happen inside a single transaction so the parent + children
//! land atomically. Reads are sequential queries that assemble into the
//! existing [`EvaluationDto`] wire shape.
//!
//! Every `/evaluate` call appends a fresh evaluation row; the
//! per-campaign UNIQUE constraint was dropped in the 20260424130000
//! migration so history accumulates in `campaign_evaluations`. The
//! read-path surfaces the most recent row via `ORDER BY evaluated_at
//! DESC LIMIT 1`.

use super::cursor::{PairDetailCursor, SortValue};
use super::dto::{
    EvaluationCandidateDto, EvaluationDto, EvaluationPairDetailDto, EvaluationPairDetailQuery,
    EvaluationResultsDto, PairDetailSortCol, PairDetailSortDir,
};
use super::eval::{EdgeCandidateOutputs, EvaluationOutputs, TripleEvaluationOutputs};
use super::model::{CampaignState, DirectSource, EdgeRouteKind, EvaluationMode};
use super::repo::RepoError;
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::str::FromStr;
use uuid::Uuid;

/// Map an [`EdgeRouteKind`] to the TEXT value stored in the
/// `best_route_kind` column. Matches the CHECK constraint in the
/// `20260426000000_campaigns_edge_candidate` migration.
fn edge_route_kind_to_text(k: EdgeRouteKind) -> &'static str {
    match k {
        EdgeRouteKind::Direct => "direct",
        EdgeRouteKind::OneHop => "1hop",
        EdgeRouteKind::TwoHop => "2hop",
    }
}

/// Persist the Triple-mode evaluator output (Diversity / Optimization)
/// as a new `campaign_evaluations` parent row and all child rows, inside
/// the caller's transaction. Returns the newly minted evaluation id.
///
/// The parent row is written first so its `id` is available as the FK
/// target for every child row. No UPSERT semantics — each call
/// appends, preserving the history that the
/// `campaign_evaluations_campaign_evaluated_idx` index is tuned for.
///
/// Consistency contract: the parent row's `candidates_total` /
/// `candidates_good` counters always match the child-row counts the
/// caller sees after commit. If any `destination_ip` string on the
/// evaluator output fails to parse back into an [`IpAddr`] — an
/// unreachable case in normal operation because the evaluator builds
/// those strings from `IpAddr` — the function returns
/// [`sqlx::Error::Protocol`] so the caller's tx rolls back the
/// already-inserted parent row rather than persisting skewed
/// counters.
#[allow(clippy::too_many_arguments)]
pub async fn insert_evaluation(
    tx: &mut Transaction<'_, Postgres>,
    campaign_id: Uuid,
    outputs: &TripleEvaluationOutputs,
    loss_threshold_ratio: f32,
    stddev_weight: f32,
    mode: EvaluationMode,
    max_transit_rtt_ms: Option<f64>,
    max_transit_stddev_ms: Option<f64>,
    min_improvement_ms: Option<f64>,
    min_improvement_ratio: Option<f64>,
    useful_latency_ms: Option<f32>,
    max_hops: i16,
    vm_lookback_minutes: i32,
) -> sqlx::Result<Uuid> {
    // Parent row. `evaluated_at` stamps the write wall-clock so the
    // read-path's `ORDER BY evaluated_at DESC` picks up the freshest
    // entry. The three T56 snapshot columns are written even for
    // Diversity / Optimization campaigns so a later mode switch
    // surfaces the previous knob context.
    let evaluation_id: Uuid = sqlx::query_scalar!(
        r#"INSERT INTO campaign_evaluations
              (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
               max_transit_rtt_ms, max_transit_stddev_ms,
               min_improvement_ms, min_improvement_ratio,
               useful_latency_ms, max_hops, vm_lookback_minutes,
               baseline_pair_count, candidates_total, candidates_good,
               avg_improvement_ms, evaluated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, now())
           RETURNING id"#,
        campaign_id,
        loss_threshold_ratio,
        stddev_weight,
        mode as EvaluationMode,
        max_transit_rtt_ms,
        max_transit_stddev_ms,
        min_improvement_ms,
        min_improvement_ratio,
        useful_latency_ms,
        max_hops,
        vm_lookback_minutes,
        outputs.baseline_pair_count,
        outputs.candidates_total,
        outputs.candidates_good,
        outputs.avg_improvement_ms,
    )
    .fetch_one(&mut **tx)
    .await?;

    // Candidates — keyed on `(evaluation_id, destination_ip)` so the
    // child `pair_details` FK can chain off the same tuple. The
    // evaluator emits the candidate row and its pair-detail rows in
    // lockstep `Vec`s indexed identically — see [`EvaluationOutputs`].
    if outputs.results.candidates.len() != outputs.pair_details_by_candidate.len() {
        // Defensive: the evaluator builds these in parallel inside
        // `evaluate()` so a length mismatch would be a writer-side
        // bug, not operator input. Aborting the tx is preferable to
        // silently persisting only one half.
        return Err(sqlx::Error::Protocol(format!(
            "evaluator output length mismatch: candidates={}, pair_details_by_candidate={}",
            outputs.results.candidates.len(),
            outputs.pair_details_by_candidate.len()
        )));
    }
    for (cand, bundle) in outputs
        .results
        .candidates
        .iter()
        .zip(outputs.pair_details_by_candidate.iter())
    {
        // The evaluator owns `destination_ip` string formatting so an
        // unparseable value would be a bug in this service, not
        // operator input. Abort the transaction rather than silently
        // drop the row — a skip would leave the parent row's
        // `candidates_total` counter disagreeing with the actual
        // child-row count, violating the function's consistency
        // contract documented above.
        let ip = IpAddr::from_str(&cand.destination_ip).map_err(|err| {
            tracing::error!(
                %campaign_id,
                %evaluation_id,
                destination_ip = %cand.destination_ip,
                %err,
                "candidate destination_ip failed to parse; aborting tx",
            );
            sqlx::Error::Protocol(format!(
                "unparseable candidate destination_ip {:?}",
                cand.destination_ip
            ))
        })?;
        // Defensive: the parallel-Vec contract requires the bundle's
        // parsed IP to match the candidate's stringified one. A
        // mismatch would point pair_details at the wrong candidate row.
        if bundle.destination_ip != ip {
            return Err(sqlx::Error::Protocol(format!(
                "evaluator pair_details_by_candidate IP mismatch: candidate={ip}, bundle={}",
                bundle.destination_ip
            )));
        }
        let destination_ip = IpNetwork::from(ip);
        sqlx::query!(
            r#"INSERT INTO campaign_evaluation_candidates
                  (evaluation_id, destination_ip, display_name, city,
                   country_code, asn, network_operator, is_mesh_member,
                   pairs_improved, pairs_total_considered, avg_improvement_ms,
                   avg_loss_ratio)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
            evaluation_id,
            destination_ip,
            cand.display_name.as_deref(),
            cand.city.as_deref(),
            cand.country_code.as_deref(),
            cand.asn,
            cand.network_operator.as_deref(),
            cand.is_mesh_member,
            cand.pairs_improved,
            cand.pairs_total_considered,
            cand.avg_improvement_ms,
            cand.avg_loss_ratio,
        )
        .execute(&mut **tx)
        .await?;

        // Persist the qualifying-leg set for this candidate. The
        // evaluator captures these BEFORE the storage filter so the
        // `Detail: good candidates` expansion sees every qualifying
        // triple — the post-storage `pair_details` table can drop
        // qualifying rows when `min_improvement_ms` /
        // `min_improvement_ratio` are tight.
        for (source_agent_id, destination_agent_id) in &bundle.qualifying_legs {
            sqlx::query!(
                r#"INSERT INTO campaign_evaluation_qualifying_legs
                      (evaluation_id, candidate_destination_ip,
                       source_agent_id, destination_agent_id)
                   VALUES ($1, $2, $3, $4)"#,
                evaluation_id,
                destination_ip,
                source_agent_id,
                destination_agent_id,
            )
            .execute(&mut **tx)
            .await?;
        }

        // Pair-detail rows FK to the (evaluation_id, destination_ip)
        // tuple on `campaign_evaluation_candidates`, so the insert
        // only succeeds once the candidate is in place.
        // Substitution flags + winning_x_position are populated from
        // the evaluator's output (Phase F) and written here (Phase G).
        for pd in &bundle.pair_details {
            sqlx::query!(
                r#"INSERT INTO campaign_evaluation_pair_details
                      (evaluation_id, candidate_destination_ip,
                       source_agent_id, destination_agent_id,
                       direct_rtt_ms, direct_stddev_ms, direct_loss_ratio,
                       direct_source,
                       transit_rtt_ms, transit_stddev_ms, transit_loss_ratio,
                       improvement_ms, qualifies,
                       mtr_measurement_id_ax, mtr_measurement_id_xb,
                       ax_was_substituted, xb_was_substituted,
                       direct_was_substituted, winning_x_position)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)"#,
                evaluation_id,
                destination_ip,
                pd.source_agent_id,
                pd.destination_agent_id,
                pd.direct_rtt_ms,
                pd.direct_stddev_ms,
                pd.direct_loss_ratio,
                pd.direct_source as DirectSource,
                pd.transit_rtt_ms,
                pd.transit_stddev_ms,
                pd.transit_loss_ratio,
                pd.improvement_ms,
                pd.qualifies,
                pd.mtr_measurement_id_ax,
                pd.mtr_measurement_id_xb,
                pd.ax_was_substituted,
                pd.xb_was_substituted,
                pd.direct_was_substituted,
                pd.winning_x_position.map(|v| v as i16),
            )
            .execute(&mut **tx)
            .await?;
        }
    }

    // Unqualified reasons map — keyed on the (evaluation_id,
    // destination_ip) pair; independent of the candidate table because
    // a destination can be flagged unqualified without ever producing
    // a candidate row.
    for (raw_ip, reason) in &outputs.results.unqualified_reasons {
        // Same consistency contract as the candidate loop above: a
        // parse failure here is a writer-side bug, not operator
        // input, so abort the whole tx rather than persisting a row
        // with an unreachable destination key.
        let ip = IpAddr::from_str(raw_ip).map_err(|err| {
            tracing::error!(
                %campaign_id,
                %evaluation_id,
                destination_ip = %raw_ip,
                %err,
                "unqualified_reasons destination_ip failed to parse; aborting tx",
            );
            sqlx::Error::Protocol(format!(
                "unparseable unqualified_reasons destination_ip {raw_ip:?}"
            ))
        })?;
        sqlx::query!(
            r#"INSERT INTO campaign_evaluation_unqualified_reasons
                  (evaluation_id, destination_ip, reason)
               VALUES ($1, $2, $3)"#,
            evaluation_id,
            IpNetwork::from(ip),
            reason,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(evaluation_id)
}

/// Read the most recent evaluation for a campaign, assembling the
/// parent + child rows into an [`EvaluationDto`]. Returns `Ok(None)`
/// when the campaign has never been evaluated.
///
/// Runs three queries (parent, candidates, unqualified_reasons) and
/// joins in Rust. The candidate read path is JOIN-free: every
/// candidate-level aggregate (including `avg_loss_ratio`) is sourced
/// from the persisted column on `campaign_evaluation_candidates`.
/// Per-pair detail rows are not shipped on the wire — they stream
/// through the paginated `…/candidates/{ip}/pair_details` endpoint
/// instead.
pub async fn latest_evaluation_for_campaign(
    pool: &PgPool,
    campaign_id: Uuid,
) -> sqlx::Result<Option<EvaluationDto>> {
    let parent = sqlx::query!(
        r#"SELECT id, campaign_id, evaluated_at, loss_threshold_ratio, stddev_weight,
                  evaluation_mode AS "evaluation_mode: EvaluationMode",
                  max_transit_rtt_ms, max_transit_stddev_ms,
                  min_improvement_ms, min_improvement_ratio,
                  useful_latency_ms, max_hops, vm_lookback_minutes,
                  baseline_pair_count, candidates_total, candidates_good,
                  avg_improvement_ms
             FROM campaign_evaluations
            WHERE campaign_id = $1
            ORDER BY evaluated_at DESC
            LIMIT 1"#,
        campaign_id,
    )
    .fetch_optional(pool)
    .await?;

    let Some(parent) = parent else {
        return Ok(None);
    };

    // Candidates in composite-score order. The evaluator persists them
    // without a sort key of their own, so the read-path re-sorts by
    // `(pairs_improved/pairs_total_considered * avg_improvement_ms)`
    // implied by `avg_improvement_ms DESC` + `pairs_improved DESC` as
    // a stable approximation. The frontend doesn't rely on an exact
    // sort beyond "good candidates first"; the tiebreaker falls back
    // to `destination_ip` so the order is deterministic across reads.
    //
    // `avg_loss_ratio` is sourced from the persisted column — the
    // evaluator computes it from the pre-storage-filter accumulator
    // so the headline reading is independent of how aggressively
    // `min_improvement_ms` / `min_improvement_ratio` prune detail rows.
    //
    // The LEFT JOIN against `agents_with_catalogue` projects `agent_id`
    // for mesh-member candidates. For EdgeCandidate rows the `agent_id`
    // column is written directly by the evaluator; for Triple rows the
    // column is NULL in the candidates table and the JOIN fills it from
    // the live agent catalogue so downstream callers always have it.
    let candidate_rows = sqlx::query!(
        r#"SELECT c.destination_ip,
                  c.display_name,
                  c.city,
                  c.country_code,
                  c.asn,
                  c.network_operator,
                  c.is_mesh_member,
                  c.pairs_improved,
                  c.pairs_total_considered,
                  c.avg_improvement_ms,
                  c.avg_loss_ratio,
                  c.website,
                  c.notes,
                  c.hostname,
                  COALESCE(c.agent_id, a.agent_id) AS agent_id,
                  c.coverage_count,
                  c.destinations_total,
                  c.mean_ms_under_t,
                  c.coverage_weighted_ping_ms,
                  c.direct_share,
                  c.onehop_share,
                  c.twohop_share,
                  c.has_real_x_source_data
             FROM campaign_evaluation_candidates c
             LEFT JOIN agents_with_catalogue a
                    ON a.ip = c.destination_ip
            WHERE c.evaluation_id = $1
            ORDER BY c.pairs_improved DESC,
                     COALESCE(c.avg_improvement_ms, 0.0) DESC,
                     c.destination_ip ASC"#,
        parent.id,
    )
    .fetch_all(pool)
    .await?;

    let reason_rows = sqlx::query!(
        r#"SELECT destination_ip, reason
             FROM campaign_evaluation_unqualified_reasons
            WHERE evaluation_id = $1"#,
        parent.id,
    )
    .fetch_all(pool)
    .await?;

    // Assemble candidates. `composite_score` isn't persisted (it's a
    // derivable read-time value); recompute it from the persisted
    // counters so the wire DTO matches what the evaluator emits at
    // `/evaluate` time.
    let mut candidates: Vec<EvaluationCandidateDto> = Vec::with_capacity(candidate_rows.len());
    for c in candidate_rows {
        let cand_ip = c.destination_ip.ip();
        let composite_score = if parent.baseline_pair_count > 0 {
            (c.pairs_improved as f32 / parent.baseline_pair_count as f32)
                * c.avg_improvement_ms.unwrap_or(0.0)
        } else {
            0.0
        };
        candidates.push(EvaluationCandidateDto {
            destination_ip: cand_ip.to_string(),
            display_name: c.display_name,
            city: c.city,
            country_code: c.country_code,
            asn: c.asn,
            network_operator: c.network_operator,
            is_mesh_member: c.is_mesh_member,
            pairs_improved: c.pairs_improved,
            pairs_total_considered: c.pairs_total_considered,
            avg_improvement_ms: c.avg_improvement_ms,
            avg_loss_ratio: c.avg_loss_ratio,
            composite_score: Some(composite_score),
            hostname: c.hostname,
            website: c.website,
            notes: c.notes,
            agent_id: c.agent_id,
            coverage_count: c.coverage_count,
            destinations_total: c.destinations_total,
            mean_ms_under_t: c.mean_ms_under_t,
            coverage_weighted_ping_ms: c.coverage_weighted_ping_ms,
            direct_share: c.direct_share,
            onehop_share: c.onehop_share,
            twohop_share: c.twohop_share,
            has_real_x_source_data: c.has_real_x_source_data,
        });
    }

    let unqualified_reasons: BTreeMap<String, String> = reason_rows
        .into_iter()
        .map(|r| (r.destination_ip.ip().to_string(), r.reason))
        .collect();

    Ok(Some(EvaluationDto {
        campaign_id: parent.campaign_id,
        evaluated_at: parent.evaluated_at,
        loss_threshold_ratio: parent.loss_threshold_ratio,
        stddev_weight: parent.stddev_weight,
        evaluation_mode: parent.evaluation_mode,
        max_transit_rtt_ms: parent.max_transit_rtt_ms,
        max_transit_stddev_ms: parent.max_transit_stddev_ms,
        min_improvement_ms: parent.min_improvement_ms,
        min_improvement_ratio: parent.min_improvement_ratio,
        useful_latency_ms: parent.useful_latency_ms,
        max_hops: parent.max_hops,
        vm_lookback_minutes: parent.vm_lookback_minutes,
        baseline_pair_count: parent.baseline_pair_count,
        candidates_total: parent.candidates_total,
        candidates_good: parent.candidates_good,
        avg_improvement_ms: parent.avg_improvement_ms,
        results: EvaluationResultsDto {
            candidates,
            unqualified_reasons,
        },
    }))
}

/// Persist one EdgeCandidate evaluation: parent `campaign_evaluations`
/// row + per-candidate `campaign_evaluation_candidates` rows + per-(X, B)
/// `campaign_evaluation_edge_pair_details` rows, all inside the caller's
/// transaction.
#[allow(clippy::too_many_arguments)]
async fn persist_edge_candidate_evaluation(
    tx: &mut Transaction<'_, Postgres>,
    campaign_id: Uuid,
    outputs: EdgeCandidateOutputs,
    loss_threshold_ratio: f32,
    stddev_weight: f32,
    mode: EvaluationMode,
    useful_latency_ms: Option<f32>,
    max_hops: i16,
    vm_lookback_minutes: i32,
) -> sqlx::Result<Uuid> {
    let candidates_total = outputs.candidates.len() as i32;
    // "Good" candidates: those with at least one qualifying pair.
    let candidates_good = outputs
        .candidates
        .iter()
        .filter(|c| c.coverage_count > 0)
        .count() as i32;

    let evaluation_id: Uuid = sqlx::query_scalar!(
        r#"INSERT INTO campaign_evaluations
              (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
               useful_latency_ms, max_hops, vm_lookback_minutes,
               baseline_pair_count, candidates_total, candidates_good,
               evaluated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8, $9, now())
           RETURNING id"#,
        campaign_id,
        loss_threshold_ratio,
        stddev_weight,
        mode as EvaluationMode,
        useful_latency_ms,
        max_hops,
        vm_lookback_minutes,
        candidates_total,
        candidates_good,
    )
    .fetch_one(&mut **tx)
    .await?;

    for c in outputs.candidates {
        let candidate_ip = IpNetwork::from(c.candidate_ip);

        sqlx::query!(
            r#"INSERT INTO campaign_evaluation_candidates (
                   evaluation_id, destination_ip,
                   display_name, city, country_code, asn, network_operator,
                   website, notes, hostname,
                   is_mesh_member, agent_id,
                   coverage_count, destinations_total, mean_ms_under_t,
                   coverage_weighted_ping_ms, direct_share, onehop_share, twohop_share,
                   has_real_x_source_data
               ) VALUES (
                   $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                   $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
               )"#,
            evaluation_id,
            candidate_ip,
            c.display_name,
            c.city,
            c.country_code,
            c.asn,
            c.network_operator,
            c.website,
            c.notes,
            c.hostname,
            c.is_mesh_member,
            c.agent_id,
            c.coverage_count,
            c.destinations_total,
            c.mean_ms_under_t,
            c.coverage_weighted_ping_ms,
            c.direct_share,
            c.onehop_share,
            c.twohop_share,
            c.has_real_x_source_data,
        )
        .execute(&mut **tx)
        .await?;

        for pair in c.pair_details {
            let legs_jsonb = serde_json::to_value(&pair.best_route_legs).map_err(|e| {
                sqlx::Error::Protocol(format!("failed to serialize best_route_legs: {e}"))
            })?;
            let kind_text = edge_route_kind_to_text(pair.best_route_kind);

            sqlx::query!(
                r#"INSERT INTO campaign_evaluation_edge_pair_details (
                       evaluation_id, candidate_ip, destination_agent_id,
                       best_route_ms, best_route_loss_ratio, best_route_stddev_ms,
                       best_route_kind, best_route_intermediaries, best_route_legs,
                       qualifies_under_t, is_unreachable
                   ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
                evaluation_id,
                candidate_ip,
                pair.destination_agent_id,
                pair.best_route_ms,
                pair.best_route_loss_ratio,
                pair.best_route_stddev_ms,
                kind_text,
                &pair.best_route_intermediaries[..],
                legs_jsonb,
                pair.qualifies_under_t,
                pair.is_unreachable,
            )
            .execute(&mut **tx)
            .await?;
        }
    }

    Ok(evaluation_id)
}

/// Orchestrate a full `/evaluate` persistence pass: lock the parent
/// campaign row, re-check its state, persist the evaluator output, and
/// promote `completed → evaluated` — all inside a single transaction.
///
/// The `SELECT ... FOR UPDATE` guards against a concurrent `/detail`
/// flipping the campaign to `running` between the handler's advisory
/// state check and the insert. A lost gate surfaces as
/// [`RepoError::IllegalTransition`].
///
/// Returns the newly minted `campaign_evaluations.id`. Callers that
/// need the full wire DTO follow up with
/// [`latest_evaluation_for_campaign`] — it's one extra read, but the
/// alternative (returning the just-written child rows) duplicates the
/// assembly logic without real benefit.
#[allow(clippy::too_many_arguments)]
pub async fn persist_evaluation(
    pool: &PgPool,
    campaign_id: Uuid,
    outputs: &EvaluationOutputs,
    loss_threshold_ratio: f32,
    stddev_weight: f32,
    mode: EvaluationMode,
    max_transit_rtt_ms: Option<f64>,
    max_transit_stddev_ms: Option<f64>,
    min_improvement_ms: Option<f64>,
    min_improvement_ratio: Option<f64>,
    useful_latency_ms: Option<f32>,
    max_hops: i16,
    vm_lookback_minutes: i32,
) -> Result<Uuid, RepoError> {
    let mut tx = pool.begin().await?;

    // Re-read the campaign state under a row lock. The handler's
    // pre-flight `repo::get` is advisory — without the lock, a
    // concurrent `/detail` that flips to `running` between read and
    // insert would still let this call persist against a now-running
    // campaign.
    let locked_state: Option<CampaignState> = sqlx::query_scalar!(
        r#"SELECT state AS "state: CampaignState"
             FROM measurement_campaigns
            WHERE id = $1
              FOR UPDATE"#,
        campaign_id,
    )
    .fetch_optional(&mut *tx)
    .await?;
    let locked_state = match locked_state {
        Some(s) => s,
        None => return Err(RepoError::NotFound(campaign_id)),
    };
    if !matches!(
        locked_state,
        CampaignState::Completed | CampaignState::Evaluated
    ) {
        return Err(RepoError::IllegalTransition {
            campaign_id,
            from: Some(locked_state),
            expected: vec![CampaignState::Completed, CampaignState::Evaluated],
        });
    }

    // Dispatch on the evaluator output variant.
    let evaluation_id = match outputs {
        EvaluationOutputs::Triple(triple) => {
            insert_evaluation(
                &mut tx,
                campaign_id,
                triple,
                loss_threshold_ratio,
                stddev_weight,
                mode,
                max_transit_rtt_ms,
                max_transit_stddev_ms,
                min_improvement_ms,
                min_improvement_ratio,
                useful_latency_ms,
                max_hops,
                vm_lookback_minutes,
            )
            .await?
        }
        EvaluationOutputs::EdgeCandidate(edge) => {
            persist_edge_candidate_evaluation(
                &mut tx,
                campaign_id,
                edge.clone(),
                loss_threshold_ratio,
                stddev_weight,
                mode,
                useful_latency_ms,
                max_hops,
                vm_lookback_minutes,
            )
            .await?
        }
    };

    // Promote `completed → evaluated` on first evaluate; otherwise just
    // restamp `measurement_campaigns.evaluated_at` so UI consumers see
    // a fresh timestamp. Splitting the branches keeps
    // `measurement_campaigns_notify` (which is `AFTER UPDATE OF state`)
    // silent on repeat evaluates — touching `state` unconditionally
    // would fire a redundant `campaign_state_changed` frame on every
    // retune.
    match locked_state {
        CampaignState::Completed => {
            sqlx::query!(
                r#"UPDATE measurement_campaigns
                      SET state = 'evaluated', evaluated_at = now()
                    WHERE id = $1 AND state = 'completed'"#,
                campaign_id,
            )
            .execute(&mut *tx)
            .await?;
        }
        CampaignState::Evaluated => {
            sqlx::query!(
                r#"UPDATE measurement_campaigns
                      SET evaluated_at = now()
                    WHERE id = $1 AND state = 'evaluated'"#,
                campaign_id,
            )
            .execute(&mut *tx)
            .await?;
        }
        // Unreachable given the gate above, but keep the fallback so a
        // future refactor that widens the gate without updating this
        // match cannot silently miss the state promotion.
        _ => {}
    }

    tx.commit().await?;
    Ok(evaluation_id)
}

/// Delete all `campaign_evaluations` rows for `campaign_id` and, if the
/// campaign is currently in `evaluated` state, flip it back to `completed`.
///
/// Called by the `PATCH /api/campaigns/{id}` handler whenever one of the
/// evaluator knobs (`useful_latency_ms`, `max_hops`, `vm_lookback_minutes`,
/// `loss_threshold_ratio`, `stddev_weight`) changes value, because the
/// stored evaluation result would be inconsistent with the new settings.
///
/// The operation is a no-op when there are no evaluation rows or the
/// campaign is not in `evaluated` state (e.g. already `completed`,
/// `running`, etc.).
pub async fn dismiss_evaluation(pool: &PgPool, campaign_id: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    // Dynamic queries (not `sqlx::query!` macros) so no offline-cache
    // regeneration is required for these two new statements.
    sqlx::query("DELETE FROM campaign_evaluations WHERE campaign_id = $1")
        .bind(campaign_id)
        .execute(&mut *tx)
        .await?;

    // Flip `evaluated → completed` so the state machine is consistent
    // after the evaluation row is gone. The WHERE-clause guard means this
    // is a no-op when the campaign is in any other state.
    sqlx::query(
        "UPDATE measurement_campaigns \
              SET state        = 'completed', \
                  evaluated_at = NULL \
            WHERE id    = $1 \
              AND state = 'evaluated'",
    )
    .bind(campaign_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Outcome of [`latest_pair_details_for_candidate`], discriminating the
/// "campaign exists / has been evaluated / has the candidate" path from
/// the three 404 paths the handler renders distinct error codes for.
#[derive(Debug)]
pub enum PairDetailLookup {
    /// Happy path — the page-and-total result. `next_cursor` is `Some`
    /// when the underlying scan returned `limit` rows (a strict-less-
    /// than test would also be defensible; this conservative variant
    /// can yield one redundant empty page on the boundary, which the
    /// frontend already handles).
    Found {
        /// Pair-detail rows for this page, ordered per the request.
        entries: Vec<EvaluationPairDetailDto>,
        /// Total rows across the full filtered result set, ignoring
        /// the cursor.
        total: u64,
        /// Opaque cursor for the next page, or `None` at end-of-result.
        next_cursor: Option<String>,
    },
    /// `measurement_campaigns.id = $campaign_id` not found.
    CampaignNotFound,
    /// Campaign exists but has never been evaluated. Maps to 404
    /// `no_evaluation` on the wire.
    NoEvaluation,
    /// Latest evaluation does not include `dest_ip` as a candidate.
    /// Maps to 404 `not_a_candidate`.
    NotACandidate,
}

/// SQL fragment for the requested sort column. Returned as a `&'static str`
/// so the format!() that splices it into the query never sees user input.
fn sort_col_sql(c: PairDetailSortCol) -> &'static str {
    match c {
        PairDetailSortCol::ImprovementMs => "improvement_ms",
        PairDetailSortCol::DirectRttMs => "direct_rtt_ms",
        PairDetailSortCol::DirectStddevMs => "direct_stddev_ms",
        PairDetailSortCol::TransitRttMs => "transit_rtt_ms",
        PairDetailSortCol::TransitStddevMs => "transit_stddev_ms",
        PairDetailSortCol::DirectLossRatio => "direct_loss_ratio",
        PairDetailSortCol::TransitLossRatio => "transit_loss_ratio",
        PairDetailSortCol::SourceAgentId => "source_agent_id",
        PairDetailSortCol::DestinationAgentId => "destination_agent_id",
        PairDetailSortCol::Qualifies => "qualifies",
    }
}

/// Page through the most recent evaluation's `campaign_evaluation_pair_details`
/// rows for a given candidate destination, with server-side sort,
/// runtime filters, and an opaque keyset cursor for pagination.
///
/// The cursor predicate is hand-built per sort direction:
/// - `desc`: `(col < cursor_v) OR (col = cursor_v AND src > cursor_src) OR
///   (col = cursor_v AND src = cursor_src AND dest > cursor_dest)`
/// - `asc`: flip the leading `<` to `>`. The
///   `(source_agent_id, destination_agent_id)` tiebreak stays `>` in
///   both directions because the composite PK uniqueness inside a single
///   `(evaluation, candidate)` is what terminates the walk.
///
/// `total` comes from a sibling `COUNT(*)` query that shares every
/// non-cursor WHERE term, so the operator-facing "showing N of TOTAL"
/// stays stable across pages.
///
/// The ratio gate uses `direct_rtt_ms <= 0` as an auto-pass — mirrors
/// the I2 evaluator's storage-filter semantics. A `NULLIF(direct_rtt_ms, 0)`
/// formulation would silently drop degenerate-baseline rows, which the
/// `direct_rtt_zero_ratio_auto_passes` regression test covers.
pub async fn latest_pair_details_for_candidate(
    pool: &PgPool,
    campaign_id: Uuid,
    dest_ip: IpAddr,
    query: &EvaluationPairDetailQuery,
) -> sqlx::Result<PairDetailLookup> {
    // Cheap existence check up-front so the three 404 codes the wire
    // surface promises never collapse into a single "no results" path.
    let campaign_exists: Option<Uuid> = sqlx::query_scalar!(
        r#"SELECT id FROM measurement_campaigns WHERE id = $1"#,
        campaign_id,
    )
    .fetch_optional(pool)
    .await?;
    if campaign_exists.is_none() {
        return Ok(PairDetailLookup::CampaignNotFound);
    }

    let evaluation_id: Option<Uuid> = sqlx::query_scalar!(
        r#"SELECT id FROM campaign_evaluations
            WHERE campaign_id = $1
            ORDER BY evaluated_at DESC
            LIMIT 1"#,
        campaign_id,
    )
    .fetch_optional(pool)
    .await?;
    let Some(evaluation_id) = evaluation_id else {
        return Ok(PairDetailLookup::NoEvaluation);
    };

    let dest_inet = IpNetwork::from(dest_ip);
    let candidate_exists: Option<IpNetwork> = sqlx::query_scalar!(
        r#"SELECT destination_ip
             FROM campaign_evaluation_candidates
            WHERE evaluation_id = $1 AND destination_ip = $2"#,
        evaluation_id,
        dest_inet,
    )
    .fetch_optional(pool)
    .await?;
    if candidate_exists.is_none() {
        return Ok(PairDetailLookup::NotACandidate);
    }

    // Build the filter WHERE fragment with five always-bound parameters.
    // The `$N::TYPE IS NULL` form lets the planner constant-fold absent
    // filters without us branching the SQL builder per filter.
    //
    // Ratio gate uses `<= 0 OR ratio >= $N` — the `<= 0` arm is the
    // auto-pass for degenerate-baseline rows. NULLIF would silently
    // drop them.
    let filter_sql = "\
        AND ($3::float8 IS NULL OR pd.improvement_ms >= $3) \
        AND ($4::float8 IS NULL \
             OR pd.direct_rtt_ms <= 0 \
             OR pd.improvement_ms / pd.direct_rtt_ms >= $4) \
        AND ($5::float8 IS NULL OR pd.transit_rtt_ms <= $5) \
        AND ($6::float8 IS NULL OR pd.transit_stddev_ms <= $6) \
        AND ($7::bool   IS NULL OR pd.qualifies = $7)";

    let sort_col = sort_col_sql(query.sort);
    let sort_dir_kw = match query.dir {
        PairDetailSortDir::Asc => "ASC",
        PairDetailSortDir::Desc => "DESC",
    };
    // For `asc`, the leading inequality on the sort column flips to `>`;
    // for `desc`, it stays `<`. The tiebreak on
    // `(source_agent_id, destination_agent_id)` is always `>` because
    // we always walk forward through the unique composite-PK tail.
    let leading_cmp = match query.dir {
        PairDetailSortDir::Asc => ">",
        PairDetailSortDir::Desc => "<",
    };

    // Cursor predicate: bound to $8 ($9, $10) with type-specific casts
    // so the planner doesn't reject the comparison. We always emit the
    // same three placeholders and bind a typed-value or a typed-NULL —
    // keeps the parameter count constant across cursor / no-cursor.
    let cursor_predicate = if query.cursor.is_some() {
        format!(
            "AND ( pd.{col} {cmp} $8 \
                OR (pd.{col} = $8 AND pd.source_agent_id > $9) \
                OR (pd.{col} = $8 AND pd.source_agent_id = $9 AND pd.destination_agent_id > $10) )",
            col = sort_col,
            cmp = leading_cmp,
        )
    } else {
        // Three throwaway parameter slots so the builder below stays
        // constant-arity. Casting NULL keeps the planner happy.
        "AND ($8::text IS NULL AND $9::text IS NULL AND $10::text IS NULL)".to_string()
    };

    // Decode the cursor into typed parts. Decoding is the handler's job
    // (it owns the wire-error mapping); here we receive an already-
    // decoded `PairDetailCursor` via the query's pre-validated form.
    // The handler validates the `sort_col` matches before we get here —
    // the `expect` is unreachable in operational code but defends a
    // future refactor that wires this function up directly.
    let cursor = query.cursor.as_ref().map(|raw| {
        PairDetailCursor::decode(raw, query.sort)
            .expect("handler must validate cursor before calling latest_pair_details_for_candidate")
    });

    // Bind the cursor's leading sort value into one of three optional
    // placeholders typed to the column's SQL type. Only one is ever
    // bound to a real value at a time; the others stay NULL.
    let (cursor_f64, cursor_text, cursor_bool) = match cursor.as_ref() {
        Some(c) => match &c.sort_value {
            SortValue::F64(v) => (Some(*v), None, None),
            SortValue::String(s) => (None, Some(s.clone()), None),
            SortValue::Bool(b) => (None, None, Some(*b)),
        },
        None => (None, None, None),
    };
    let cursor_src: Option<String> = cursor.as_ref().map(|c| c.source_agent_id.clone());
    let cursor_dest: Option<String> = cursor.as_ref().map(|c| c.destination_agent_id.clone());

    // Single value column: pick the typed bind that matches the sort
    // column. The cursor's typed variant must agree with the column's
    // SQL type — the handler enforces this match via the sort whitelist
    // and the cursor's `sort_col` field already validated above.
    //
    // We splice the typed cast into the SQL so a numeric column is
    // compared against a numeric placeholder, not a coerced text.
    //
    // Exhaustive match (no wildcard) so adding a new
    // [`PairDetailSortCol`] variant is a hard compile-time decision —
    // a wildcard would silently absorb a future non-numeric column
    // into the float8 cast and produce wrong-type cursor binds.
    let cursor_value_sql_cast = match query.sort {
        PairDetailSortCol::SourceAgentId | PairDetailSortCol::DestinationAgentId => "$8::text",
        PairDetailSortCol::Qualifies => "$8::bool",
        PairDetailSortCol::ImprovementMs
        | PairDetailSortCol::DirectRttMs
        | PairDetailSortCol::DirectStddevMs
        | PairDetailSortCol::TransitRttMs
        | PairDetailSortCol::TransitStddevMs
        | PairDetailSortCol::DirectLossRatio
        | PairDetailSortCol::TransitLossRatio => "$8::float8",
    };

    // Substitute the typed `$8` placeholder so the casts in the cursor
    // predicate match the column type. We do this by string replacement
    // on the predicate string (the only `$8` reference), since the
    // surrounding parameter binding stays at slot 8 for every sort.
    let cursor_predicate = cursor_predicate.replace("$8", cursor_value_sql_cast);

    let sql = format!(
        "SELECT \
            candidate_destination_ip, source_agent_id, destination_agent_id, \
            direct_rtt_ms, direct_stddev_ms, direct_loss_ratio, \
            direct_source::text AS direct_source_text, \
            transit_rtt_ms, transit_stddev_ms, transit_loss_ratio, \
            improvement_ms, qualifies, \
            mtr_measurement_id_ax, mtr_measurement_id_xb \
         FROM campaign_evaluation_pair_details pd \
         WHERE pd.evaluation_id = $1 AND pd.candidate_destination_ip = $2 \
         {filters} {cursor} \
         ORDER BY pd.{sort_col} {dir}, pd.source_agent_id ASC, pd.destination_agent_id ASC \
         LIMIT $11",
        filters = filter_sql,
        cursor = cursor_predicate,
        sort_col = sort_col,
        dir = sort_dir_kw,
    );

    // Cap the limit at 500 rows. The handler validates `> 500` as
    // `invalid_filter` already, so any oversize value reaching here is
    // a defense-in-depth issue, not a 400.
    let bound_limit = query.limit.min(500) as i64;

    let mut q = sqlx::query(&sql)
        .bind(evaluation_id)
        .bind(dest_inet)
        .bind(query.min_improvement_ms)
        .bind(query.min_improvement_ratio)
        .bind(query.max_transit_rtt_ms)
        .bind(query.max_transit_stddev_ms)
        .bind(query.qualifies_only);
    // Slot 8 — typed by sort column. Exhaustive (no wildcard) for the
    // same reason as `cursor_value_sql_cast`: a future non-numeric
    // sort column must not silently fall through into the float8 bind.
    q = match query.sort {
        PairDetailSortCol::SourceAgentId | PairDetailSortCol::DestinationAgentId => {
            q.bind(cursor_text.clone())
        }
        PairDetailSortCol::Qualifies => q.bind(cursor_bool),
        PairDetailSortCol::ImprovementMs
        | PairDetailSortCol::DirectRttMs
        | PairDetailSortCol::DirectStddevMs
        | PairDetailSortCol::TransitRttMs
        | PairDetailSortCol::TransitStddevMs
        | PairDetailSortCol::DirectLossRatio
        | PairDetailSortCol::TransitLossRatio => q.bind(cursor_f64),
    };
    let q = q
        .bind(cursor_src.clone())
        .bind(cursor_dest.clone())
        .bind(bound_limit);

    let rows = q.fetch_all(pool).await?;

    // NaN-guard pass. Stored data should never carry non-finite values
    // — the evaluator guards against them at write time — but a corrupt
    // upstream feed must not propagate NaN/Infinity into the React `Δ %`
    // formatter and crash the render. Skip the row, log, continue.
    let mut entries: Vec<EvaluationPairDetailDto> = Vec::with_capacity(rows.len());
    for row in &rows {
        let cand_inet: IpNetwork = row.try_get("candidate_destination_ip")?;
        let direct_rtt_ms: f32 = row.try_get("direct_rtt_ms")?;
        let direct_stddev_ms: f32 = row.try_get("direct_stddev_ms")?;
        let direct_loss_ratio: f32 = row.try_get("direct_loss_ratio")?;
        let transit_rtt_ms: f32 = row.try_get("transit_rtt_ms")?;
        let transit_stddev_ms: f32 = row.try_get("transit_stddev_ms")?;
        let transit_loss_ratio: f32 = row.try_get("transit_loss_ratio")?;
        let improvement_ms: f32 = row.try_get("improvement_ms")?;
        let qualifies: bool = row.try_get("qualifies")?;
        let source_agent_id: String = row.try_get("source_agent_id")?;
        let destination_agent_id: String = row.try_get("destination_agent_id")?;
        let direct_source_text: String = row.try_get("direct_source_text")?;
        let mtr_measurement_id_ax: Option<i64> = row.try_get("mtr_measurement_id_ax")?;
        let mtr_measurement_id_xb: Option<i64> = row.try_get("mtr_measurement_id_xb")?;

        let finite_ok = [
            direct_rtt_ms,
            direct_stddev_ms,
            direct_loss_ratio,
            transit_rtt_ms,
            transit_stddev_ms,
            transit_loss_ratio,
            improvement_ms,
        ]
        .iter()
        .all(|v| v.is_finite());
        if !finite_ok {
            tracing::warn!(
                %campaign_id,
                %evaluation_id,
                %source_agent_id,
                %destination_agent_id,
                "campaign::pair_details: skipping row with non-finite numeric field",
            );
            continue;
        }

        // Parse the enum from text so we don't have to type-erase it
        // through dynamic sqlx::query. A bad value here would be a
        // schema bug, not operator input.
        let direct_source = match direct_source_text.as_str() {
            "active_probe" => DirectSource::ActiveProbe,
            "vm_continuous" => DirectSource::VmContinuous,
            other => {
                tracing::error!(
                    %campaign_id,
                    %evaluation_id,
                    direct_source = %other,
                    "campaign::pair_details: unknown direct_source enum text",
                );
                return Err(sqlx::Error::Protocol(format!(
                    "unknown direct_source enum text {other:?}"
                )));
            }
        };

        entries.push(EvaluationPairDetailDto {
            source_agent_id,
            destination_agent_id,
            destination_ip: cand_inet.ip().to_string(),
            direct_rtt_ms,
            direct_stddev_ms,
            direct_loss_ratio,
            direct_source,
            transit_rtt_ms,
            transit_stddev_ms,
            transit_loss_ratio,
            improvement_ms,
            qualifies,
            mtr_measurement_id_ax,
            mtr_measurement_id_xb,
            destination_hostname: None,
            ax_was_substituted: None,
            xb_was_substituted: None,
            direct_was_substituted: None,
            winning_x_position: None,
        });
    }

    // Sibling COUNT(*). Same WHERE as the page query, minus the cursor
    // predicate. Counting the unfiltered total would defeat the
    // operator's status bar; counting the cursor-filtered total would
    // make the bar move under the operator's feet.
    let count_sql = format!(
        "SELECT COUNT(*) AS c \
         FROM campaign_evaluation_pair_details pd \
         WHERE pd.evaluation_id = $1 AND pd.candidate_destination_ip = $2 \
         {filters}",
        filters = filter_sql,
    );
    let total: i64 = sqlx::query_scalar(&count_sql)
        .bind(evaluation_id)
        .bind(dest_inet)
        .bind(query.min_improvement_ms)
        .bind(query.min_improvement_ratio)
        .bind(query.max_transit_rtt_ms)
        .bind(query.max_transit_stddev_ms)
        .bind(query.qualifies_only)
        .fetch_one(pool)
        .await?;

    // Mint the next-page cursor from the last entry of this page when
    // we delivered a full page. An empty page implies end-of-result;
    // a short page (`< limit`) likewise.
    let next_cursor = if entries.is_empty() || entries.len() < bound_limit as usize {
        None
    } else {
        let last = entries.last().expect("len >= 1 here");
        let sort_value = match query.sort {
            PairDetailSortCol::ImprovementMs => SortValue::F64(last.improvement_ms as f64),
            PairDetailSortCol::DirectRttMs => SortValue::F64(last.direct_rtt_ms as f64),
            PairDetailSortCol::DirectStddevMs => SortValue::F64(last.direct_stddev_ms as f64),
            PairDetailSortCol::TransitRttMs => SortValue::F64(last.transit_rtt_ms as f64),
            PairDetailSortCol::TransitStddevMs => SortValue::F64(last.transit_stddev_ms as f64),
            PairDetailSortCol::DirectLossRatio => SortValue::F64(last.direct_loss_ratio as f64),
            PairDetailSortCol::TransitLossRatio => SortValue::F64(last.transit_loss_ratio as f64),
            PairDetailSortCol::SourceAgentId => SortValue::String(last.source_agent_id.clone()),
            PairDetailSortCol::DestinationAgentId => {
                SortValue::String(last.destination_agent_id.clone())
            }
            PairDetailSortCol::Qualifies => SortValue::Bool(last.qualifies),
        };
        Some(
            PairDetailCursor {
                sort_col: query.sort,
                sort_value,
                source_agent_id: last.source_agent_id.clone(),
                destination_agent_id: last.destination_agent_id.clone(),
            }
            .encode(),
        )
    };

    Ok(PairDetailLookup::Found {
        entries,
        total: total.max(0) as u64,
        next_cursor,
    })
}

/// One row of `(candidate_destination_ip, source_agent_id, destination_agent_id)`
/// for every qualifying pair-detail attached to the campaign's most
/// recent evaluation. Used by the `/detail?scope=good_candidates`
/// handler to expand a candidate's qualifying triples into
/// `(source_agent_id, transit_ip)` and `(destination_agent_id,
/// transit_ip)` measurement targets.
#[derive(Debug, Clone)]
pub struct GoodCandidatePairLeg {
    /// Transit candidate destination IP (X). Equal to the matching
    /// candidate's `destination_ip`.
    pub candidate_destination_ip: IpAddr,
    /// Source agent of the baseline pair (A).
    pub source_agent_id: String,
    /// Destination agent of the baseline pair (B).
    pub destination_agent_id: String,
}

/// Expand the qualifying-leg set for the campaign's most recent
/// evaluation. Used by [`crate::campaign::handlers::detail`]'s
/// `DetailScope::GoodCandidates` branch to drive measurement-target
/// dispatch. Returns `Ok(None)` when the campaign has never been
/// evaluated.
///
/// Reads `campaign_evaluation_qualifying_legs` directly — the
/// evaluator populates that table from the pre-storage-filter
/// qualifying set, so the dispatch sees every triple a candidate
/// scored as `qualifies = true` regardless of how aggressively
/// `min_improvement_ms` / `min_improvement_ratio` prune the
/// `campaign_evaluation_pair_details` mirror table.
pub async fn good_candidate_pair_legs(
    pool: &PgPool,
    campaign_id: Uuid,
) -> sqlx::Result<Option<Vec<GoodCandidatePairLeg>>> {
    let evaluation_id: Option<Uuid> = sqlx::query_scalar!(
        r#"SELECT id FROM campaign_evaluations
            WHERE campaign_id = $1
            ORDER BY evaluated_at DESC
            LIMIT 1"#,
        campaign_id,
    )
    .fetch_optional(pool)
    .await?;
    let Some(evaluation_id) = evaluation_id else {
        return Ok(None);
    };

    let rows = sqlx::query!(
        r#"SELECT candidate_destination_ip, source_agent_id, destination_agent_id
             FROM campaign_evaluation_qualifying_legs
            WHERE evaluation_id = $1"#,
        evaluation_id,
    )
    .fetch_all(pool)
    .await?;

    Ok(Some(
        rows.into_iter()
            .map(|r| GoodCandidatePairLeg {
                candidate_destination_ip: r.candidate_destination_ip.ip(),
                source_agent_id: r.source_agent_id,
                destination_agent_id: r.destination_agent_id,
            })
            .collect(),
    ))
}
