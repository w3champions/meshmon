//! Relational persistence for campaign evaluations.
//!
//! The evaluator's output fans out across four tables:
//!
//! - `campaign_evaluations` — parent row with summary counters.
//! - `campaign_evaluation_candidates` — one row per transit destination.
//! - `campaign_evaluation_pair_details` — per-baseline-pair detail,
//!   stamped with `direct_source` provenance.
//! - `campaign_evaluation_unqualified_reasons` — reason map for
//!   destinations that never produced a qualifying pair detail.
//!
//! Writes happen inside the caller's transaction so the parent +
//! children land atomically. Reads are four sequential queries
//! (parent/candidates/pair_details/unqualified_reasons) that assemble
//! into the existing [`EvaluationDto`] wire shape.
//!
//! Every `/evaluate` call appends a fresh evaluation row; the
//! per-campaign UNIQUE constraint was dropped in the 20260424130000
//! migration so history accumulates in `campaign_evaluations`. The
//! read-path surfaces the most recent row via `ORDER BY evaluated_at
//! DESC LIMIT 1`.

use super::dto::{
    EvaluationCandidateDto, EvaluationDto, EvaluationPairDetailDto, EvaluationResultsDto,
};
use super::eval::EvaluationOutputs;
use super::model::{CampaignState, DirectSource, EvaluationMode};
use super::repo::RepoError;
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::str::FromStr;
use uuid::Uuid;

/// Insert the evaluator's output as a new `campaign_evaluations` row
/// plus fully-normalised child rows, all inside the caller's
/// transaction. Returns the newly minted evaluation id.
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
    outputs: &EvaluationOutputs,
    loss_threshold_ratio: f32,
    stddev_weight: f32,
    mode: EvaluationMode,
    max_transit_rtt_ms: Option<f64>,
    max_transit_stddev_ms: Option<f64>,
    min_improvement_ms: Option<f64>,
    min_improvement_ratio: Option<f64>,
) -> sqlx::Result<Uuid> {
    // Parent row. `evaluated_at` stamps the write wall-clock so the
    // read-path's `ORDER BY evaluated_at DESC` picks up the freshest
    // entry.
    let evaluation_id: Uuid = sqlx::query_scalar!(
        r#"INSERT INTO campaign_evaluations
              (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
               max_transit_rtt_ms, max_transit_stddev_ms,
               min_improvement_ms, min_improvement_ratio,
               baseline_pair_count, candidates_total, candidates_good,
               avg_improvement_ms, evaluated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now())
           RETURNING id"#,
        campaign_id,
        loss_threshold_ratio,
        stddev_weight,
        mode as EvaluationMode,
        max_transit_rtt_ms,
        max_transit_stddev_ms,
        min_improvement_ms,
        min_improvement_ratio,
        outputs.baseline_pair_count,
        outputs.candidates_total,
        outputs.candidates_good,
        outputs.avg_improvement_ms,
    )
    .fetch_one(&mut **tx)
    .await?;

    // Candidates — keyed on `(evaluation_id, destination_ip)` so the
    // child `pair_details` FK can chain off the same tuple.
    for cand in &outputs.results.candidates {
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
        let destination_ip = IpNetwork::from(ip);
        sqlx::query!(
            r#"INSERT INTO campaign_evaluation_candidates
                  (evaluation_id, destination_ip, display_name, city,
                   country_code, asn, network_operator, is_mesh_member,
                   pairs_improved, pairs_total_considered, avg_improvement_ms)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
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
        )
        .execute(&mut **tx)
        .await?;

        // Pair-detail rows FK to the (evaluation_id, destination_ip)
        // tuple on `campaign_evaluation_candidates`, so the insert
        // only succeeds once the candidate is in place.
        for pd in &cand.pair_details {
            sqlx::query!(
                r#"INSERT INTO campaign_evaluation_pair_details
                      (evaluation_id, candidate_destination_ip,
                       source_agent_id, destination_agent_id,
                       direct_rtt_ms, direct_stddev_ms, direct_loss_ratio,
                       direct_source,
                       transit_rtt_ms, transit_stddev_ms, transit_loss_ratio,
                       improvement_ms, qualifies,
                       mtr_measurement_id_ax, mtr_measurement_id_xb)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)"#,
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
/// Runs one query per table (parent, candidates, pair_details,
/// unqualified_reasons) and joins in Rust. The join key is the parent
/// evaluation's `id`; all child queries filter on that single value so
/// the common case is four index-scan round-trips.
pub async fn latest_evaluation_for_campaign(
    pool: &PgPool,
    campaign_id: Uuid,
) -> sqlx::Result<Option<EvaluationDto>> {
    let parent = sqlx::query!(
        r#"SELECT id, campaign_id, evaluated_at, loss_threshold_ratio, stddev_weight,
                  evaluation_mode AS "evaluation_mode: EvaluationMode",
                  max_transit_rtt_ms, max_transit_stddev_ms,
                  min_improvement_ms, min_improvement_ratio,
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
    let candidate_rows = sqlx::query!(
        r#"SELECT destination_ip, display_name, city, country_code, asn,
                  network_operator, is_mesh_member, pairs_improved,
                  pairs_total_considered, avg_improvement_ms
             FROM campaign_evaluation_candidates
            WHERE evaluation_id = $1
            ORDER BY pairs_improved DESC,
                     COALESCE(avg_improvement_ms, 0.0) DESC,
                     destination_ip ASC"#,
        parent.id,
    )
    .fetch_all(pool)
    .await?;

    let pair_detail_rows = sqlx::query!(
        r#"SELECT candidate_destination_ip, source_agent_id, destination_agent_id,
                  direct_rtt_ms, direct_stddev_ms, direct_loss_ratio,
                  direct_source AS "direct_source: DirectSource",
                  transit_rtt_ms, transit_stddev_ms, transit_loss_ratio,
                  improvement_ms, qualifies,
                  mtr_measurement_id_ax, mtr_measurement_id_xb
             FROM campaign_evaluation_pair_details
            WHERE evaluation_id = $1
            ORDER BY candidate_destination_ip ASC,
                     source_agent_id ASC,
                     destination_agent_id ASC"#,
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

    // Group pair_details by their candidate IP so the assembly loop
    // below is O(candidates + pair_details), not O(candidates *
    // pair_details).
    let mut pair_details_by_ip: std::collections::HashMap<IpAddr, Vec<EvaluationPairDetailDto>> =
        std::collections::HashMap::new();
    for pd in pair_detail_rows {
        let cand_ip = pd.candidate_destination_ip.ip();
        pair_details_by_ip
            .entry(cand_ip)
            .or_default()
            .push(EvaluationPairDetailDto {
                source_agent_id: pd.source_agent_id,
                destination_agent_id: pd.destination_agent_id,
                destination_ip: cand_ip.to_string(),
                direct_rtt_ms: pd.direct_rtt_ms,
                direct_stddev_ms: pd.direct_stddev_ms,
                direct_loss_ratio: pd.direct_loss_ratio,
                direct_source: pd.direct_source,
                transit_rtt_ms: pd.transit_rtt_ms,
                transit_stddev_ms: pd.transit_stddev_ms,
                transit_loss_ratio: pd.transit_loss_ratio,
                improvement_ms: pd.improvement_ms,
                qualifies: pd.qualifies,
                mtr_measurement_id_ax: pd.mtr_measurement_id_ax,
                mtr_measurement_id_xb: pd.mtr_measurement_id_xb,
                destination_hostname: None,
            });
    }

    // Assemble candidates. `avg_loss_ratio` and `composite_score`
    // aren't persisted (they're derivable); recompute them from the
    // pair_details here so the wire DTO stays backwards-compatible.
    let mut candidates: Vec<EvaluationCandidateDto> = Vec::with_capacity(candidate_rows.len());
    for c in candidate_rows {
        let cand_ip = c.destination_ip.ip();
        let pair_details = pair_details_by_ip.remove(&cand_ip).unwrap_or_default();
        let composite_score = if parent.baseline_pair_count > 0 {
            (c.pairs_improved as f32 / parent.baseline_pair_count as f32)
                * c.avg_improvement_ms.unwrap_or(0.0)
        } else {
            0.0
        };
        // Mean compound loss across transit rows that cleared the loss
        // gate (`qualifies==true` implies loss_ok, but the evaluator
        // also stores pair_details where the qualify predicate failed
        // for non-loss reasons — those still carry a valid
        // transit_loss_ratio). Mirror the evaluator's definition in
        // `eval::evaluate`.
        let avg_loss_ratio = if pair_details.is_empty() {
            None
        } else {
            let losses: Vec<f32> = pair_details.iter().map(|p| p.transit_loss_ratio).collect();
            Some(losses.iter().sum::<f32>() / losses.len() as f32)
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
            avg_loss_ratio,
            composite_score,
            pair_details,
            hostname: None,
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

/// Orchestrate a full `/evaluate` persistence pass: lock the parent
/// campaign row, re-check its state, [`insert_evaluation`], and promote
/// `completed → evaluated` — all inside a single transaction.
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

    let evaluation_id = insert_evaluation(
        &mut tx,
        campaign_id,
        outputs,
        loss_threshold_ratio,
        stddev_weight,
        mode,
        max_transit_rtt_ms,
        max_transit_stddev_ms,
        min_improvement_ms,
        min_improvement_ratio,
    )
    .await?;

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
