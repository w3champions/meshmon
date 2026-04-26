//! Integration tests for `campaign::repo`.
//!
//! Uses the shared migrated pool. Every test that mutates campaign rows
//! is responsible for cleaning up via `repo::delete` at the end —
//! `campaign_pairs` cascade-delete on the parent row, so one call
//! suffices.

mod common;

use meshmon_service::campaign::model::{
    CampaignState, EvaluationMode, MeasurementKind, PairResolutionState, ProbeProtocol,
};
use meshmon_service::campaign::repo::{self, CreateInput, EditInput, RepoError};
use std::net::IpAddr;
use std::str::FromStr;

fn make_input(title: &str) -> CreateInput {
    CreateInput {
        title: title.to_string(),
        notes: String::new(),
        protocol: ProbeProtocol::Icmp,
        source_agent_ids: vec!["agent-a".into(), "agent-b".into()],
        destination_ips: vec![
            IpAddr::from_str("198.51.100.1").unwrap(),
            IpAddr::from_str("198.51.100.2").unwrap(),
        ],
        force_measurement: false,
        probe_count: None,
        probe_count_detail: None,
        timeout_ms: None,
        probe_stagger_ms: None,
        loss_threshold_ratio: None,
        stddev_weight: None,
        evaluation_mode: None,
        max_transit_rtt_ms: None,
        max_transit_stddev_ms: None,
        min_improvement_ms: None,
        min_improvement_ratio: None,
        useful_latency_ms: None,
        max_hops: None,
        vm_lookback_minutes: None,
        created_by: Some("tester".into()),
    }
}

#[tokio::test]
async fn create_persists_campaign_and_cross_product_pairs() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-create")).await.unwrap();

    assert_eq!(row.state, CampaignState::Draft);
    assert_eq!(row.protocol, ProbeProtocol::Icmp);
    assert_eq!(row.probe_count, 10, "default probe_count seed");
    assert_eq!(row.evaluation_mode, EvaluationMode::Optimization);

    let pair_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_pairs WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pair_count, 4, "2 sources × 2 destinations = 4 pairs");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn create_uses_ratio_space_default_loss_threshold() {
    // Regression for T54-04: after the `_pct → _ratio` column rename, the
    // application-level `COALESCE` must match the DB-level `DEFAULT 0.02`
    // (2 % loss). A stale 2.0 default would be 200 % — effectively
    // disabling the evaluator's loss gate on campaigns created without an
    // explicit threshold.
    let pool = common::shared_migrated_pool().await;
    let mut input = make_input("t-create-default-loss");
    input.loss_threshold_ratio = None;
    let row = repo::create(&pool, input).await.unwrap();

    // Row returned from INSERT ... RETURNING.
    assert!(
        (row.loss_threshold_ratio - 0.02).abs() < f32::EPSILON,
        "default loss_threshold_ratio must be 0.02 (2 %), got {}",
        row.loss_threshold_ratio,
    );

    // And the same when re-read from the DB.
    let persisted = repo::get(&pool, row.id)
        .await
        .unwrap()
        .expect("row present");
    assert!(
        (persisted.loss_threshold_ratio - 0.02).abs() < f32::EPSILON,
        "persisted loss_threshold_ratio must be 0.02 (2 %), got {}",
        persisted.loss_threshold_ratio,
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn get_returns_none_for_unknown_id() {
    let pool = common::shared_migrated_pool().await;
    let fresh = uuid::Uuid::new_v4();
    let out = repo::get(&pool, fresh).await.unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn get_returns_persisted_row() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-get")).await.unwrap();

    let fetched = repo::get(&pool, row.id)
        .await
        .unwrap()
        .expect("row present");
    assert_eq!(fetched.id, row.id);
    assert_eq!(fetched.title, "t-get");
    assert_eq!(fetched.state, CampaignState::Draft);

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn list_filters_by_state_and_created_by() {
    let pool = common::shared_migrated_pool().await;
    let marker = format!("list-marker-{}", uuid::Uuid::new_v4().simple());

    let mut with_marker = make_input(&format!("{marker}-a"));
    with_marker.created_by = Some(marker.clone());
    let a = repo::create(&pool, with_marker.clone()).await.unwrap();

    let mut other = make_input(&format!("{marker}-b"));
    other.created_by = Some("someone-else".into());
    let b = repo::create(&pool, other).await.unwrap();

    let mine = repo::list(&pool, None, None, Some(&marker), 100)
        .await
        .unwrap();
    let ids: Vec<_> = mine.iter().map(|c| c.id).collect();
    assert!(ids.contains(&a.id), "my created_by returns my row");
    assert!(!ids.contains(&b.id), "other user's row is filtered out");

    let drafts = repo::list(&pool, None, Some(CampaignState::Draft), Some(&marker), 100)
        .await
        .unwrap();
    assert!(drafts.iter().any(|c| c.id == a.id));

    let none_running = repo::list(
        &pool,
        None,
        Some(CampaignState::Running),
        Some(&marker),
        100,
    )
    .await
    .unwrap();
    assert!(
        !none_running.iter().any(|c| c.id == a.id),
        "draft row is not returned when filtering for running"
    );

    repo::delete(&pool, a.id).await.unwrap();
    repo::delete(&pool, b.id).await.unwrap();
}

#[tokio::test]
async fn list_substring_match_on_title() {
    let pool = common::shared_migrated_pool().await;
    let marker = format!("substr-{}", uuid::Uuid::new_v4().simple());
    let row = repo::create(&pool, make_input(&format!("prefix-{marker}-suffix")))
        .await
        .unwrap();

    let hits = repo::list(&pool, Some(&marker), None, None, 100)
        .await
        .unwrap();
    assert!(hits.iter().any(|c| c.id == row.id));

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn patch_updates_provided_fields_only() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-patch")).await.unwrap();

    let patched = repo::patch(
        &pool,
        row.id,
        Some("t-patch-renamed"),
        None,
        Some(0.05_f32),
        None,
        Some(EvaluationMode::Diversity),
        Some(220.0_f64),
        None,
        Some(7.5_f64),
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    assert_eq!(patched.title, "t-patch-renamed");
    assert_eq!(patched.notes, row.notes, "notes untouched when None");
    assert!((patched.loss_threshold_ratio - 0.05).abs() < f32::EPSILON);
    assert_eq!(patched.evaluation_mode, EvaluationMode::Diversity);
    assert_eq!(patched.max_transit_rtt_ms, Some(220.0));
    assert_eq!(
        patched.max_transit_stddev_ms, None,
        "absent guardrail leaves stored column untouched"
    );
    assert_eq!(patched.min_improvement_ms, Some(7.5));
    assert_eq!(patched.min_improvement_ratio, None);

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn patch_returns_not_found_for_unknown_id() {
    let pool = common::shared_migrated_pool().await;
    let fresh = uuid::Uuid::new_v4();
    let err = repo::patch(
        &pool,
        fresh,
        Some("x"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RepoError::NotFound(id) if id == fresh));
}

#[tokio::test]
async fn start_transitions_draft_to_running() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-start")).await.unwrap();

    let started = repo::start(&pool, row.id).await.unwrap();
    assert_eq!(started.state, CampaignState::Running);
    assert!(started.started_at.is_some());

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn start_rejects_non_draft() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-start-reject"))
        .await
        .unwrap();
    repo::start(&pool, row.id).await.unwrap();

    let err = repo::start(&pool, row.id).await.unwrap_err();
    assert!(matches!(err, RepoError::IllegalTransition { .. }));

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn stop_skips_pending_pairs_and_leaves_dispatched_alone() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-stop")).await.unwrap();
    repo::start(&pool, row.id).await.unwrap();

    // Manually promote one pair to `dispatched` so we can assert it survives.
    sqlx::query(
        "UPDATE campaign_pairs SET resolution_state='dispatched', dispatched_at=now() \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-a' AND destination_ip = '198.51.100.1'",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();

    let stopped = repo::stop(&pool, row.id).await.unwrap();
    assert_eq!(stopped.state, CampaignState::Stopped);

    let pending_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs WHERE campaign_id = $1 AND resolution_state = 'pending'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending_count, 0, "stop must skip all pending pairs");

    let dispatched_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs WHERE campaign_id = $1 AND resolution_state = 'dispatched'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dispatched_count, 1, "dispatched pair must survive stop");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn delete_cascades_to_pairs() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-delete")).await.unwrap();

    let deleted = repo::delete(&pool, row.id).await.unwrap();
    assert!(deleted);

    let leftovers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_pairs WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn active_campaigns_returns_running_ordered_by_started_at() {
    let pool = common::shared_migrated_pool().await;
    let a = repo::create(&pool, make_input("t-active-a")).await.unwrap();
    let b = repo::create(&pool, make_input("t-active-b")).await.unwrap();
    repo::start(&pool, a.id).await.unwrap();
    repo::start(&pool, b.id).await.unwrap();

    let ids = repo::active_campaigns(&pool).await.unwrap();
    let a_pos = ids.iter().position(|id| *id == a.id);
    let b_pos = ids.iter().position(|id| *id == b.id);
    assert!(
        a_pos.is_some() && b_pos.is_some(),
        "both active ids returned"
    );

    repo::delete(&pool, a.id).await.unwrap();
    repo::delete(&pool, b.id).await.unwrap();
}

#[tokio::test]
async fn list_pairs_filters_by_state_and_limit() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-list-pairs"))
        .await
        .unwrap();

    let all = repo::list_pairs(&pool, row.id, &[], 100).await.unwrap();
    assert_eq!(all.len(), 4, "all four seeded pairs returned");

    let pending = repo::list_pairs(&pool, row.id, &[PairResolutionState::Pending], 100)
        .await
        .unwrap();
    assert_eq!(pending.len(), 4, "every seeded pair is pending");

    let none_done = repo::list_pairs(&pool, row.id, &[PairResolutionState::Succeeded], 100)
        .await
        .unwrap();
    assert!(none_done.is_empty());

    let capped = repo::list_pairs(&pool, row.id, &[], 2).await.unwrap();
    assert_eq!(capped.len(), 2, "limit honoured");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn list_pairs_excludes_detail_rows() {
    // `GET /api/campaigns/:id/pairs` is baseline-only. Without this
    // filter, `/detail`-triggered rows would surface as duplicate-
    // looking entries on the same `(source, destination)` tuple
    // because `PairDto` does not expose `kind`.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-list-pairs-detail"))
        .await
        .unwrap();

    // Seed a detail_mtr row sharing a tuple with one of the baseline
    // pairs `make_input` inserted.
    sqlx::query(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, \
              resolution_state, kind) \
         VALUES ($1::uuid, 'agent-a', '198.51.100.1'::inet, \
                 'pending', 'detail_mtr')",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .expect("seed detail_mtr row");

    let pairs = repo::list_pairs(&pool, row.id, &[], 100).await.unwrap();
    assert_eq!(
        pairs.len(),
        4,
        "only baseline rows returned; detail_mtr excluded"
    );
    assert!(
        pairs.iter().all(|p| p.kind == MeasurementKind::Campaign),
        "every row must carry kind=Campaign"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn preview_dispatch_count_returns_total_reusable_fresh() {
    let pool = common::shared_migrated_pool().await;

    // Seed a "reusable" measurement against a unique agent so we don't
    // collide with other concurrent tests in the shared DB.
    let agent = format!("preview-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, probe_count, loss_ratio) \
         VALUES ($1, '198.51.100.1', 'icmp', 10, 0.0)",
    )
    .bind(&agent)
    .execute(&pool)
    .await
    .unwrap();

    let counts = repo::preview_dispatch_count(
        &pool,
        ProbeProtocol::Icmp,
        &[agent.clone(), "preview-other".into()],
        &[
            IpAddr::from_str("198.51.100.1").unwrap(),
            IpAddr::from_str("198.51.100.2").unwrap(),
        ],
    )
    .await
    .unwrap();

    assert_eq!(counts.total, 4);
    assert!(counts.reusable >= 1);
    assert_eq!(counts.fresh, counts.total - counts.reusable);

    sqlx::query("DELETE FROM measurements WHERE source_agent_id = $1")
        .bind(&agent)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn take_pending_batch_flips_to_dispatched_atomically() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-batch")).await.unwrap();
    repo::start(&pool, row.id).await.unwrap();

    let batch = repo::take_pending_batch(&pool, row.id, "agent-a", 10)
        .await
        .unwrap();
    assert!(
        !batch.is_empty(),
        "expected at least one pending pair for agent-a"
    );
    for p in &batch {
        assert_eq!(p.resolution_state, PairResolutionState::Dispatched);
        assert_eq!(p.source_agent_id, "agent-a");
        assert!(p.dispatched_at.is_some());
        assert_eq!(p.attempt_count, 1);
    }

    let batch2 = repo::take_pending_batch(&pool, row.id, "agent-a", 10)
        .await
        .unwrap();
    assert!(
        batch2.is_empty(),
        "no pending left for agent-a after first batch"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn resolve_and_apply_reuse_settles_matched_pairs() {
    let pool = common::shared_migrated_pool().await;

    // Build a campaign with a unique agent id so the reuse lookup
    // doesn't collide with neighbouring tests.
    let agent = format!("reuse-{}", uuid::Uuid::new_v4().simple());
    let dst = IpAddr::from_str("198.51.100.77").unwrap();

    let mut input = make_input("t-reuse");
    input.source_agent_ids = vec![agent.clone()];
    input.destination_ips = vec![dst];
    let row = repo::create(&pool, input).await.unwrap();

    // Seed a reusable measurement younger than 24 h. `resolve_reuse`
    // now filters `m.latency_avg_ms IS NOT NULL` (see repo.rs comment)
    // so RTT-less rows can never bind to a baseline pair; seed a
    // populated RTT here to exercise the happy-path match.
    let measurement_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             latency_avg_ms, loss_ratio) \
         VALUES ($1, $2, 'icmp', 10, 100.0, 0.0) RETURNING id",
    )
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst))
    .fetch_one(&pool)
    .await
    .unwrap();

    // Simulate the scheduler path: claim first (flipping to dispatched
    // and bumping attempt_count), then apply_reuse. The invariant under
    // test is that `apply_reuse` rolls back the dispatch metadata for
    // reused pairs — they never actually reached an agent.
    repo::start(&pool, row.id).await.unwrap();
    let batch = repo::take_pending_batch(&pool, row.id, &agent, 100)
        .await
        .unwrap();
    assert!(!batch.is_empty(), "batch carries at least one pair");
    assert!(
        batch
            .iter()
            .all(|p| p.resolution_state == PairResolutionState::Dispatched
                && p.dispatched_at.is_some()
                && p.attempt_count == 1),
        "take_pending_batch flips to dispatched and bumps attempt_count"
    );

    let decisions = repo::resolve_reuse(&pool, &batch, ProbeProtocol::Icmp)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1, "exactly one pair has reuse data");
    assert_eq!(decisions[0].1, measurement_id);

    repo::apply_reuse(&pool, &decisions).await.unwrap();

    let after = repo::list_pairs(&pool, row.id, &[], 100).await.unwrap();
    let reused = after
        .iter()
        .find(|p| p.id == decisions[0].0)
        .expect("pair present");
    assert_eq!(reused.resolution_state, PairResolutionState::Reused);
    assert_eq!(reused.measurement_id, Some(measurement_id));
    assert!(reused.settled_at.is_some());
    assert!(
        reused.dispatched_at.is_none(),
        "apply_reuse clears dispatched_at since the pair never reached an agent"
    );
    assert_eq!(
        reused.attempt_count, 0,
        "apply_reuse rolls back the attempt_count bump from take_pending_batch"
    );

    repo::delete(&pool, row.id).await.unwrap();
    sqlx::query("DELETE FROM measurements WHERE id = $1")
        .bind(measurement_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn expire_stale_attempts_skips_pending_above_threshold() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-expire")).await.unwrap();

    // Bump one pair over the threshold without flipping its state.
    sqlx::query(
        "UPDATE campaign_pairs SET attempt_count = 5 \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-a' AND destination_ip = '198.51.100.1'",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();

    let n = repo::expire_stale_attempts(&pool, 3).await.unwrap();
    assert!(n >= 1, "at least one row swept");

    let pair_state: String = sqlx::query_scalar(
        "SELECT resolution_state::text FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-a' AND destination_ip = '198.51.100.1'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pair_state, "skipped");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn maybe_complete_flips_running_to_completed_when_no_work_left() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-complete")).await.unwrap();
    repo::start(&pool, row.id).await.unwrap();

    // Not yet — pairs are still pending.
    let flipped_now = repo::maybe_complete(&pool, row.id).await.unwrap();
    assert!(!flipped_now, "pending pairs block completion");

    // Force every pair to a terminal state.
    sqlx::query(
        "UPDATE campaign_pairs SET resolution_state='succeeded', settled_at=now() \
         WHERE campaign_id = $1",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();

    let flipped = repo::maybe_complete(&pool, row.id).await.unwrap();
    assert!(flipped, "all pairs terminal => campaign completed");

    let again = repo::maybe_complete(&pool, row.id).await.unwrap();
    assert!(!again, "idempotent: second call is a no-op");

    let state_after = repo::get(&pool, row.id).await.unwrap().unwrap().state;
    assert_eq!(state_after, CampaignState::Completed);

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_adds_removes_and_reruns_on_stopped() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-edit")).await.unwrap();
    repo::start(&pool, row.id).await.unwrap();
    repo::stop(&pool, row.id).await.unwrap();

    let remove = (
        "agent-a".to_string(),
        IpAddr::from_str("198.51.100.1").unwrap(),
    );
    let add = (
        "agent-c".to_string(),
        IpAddr::from_str("198.51.100.5").unwrap(),
    );

    let edited = repo::apply_edit(
        &pool,
        row.id,
        EditInput {
            add_pairs: vec![add.clone()],
            remove_pairs: vec![remove.clone()],
            force_measurement: Some(false),
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.state, CampaignState::Running);

    let pairs = repo::list_pairs(&pool, row.id, &[], 100).await.unwrap();
    let has_added = pairs
        .iter()
        .any(|p| p.source_agent_id == add.0 && p.destination_ip.ip() == add.1);
    let has_removed = pairs
        .iter()
        .any(|p| p.source_agent_id == remove.0 && p.destination_ip.ip() == remove.1);
    assert!(has_added, "added pair present");
    assert!(!has_removed, "removed pair absent");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_rejects_draft_campaign() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-edit-draft"))
        .await
        .unwrap();

    let err = repo::apply_edit(&pool, row.id, EditInput::default())
        .await
        .unwrap_err();
    assert!(matches!(err, RepoError::IllegalTransition { .. }));

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_tolerates_duplicate_add_pairs() {
    // Postgres's `INSERT ... ON CONFLICT DO UPDATE` refuses to affect
    // the same row twice in one statement (error 21000). If a client
    // sends a duplicated `(agent, ip)` in `add_pairs`, apply_edit must
    // collapse them client-side rather than surfacing a 500.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-edit-dup")).await.unwrap();
    repo::start(&pool, row.id).await.unwrap();
    repo::stop(&pool, row.id).await.unwrap();

    let new_ip = IpAddr::from_str("198.51.100.55").unwrap();
    let edit = EditInput {
        add_pairs: vec![
            ("agent-new".into(), new_ip),
            ("agent-new".into(), new_ip), // duplicate
        ],
        ..EditInput::default()
    };
    let restarted = repo::apply_edit(&pool, row.id, edit).await.unwrap();
    assert_eq!(restarted.state, CampaignState::Running);

    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-new' AND destination_ip = '198.51.100.55'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1, "duplicate collapsed to a single row");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_force_measurement_resets_resolved_pairs() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-edit-force"))
        .await
        .unwrap();
    repo::start(&pool, row.id).await.unwrap();

    // Mark every pair succeeded so maybe_complete can flip the parent.
    sqlx::query(
        "UPDATE campaign_pairs SET resolution_state='succeeded', settled_at=now() \
         WHERE campaign_id = $1",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();
    repo::maybe_complete(&pool, row.id).await.unwrap();

    let edited = repo::apply_edit(
        &pool,
        row.id,
        EditInput {
            force_measurement: Some(true),
            ..EditInput::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(edited.state, CampaignState::Running);
    assert!(edited.force_measurement, "sticky flag flipped");

    let pairs = repo::list_pairs(&pool, row.id, &[], 100).await.unwrap();
    assert!(
        pairs
            .iter()
            .all(|p| p.resolution_state == PairResolutionState::Pending),
        "every previously-resolved pair reset to pending"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn force_pair_resets_pair_and_transitions_to_running() {
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-force")).await.unwrap();
    repo::start(&pool, row.id).await.unwrap();
    sqlx::query(
        "UPDATE campaign_pairs SET resolution_state='succeeded', settled_at=now() \
         WHERE campaign_id = $1",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();
    repo::maybe_complete(&pool, row.id).await.unwrap();

    let forced = repo::force_pair(
        &pool,
        row.id,
        "agent-a",
        IpAddr::from_str("198.51.100.1").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(forced.state, CampaignState::Running);

    let pair_state: String = sqlx::query_scalar(
        "SELECT resolution_state::text FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-a' AND destination_ip = '198.51.100.1'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pair_state, "pending");

    let untouched_state: String = sqlx::query_scalar(
        "SELECT resolution_state::text FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-b' AND destination_ip = '198.51.100.2'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(untouched_state, "succeeded", "other pairs are untouched");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn force_pair_preserves_started_at_on_running_campaign() {
    // force_pair on a Running campaign must NOT bump started_at — the
    // scheduler's fair-RR rotation orders active campaigns by started_at,
    // so a stamp reset would shove the campaign to the back of the queue.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-force-running"))
        .await
        .unwrap();
    let started = repo::start(&pool, row.id).await.unwrap();
    let original_started_at = started.started_at.expect("started_at stamped by start()");

    // Ensure enough wall-clock drift that `now()` differs from
    // original_started_at at microsecond resolution.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let forced = repo::force_pair(
        &pool,
        row.id,
        "agent-a",
        IpAddr::from_str("198.51.100.1").unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(forced.state, CampaignState::Running);
    assert_eq!(
        forced.started_at,
        Some(original_started_at),
        "started_at preserved for Running campaign"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn force_pair_targets_baseline_only_when_detail_rows_coexist() {
    // After the T5 widening, the 4-col UNIQUE key lets campaign +
    // detail_ping + detail_mtr rows coexist on the same (agent, ip)
    // tuple. `force_pair` must scope to `kind='campaign'` — otherwise
    // sqlx's `fetch_optional` on `RETURNING id` sees >1 rows and errors
    // out, and the detail rows get silently reset.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-force-detail-coexist"))
        .await
        .unwrap();
    let agent = "agent-a".to_string();
    let dst = IpAddr::from_str("198.51.100.1").unwrap();
    repo::start(&pool, row.id).await.unwrap();
    sqlx::query(
        "UPDATE campaign_pairs SET resolution_state='succeeded', settled_at=now() \
         WHERE campaign_id = $1",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();
    repo::maybe_complete(&pool, row.id).await.unwrap();

    // Seed a detail_mtr row on the same tuple, in `succeeded` state —
    // force_pair must leave this alone.
    sqlx::query(
        "INSERT INTO campaign_pairs \
             (campaign_id, source_agent_id, destination_ip, \
              resolution_state, settled_at, kind) \
         VALUES ($1::uuid, $2, $3, 'succeeded', now(), 'detail_mtr')",
    )
    .bind(row.id)
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst))
    .execute(&pool)
    .await
    .expect("seed detail_mtr row");

    let forced = repo::force_pair(&pool, row.id, &agent, dst).await.unwrap();
    assert_eq!(forced.state, CampaignState::Running);

    let baseline_state: String = sqlx::query_scalar(
        "SELECT resolution_state::text FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = $2 \
           AND destination_ip = $3 AND kind = 'campaign'",
    )
    .bind(row.id)
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(baseline_state, "pending", "baseline row was reset");

    let detail_state: String = sqlx::query_scalar(
        "SELECT resolution_state::text FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = $2 \
           AND destination_ip = $3 AND kind = 'detail_mtr'",
    )
    .bind(row.id)
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        detail_state, "succeeded",
        "detail_mtr row must not be reset by force_pair"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn skip_pending_for_inactive_sources_targets_offline_agents_only() {
    // A campaign targeting (agent-a, agent-b). Feed the sweep only
    // agent-a as active — agent-b's pairs must flip to skipped with
    // `last_error='agent_offline'`, agent-a's must stay pending.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-offline-sweep"))
        .await
        .unwrap();
    repo::start(&pool, row.id).await.unwrap();

    let affected =
        repo::skip_pending_for_inactive_sources(&pool, &["agent-a".to_string()], &[row.id])
            .await
            .unwrap();
    assert!(affected >= 1, "at least one agent-b pair skipped");

    let a_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-a' AND resolution_state = 'pending'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(a_pending >= 1, "agent-a pairs stay pending");

    let b_skipped: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = 'agent-b' \
           AND resolution_state = 'skipped' AND last_error = 'agent_offline'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        b_skipped >= 1,
        "agent-b pairs flip to skipped/agent_offline"
    );

    // Empty campaign_ids is a no-op.
    let none = repo::skip_pending_for_inactive_sources(&pool, &["agent-a".into()], &[])
        .await
        .unwrap();
    assert_eq!(none, 0);

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_force_measurement_resets_dispatched_pairs() {
    // `stop()` preserves `dispatched` pairs — they may still settle
    // from an in-flight agent call. When a stopped campaign is edited
    // with force_measurement=true, those dispatched rows must also
    // reset or they stay stuck once the campaign re-enters running.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-edit-dispatched"))
        .await
        .unwrap();
    repo::start(&pool, row.id).await.unwrap();

    // Simulate an in-flight dispatch: take_pending_batch flips one pair
    // to `dispatched` with dispatched_at stamped and attempt_count=1.
    let batch = repo::take_pending_batch(&pool, row.id, "agent-a", 1)
        .await
        .unwrap();
    assert_eq!(batch.len(), 1, "one pair claimed");
    let dispatched_id = batch[0].id;

    repo::stop(&pool, row.id).await.unwrap();

    // After stop: one dispatched row survives, the rest are skipped.
    let dispatched_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
         WHERE campaign_id = $1 AND resolution_state = 'dispatched'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        dispatched_before, 1,
        "stop() preserves in-flight dispatched"
    );

    repo::apply_edit(
        &pool,
        row.id,
        EditInput {
            force_measurement: Some(true),
            ..EditInput::default()
        },
    )
    .await
    .unwrap();

    // After force_measurement: every pair is pending, including the
    // dispatched one — and its dispatched_at/attempt_count are cleared.
    let reset: PairResolutionState = sqlx::query_scalar::<_, PairResolutionState>(
        "SELECT resolution_state FROM campaign_pairs WHERE id = $1",
    )
    .bind(dispatched_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(reset, PairResolutionState::Pending);
    let attempt_count: i16 =
        sqlx::query_scalar("SELECT attempt_count FROM campaign_pairs WHERE id = $1")
            .bind(dispatched_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attempt_count, 0, "force_measurement clears attempt_count");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_force_measurement_resets_skipped_pairs() {
    // Stop converts pending pairs to `skipped`. A subsequent
    // force_measurement edit must reset them along with the terminal
    // `reused/succeeded/unreachable` triad — otherwise a re-run leaves
    // previously-skipped pairs permanently un-dispatched.
    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-edit-skipped"))
        .await
        .unwrap();
    repo::start(&pool, row.id).await.unwrap();
    repo::stop(&pool, row.id).await.unwrap();

    let skipped_before: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
         WHERE campaign_id = $1 AND resolution_state = 'skipped'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(skipped_before > 0, "stop() should produce skipped pairs");

    repo::apply_edit(
        &pool,
        row.id,
        EditInput {
            force_measurement: Some(true),
            ..EditInput::default()
        },
    )
    .await
    .unwrap();

    let skipped_after: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
         WHERE campaign_id = $1 AND resolution_state = 'skipped'",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(skipped_after, 0, "force_measurement resets skipped pairs");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn preview_dispatch_count_for_campaign_uses_actual_pair_set() {
    // Sparse campaign: pairs (A,1) and (B,2) — NOT the full cross
    // product (A,2) / (B,1). The Cartesian-projection path would
    // compute total=4; the correct answer is total=2.
    let pool = common::shared_migrated_pool().await;
    let agent_a = format!("prev-a-{}", uuid::Uuid::new_v4().simple());
    let agent_b = format!("prev-b-{}", uuid::Uuid::new_v4().simple());
    let ip1 = IpAddr::from_str("198.51.100.10").unwrap();
    let ip2 = IpAddr::from_str("198.51.100.11").unwrap();

    // Build a campaign with the full cross product first.
    let mut input = make_input("t-preview-sparse");
    input.source_agent_ids = vec![agent_a.clone(), agent_b.clone()];
    input.destination_ips = vec![ip1, ip2];
    let row = repo::create(&pool, input).await.unwrap();

    // Remove the off-diagonal pairs to leave only (A,1) and (B,2).
    let ip2_net = sqlx::types::ipnetwork::IpNetwork::from(ip2);
    let ip1_net = sqlx::types::ipnetwork::IpNetwork::from(ip1);
    sqlx::query(
        "DELETE FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = $2 AND destination_ip = $3",
    )
    .bind(row.id)
    .bind(&agent_a)
    .bind(ip2_net)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM campaign_pairs \
         WHERE campaign_id = $1 AND source_agent_id = $2 AND destination_ip = $3",
    )
    .bind(row.id)
    .bind(&agent_b)
    .bind(ip1_net)
    .execute(&pool)
    .await
    .unwrap();

    // Seed a reusable measurement for (agent_a, ip1) so reusable=1.
    sqlx::query(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, probe_count, loss_ratio) \
         VALUES ($1, $2, 'icmp', 10, 0.0)",
    )
    .bind(&agent_a)
    .bind(ip1_net)
    .execute(&pool)
    .await
    .unwrap();

    let counts =
        repo::preview_dispatch_count_for_campaign(&pool, row.id, ProbeProtocol::Icmp, false)
            .await
            .unwrap();
    assert_eq!(counts.total, 2, "total reflects actual pair set, not AxB");
    assert_eq!(counts.reusable, 1, "one reusable measurement");
    assert_eq!(counts.fresh, 1, "total - reusable");

    // force_measurement=true must skip reuse — scheduler disables reuse
    // for those campaigns, preview must agree.
    let forced_counts =
        repo::preview_dispatch_count_for_campaign(&pool, row.id, ProbeProtocol::Icmp, true)
            .await
            .unwrap();
    assert_eq!(forced_counts.total, 2);
    assert_eq!(
        forced_counts.reusable, 0,
        "force_measurement disables reuse in preview"
    );
    assert_eq!(forced_counts.fresh, 2);

    repo::delete(&pool, row.id).await.unwrap();
    sqlx::query("DELETE FROM measurements WHERE source_agent_id IN ($1, $2)")
        .bind(&agent_a)
        .bind(&agent_b)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn resolve_reuse_skips_detail_kind_pairs() {
    // A detail measurement (kind='detail_ping'/'detail_mtr') must always run
    // fresh — the 24h reuse cache is for campaign-kind rows only. This test
    // seeds one campaign with two destinations: one pair stays kind='campaign'
    // (MUST reuse), the other is flipped to kind='detail_ping' (MUST NOT
    // reuse even when a matching measurement exists in the window).
    let pool = common::shared_migrated_pool().await;

    let agent = format!("detail-skip-{}", uuid::Uuid::new_v4().simple());
    let dst_campaign = IpAddr::from_str("198.51.100.201").unwrap();
    let dst_detail = IpAddr::from_str("198.51.100.202").unwrap();

    let mut input = make_input("t-detail-skip");
    input.source_agent_ids = vec![agent.clone()];
    input.destination_ips = vec![dst_campaign, dst_detail];
    let row = repo::create(&pool, input).await.unwrap();

    // Seed a reusable measurement younger than 24h for both
    // destinations, with `latency_avg_ms` populated — `resolve_reuse`
    // now rejects RTT-less rows (see repo.rs comment), so the match
    // would otherwise fail before the kind filter has anything to do.
    let m_campaign_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             latency_avg_ms, loss_ratio) \
         VALUES ($1, $2, 'icmp', 10, 100.0, 0.0) RETURNING id",
    )
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_campaign))
    .fetch_one(&pool)
    .await
    .unwrap();
    let m_detail_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             latency_avg_ms, loss_ratio) \
         VALUES ($1, $2, 'icmp', 10, 100.0, 0.0) RETURNING id",
    )
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_detail))
    .fetch_one(&pool)
    .await
    .unwrap();

    // Flip the detail-destination pair to kind='detail_ping'. The campaign
    // pair keeps the default kind='campaign'.
    sqlx::query(
        "UPDATE campaign_pairs SET kind = 'detail_ping' \
         WHERE campaign_id = $1 AND destination_ip = $2",
    )
    .bind(row.id)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_detail))
    .execute(&pool)
    .await
    .unwrap();

    // `list_pairs` narrows to `kind='campaign'` (baseline-only), so
    // fetch the actual two-kind pair set via a raw SELECT — this test
    // needs to feed BOTH kinds into `resolve_reuse` to prove the detail
    // row is filtered by the `cp.kind='campaign'` CTE clause.
    use meshmon_service::campaign::model::PairRow;
    use sqlx::types::ipnetwork::IpNetwork;
    let rows: Vec<(i64, String, IpNetwork, MeasurementKind)> =
        sqlx::query_as::<_, (i64, String, IpNetwork, MeasurementKind)>(
            "SELECT id, source_agent_id, destination_ip, kind \
           FROM campaign_pairs WHERE campaign_id = $1 ORDER BY id",
        )
        .bind(row.id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "two pairs seeded");
    let pairs: Vec<PairRow> = rows
        .into_iter()
        .map(|(id, src, dst, kind)| PairRow {
            id,
            campaign_id: row.id,
            source_agent_id: src,
            destination_ip: dst,
            resolution_state: PairResolutionState::Pending,
            measurement_id: None,
            dispatched_at: None,
            settled_at: None,
            attempt_count: 0,
            last_error: None,
            kind,
        })
        .collect();

    let decisions = repo::resolve_reuse(&pool, &pairs, ProbeProtocol::Icmp)
        .await
        .unwrap();

    let campaign_pair_id = pairs
        .iter()
        .find(|p| p.destination_ip.ip() == dst_campaign)
        .expect("campaign pair present")
        .id;
    let detail_pair_id = pairs
        .iter()
        .find(|p| p.destination_ip.ip() == dst_detail)
        .expect("detail pair present")
        .id;

    assert_eq!(decisions.len(), 1, "only the campaign-kind pair reuses");
    assert_eq!(
        decisions[0].0, campaign_pair_id,
        "campaign-kind pair id present"
    );
    assert_eq!(
        decisions[0].1, m_campaign_id,
        "campaign pair points at its own seeded measurement"
    );
    assert!(
        decisions.iter().all(|(pid, _)| *pid != detail_pair_id),
        "detail_ping pair must never appear in reuse output"
    );

    repo::delete(&pool, row.id).await.unwrap();
    sqlx::query("DELETE FROM measurements WHERE id = ANY($1)")
        .bind(&[m_campaign_id, m_detail_id][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn resolve_reuse_skips_rtt_null_measurements() {
    // Reuse must reject measurement rows with `latency_avg_ms IS NULL`
    // — the writer's MTR branch always produces such rows, and
    // binding one to a baseline pair would leave the evaluator unable
    // to score against it (falling back to `no_baseline_pairs` on
    // low-probe campaigns).
    use meshmon_service::campaign::model::PairRow;
    use sqlx::types::ipnetwork::IpNetwork;
    let pool = common::shared_migrated_pool().await;

    let agent = format!("reuse-rtt-null-{}", uuid::Uuid::new_v4().simple());
    let dst = IpAddr::from_str("198.51.100.240").unwrap();

    let mut input = make_input("t-reuse-rtt-null");
    input.source_agent_ids = vec![agent.clone()];
    input.destination_ips = vec![dst];
    let row = repo::create(&pool, input).await.unwrap();

    // Seed a measurement with `latency_avg_ms = NULL` (the shape the
    // MTR writer produces today).
    let m_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             loss_ratio, kind) \
         VALUES ($1, $2, 'icmp', 1, 0.0, 'detail_mtr') RETURNING id",
    )
    .bind(&agent)
    .bind(IpNetwork::from(dst))
    .fetch_one(&pool)
    .await
    .unwrap();

    let (pair_id, source_agent_id, dest_ip): (i64, String, IpNetwork) = sqlx::query_as(
        "SELECT id, source_agent_id, destination_ip FROM campaign_pairs \
          WHERE campaign_id = $1",
    )
    .bind(row.id)
    .fetch_one(&pool)
    .await
    .unwrap();

    let pairs = vec![PairRow {
        id: pair_id,
        campaign_id: row.id,
        source_agent_id,
        destination_ip: dest_ip,
        resolution_state: PairResolutionState::Pending,
        measurement_id: None,
        dispatched_at: None,
        settled_at: None,
        attempt_count: 0,
        last_error: None,
        kind: MeasurementKind::Campaign,
    }];
    let decisions = repo::resolve_reuse(&pool, &pairs, ProbeProtocol::Icmp)
        .await
        .unwrap();
    assert!(
        decisions.is_empty(),
        "RTT-null measurement must not bind: {decisions:?}"
    );

    repo::delete(&pool, row.id).await.unwrap();
    sqlx::query("DELETE FROM measurements WHERE id = $1")
        .bind(m_id)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn measurements_for_campaign_filters_detail_kind() {
    // measurements_for_campaign must only surface rows attached to
    // `kind='campaign'` pairs. A detail-kind pair with its own
    // measurement attached must not leak into the evaluator's inputs.
    use meshmon_service::campaign::eval::AttributedMeasurement;
    let pool = common::shared_migrated_pool().await;

    let agent = format!("mfc-{}", uuid::Uuid::new_v4().simple());
    let dst_campaign = IpAddr::from_str("198.51.100.211").unwrap();
    let dst_detail = IpAddr::from_str("198.51.100.212").unwrap();

    let mut input = make_input("t-mfc");
    input.source_agent_ids = vec![agent.clone()];
    input.destination_ips = vec![dst_campaign, dst_detail];
    let row = repo::create(&pool, input).await.unwrap();

    // Flip one pair to kind='detail_ping' so it must not leak.
    sqlx::query(
        "UPDATE campaign_pairs SET kind = 'detail_ping' \
         WHERE campaign_id = $1 AND destination_ip = $2",
    )
    .bind(row.id)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_detail))
    .execute(&pool)
    .await
    .unwrap();

    // Seed two measurements — one per pair — and attach them via
    // campaign_pairs.measurement_id so the JOIN in
    // measurements_for_campaign picks them up.
    let m_campaign_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, \
                                   probe_count, latency_avg_ms, latency_stddev_ms, \
                                   loss_ratio, kind) \
         VALUES ($1, $2, 'icmp', 10, 25.0, 1.5, 0.0, 'campaign') RETURNING id",
    )
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_campaign))
    .fetch_one(&pool)
    .await
    .unwrap();
    let m_detail_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, \
                                   probe_count, latency_avg_ms, latency_stddev_ms, \
                                   loss_ratio, kind) \
         VALUES ($1, $2, 'icmp', 250, 18.0, 0.7, 0.0, 'detail_ping') RETURNING id",
    )
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_detail))
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE campaign_pairs SET measurement_id = $1 \
         WHERE campaign_id = $2 AND destination_ip = $3",
    )
    .bind(m_campaign_id)
    .bind(row.id)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_campaign))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE campaign_pairs SET measurement_id = $1 \
         WHERE campaign_id = $2 AND destination_ip = $3",
    )
    .bind(m_detail_id)
    .bind(row.id)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst_detail))
    .execute(&pool)
    .await
    .unwrap();

    let inputs = repo::measurements_for_campaign(&pool, row.id)
        .await
        .unwrap();

    let destinations: Vec<IpAddr> = inputs
        .measurements
        .iter()
        .map(|m: &AttributedMeasurement| m.destination_ip)
        .collect();
    assert!(
        destinations.contains(&dst_campaign),
        "campaign-kind pair surfaces in inputs: {destinations:?}",
    );
    assert!(
        !destinations.contains(&dst_detail),
        "detail-kind pair must not leak into inputs: {destinations:?}",
    );
    assert_eq!(
        inputs.measurements.len(),
        1,
        "only the campaign-kind measurement attached",
    );
    assert_eq!(
        inputs.loss_threshold_ratio, row.loss_threshold_ratio,
        "campaign scoring knobs thread through"
    );
    assert_eq!(inputs.stddev_weight, row.stddev_weight);
    assert_eq!(inputs.mode, row.evaluation_mode);

    repo::delete(&pool, row.id).await.unwrap();
    sqlx::query("DELETE FROM measurements WHERE id = ANY($1)")
        .bind(&[m_campaign_id, m_detail_id][..])
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn persist_evaluation_appends_history_and_read_surfaces_latest() {
    // Write a synthetic evaluation, read it back, then persist a
    // second one with a different mode. The new schema dropped the
    // per-campaign UNIQUE constraint, so two rows must coexist and
    // the read-path must surface the freshest.
    use meshmon_service::campaign::dto::EvaluationResultsDto;
    use meshmon_service::campaign::eval::{EvaluationOutputs, TripleEvaluationOutputs};
    use meshmon_service::campaign::evaluation_repo;
    let pool = common::shared_migrated_pool().await;

    let row = repo::create(&pool, make_input("t-eval-rt")).await.unwrap();
    // `persist_evaluation` locks the row and rechecks state inside
    // its transaction — drafts are rejected. Force completed so the
    // persistence path opens.
    common::mark_completed(&pool, &row.id.to_string()).await;

    let first = EvaluationOutputs::Triple(TripleEvaluationOutputs {
        baseline_pair_count: 3,
        candidates_total: 2,
        candidates_good: 1,
        avg_improvement_ms: Some(12.5),
        results: EvaluationResultsDto {
            candidates: Vec::new(),
            unqualified_reasons: Default::default(),
        },
        pair_details_by_candidate: Vec::new(),
    });
    let first_id = evaluation_repo::persist_evaluation(
        &pool,
        row.id,
        &first,
        0.025,
        1.25,
        EvaluationMode::Diversity,
        None,
        None,
        None,
        None,
        None,
        1,
        15,
    )
    .await
    .unwrap();

    let read_first = evaluation_repo::latest_evaluation_for_campaign(&pool, row.id)
        .await
        .unwrap()
        .expect("row present after first persist");
    assert_eq!(read_first.baseline_pair_count, 3);
    assert_eq!(read_first.candidates_good, 1);
    assert_eq!(read_first.evaluation_mode, EvaluationMode::Diversity);
    assert_eq!(read_first.avg_improvement_ms, Some(12.5));
    assert_eq!(read_first.loss_threshold_ratio, 0.025);
    assert_eq!(read_first.stddev_weight, 1.25);

    // Capture the first row's evaluated_at so we can assert the
    // insert below appends a distinct row rather than mutating the
    // existing one.
    let first_evaluated_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT evaluated_at FROM campaign_evaluations WHERE id = $1")
            .bind(first_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    // Force a clock delta so evaluated_at on the second row is
    // strictly greater regardless of platform timestamp resolution.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    let second = EvaluationOutputs::Triple(TripleEvaluationOutputs {
        baseline_pair_count: 7,
        candidates_total: 5,
        candidates_good: 4,
        avg_improvement_ms: Some(20.0),
        results: EvaluationResultsDto {
            candidates: Vec::new(),
            unqualified_reasons: Default::default(),
        },
        pair_details_by_candidate: Vec::new(),
    });
    let second_id = evaluation_repo::persist_evaluation(
        &pool,
        row.id,
        &second,
        0.03,
        0.5,
        EvaluationMode::Optimization,
        None,
        None,
        None,
        None,
        None,
        1,
        15,
    )
    .await
    .unwrap();
    assert_ne!(
        second_id, first_id,
        "INSERT-history: every /evaluate produces a new id"
    );

    // The first row must be untouched after the second insert —
    // history, not UPSERT.
    let first_still: (EvaluationMode, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "SELECT evaluation_mode, evaluated_at FROM campaign_evaluations WHERE id = $1",
    )
    .bind(first_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        first_still.0,
        EvaluationMode::Diversity,
        "first row's evaluation_mode must remain untouched"
    );
    assert_eq!(
        first_still.1, first_evaluated_at,
        "first row's evaluated_at must remain untouched"
    );

    let read_second = evaluation_repo::latest_evaluation_for_campaign(&pool, row.id)
        .await
        .unwrap()
        .expect("row present after second persist");
    assert_eq!(read_second.evaluation_mode, EvaluationMode::Optimization);
    assert_eq!(read_second.baseline_pair_count, 7);
    assert_eq!(read_second.candidates_good, 4);
    assert_eq!(read_second.loss_threshold_ratio, 0.03);
    assert_eq!(read_second.stddev_weight, 0.5);
    assert!(
        read_second.evaluated_at >= read_first.evaluated_at,
        "latest-by-evaluated_at must not regress: {:?} vs {:?}",
        read_first.evaluated_at,
        read_second.evaluated_at
    );

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        count, 2,
        "INSERT-history preserves both rows; latest-pick happens on read"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn persist_evaluation_rejects_running_campaign() {
    // Race guard: `persist_evaluation` must recheck state under the row
    // lock. If a concurrent `/detail` flipped the campaign to `running`
    // between the handler's initial gate and our insert, persistence
    // must abort with IllegalTransition rather than silently writing a
    // fresh evaluation against a now-running campaign.
    use meshmon_service::campaign::dto::EvaluationResultsDto;
    use meshmon_service::campaign::eval::{EvaluationOutputs, TripleEvaluationOutputs};
    use meshmon_service::campaign::evaluation_repo;
    let pool = common::shared_migrated_pool().await;

    let row = repo::create(&pool, make_input("t-eval-gate"))
        .await
        .unwrap();
    repo::start(&pool, row.id).await.unwrap();
    // Campaign is now in `running`.

    let outputs = EvaluationOutputs::Triple(TripleEvaluationOutputs {
        baseline_pair_count: 0,
        candidates_total: 0,
        candidates_good: 0,
        avg_improvement_ms: None,
        results: EvaluationResultsDto {
            candidates: Vec::new(),
            unqualified_reasons: Default::default(),
        },
        pair_details_by_candidate: Vec::new(),
    });

    let err = evaluation_repo::persist_evaluation(
        &pool,
        row.id,
        &outputs,
        1.0,
        1.0,
        EvaluationMode::Optimization,
        None,
        None,
        None,
        None,
        None,
        1,
        15,
    )
    .await
    .expect_err("persist_evaluation must reject running state");
    match err {
        RepoError::IllegalTransition { from, .. } => {
            assert_eq!(
                from,
                Some(CampaignState::Running),
                "from-state must reflect the observed running lock"
            );
        }
        other => panic!("expected IllegalTransition, got {other:?}"),
    }

    // No evaluation row should have been written either.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0, "nothing persisted on the reject path");

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn persist_evaluation_rolls_back_on_unparseable_candidate_ip() {
    // Consistency contract: the parent row's `candidates_total`
    // counter must never disagree with the child-row count after
    // commit. The writer guarantees this by aborting the tx when any
    // evaluator-provided `destination_ip` fails to round-trip through
    // IpAddr — an unreachable case in normal operation, but cheap to
    // guard against and cheap to verify.
    use meshmon_service::campaign::dto::{
        EvaluationCandidateDto, EvaluationPairDetailDto, EvaluationResultsDto,
    };
    use meshmon_service::campaign::eval::{
        EvaluationOutputs, PairDetailsForCandidate, TripleEvaluationOutputs,
    };
    use meshmon_service::campaign::evaluation_repo;
    use meshmon_service::campaign::model::DirectSource;
    let pool = common::shared_migrated_pool().await;

    let row = repo::create(&pool, make_input("t-eval-rollback"))
        .await
        .unwrap();
    common::mark_completed(&pool, &row.id.to_string()).await;

    // Craft an EvaluationOutputs whose candidate carries a garbage
    // destination_ip string. Every other field is valid.
    let garbage_candidate = EvaluationCandidateDto {
        destination_ip: "not-an-ip".to_string(),
        display_name: None,
        city: None,
        country_code: None,
        asn: None,
        network_operator: None,
        is_mesh_member: false,
        pairs_improved: 0,
        pairs_total_considered: 1,
        avg_improvement_ms: Some(0.0),
        avg_loss_ratio: Some(0.0),
        composite_score: Some(0.0),
        hostname: None,
        website: None,
        notes: None,
        agent_id: None,
        coverage_count: None,
        destinations_total: None,
        mean_ms_under_t: None,
        coverage_weighted_ping_ms: None,
        direct_share: None,
        onehop_share: None,
        twohop_share: None,
        has_real_x_source_data: None,
    };
    // Sidecar pair-detail bundle. The bundle's `destination_ip` is
    // intentionally a real IP — the evaluator sets this from the
    // parsed candidate IP, and the writer is supposed to abort BEFORE
    // it reaches the bundle's mismatch check (the candidate's
    // unparseable string trips the IpAddr::from_str guard first).
    // Using `1.2.3.4` here keeps the test exclusively about the
    // candidate-string parse failure.
    let bundle = PairDetailsForCandidate {
        destination_ip: "1.2.3.4".parse().unwrap(),
        pair_details: vec![EvaluationPairDetailDto {
            source_agent_id: "a".into(),
            destination_agent_id: "b".into(),
            destination_ip: "not-an-ip".into(),
            direct_rtt_ms: 1.0,
            direct_stddev_ms: 0.0,
            direct_loss_ratio: 0.0,
            direct_source: DirectSource::ActiveProbe,
            transit_rtt_ms: 1.0,
            transit_stddev_ms: 0.0,
            transit_loss_ratio: 0.0,
            improvement_ms: 0.0,
            qualifies: false,
            mtr_measurement_id_ax: None,
            mtr_measurement_id_xb: None,
            destination_hostname: None,
            ax_was_substituted: None,
            xb_was_substituted: None,
            direct_was_substituted: None,
            winning_x_position: None,
        }],
        qualifying_legs: Vec::new(),
    };
    let outputs = EvaluationOutputs::Triple(TripleEvaluationOutputs {
        baseline_pair_count: 1,
        candidates_total: 1,
        candidates_good: 0,
        avg_improvement_ms: None,
        results: EvaluationResultsDto {
            candidates: vec![garbage_candidate],
            unqualified_reasons: Default::default(),
        },
        pair_details_by_candidate: vec![bundle],
    });

    let err = evaluation_repo::persist_evaluation(
        &pool,
        row.id,
        &outputs,
        0.05,
        1.0,
        EvaluationMode::Optimization,
        None,
        None,
        None,
        None,
        None,
        1,
        15,
    )
    .await
    .expect_err("unparseable candidate destination_ip must abort the tx");
    match err {
        RepoError::Sqlx(sqlx::Error::Protocol(_)) => {}
        other => panic!("expected sqlx::Error::Protocol, got {other:?}"),
    }

    // Parent row must not exist — the abort happened after the parent
    // INSERT but before commit, so the rollback must drop it.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        count, 0,
        "tx rollback must drop the parent row so counters can never skew"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn insert_detail_pairs_flips_running_and_skips_duplicates() {
    // A completed campaign re-enters `running` when detail pairs land,
    // the same pair inserted twice produces no new rows on the second
    // call, and the 4-column UNIQUE lets one (source, destination) hold
    // both detail_ping and detail_mtr rows side-by-side.
    let pool = common::shared_migrated_pool().await;

    let agent = format!("det-{}", uuid::Uuid::new_v4().simple());
    let dst = IpAddr::from_str("198.51.100.221").unwrap();
    let mut input = make_input("t-detail-insert");
    input.source_agent_ids = vec![agent.clone()];
    input.destination_ips = vec![dst];
    let row = repo::create(&pool, input).await.unwrap();

    // Move the campaign to `completed` so we can observe the state flip.
    sqlx::query(
        "UPDATE measurement_campaigns \
            SET state = 'completed', \
                started_at = now(), \
                completed_at = now(), \
                evaluated_at = now() \
          WHERE id = $1",
    )
    .bind(row.id)
    .execute(&pool)
    .await
    .unwrap();

    let (inserted, post_state) = repo::insert_detail_pairs(&pool, row.id, &[(agent.clone(), dst)])
        .await
        .unwrap();
    assert_eq!(
        inserted, 2,
        "one requested pair spawns detail_ping + detail_mtr"
    );
    assert_eq!(
        post_state,
        CampaignState::Running,
        "completed → running transition must be reported"
    );

    let campaign_after = repo::get(&pool, row.id)
        .await
        .unwrap()
        .expect("campaign still present");
    assert_eq!(
        campaign_after.state,
        CampaignState::Running,
        "detail insert reopens the campaign"
    );
    // Historical breadcrumbs are preserved (matching force_pair's
    // convention): only the state and started_at get updated.
    assert!(
        campaign_after.completed_at.is_some(),
        "completed_at preserved as historical breadcrumb on reopen"
    );
    assert!(
        campaign_after.evaluated_at.is_some(),
        "evaluated_at preserved as historical breadcrumb on reopen"
    );

    // Verify both kinds landed against the same (source, destination).
    let kinds: Vec<String> = sqlx::query_scalar(
        "SELECT kind::text FROM campaign_pairs \
          WHERE campaign_id = $1 AND source_agent_id = $2 AND destination_ip = $3 \
          ORDER BY kind::text",
    )
    .bind(row.id)
    .bind(&agent)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(dst))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        kinds,
        vec![
            "campaign".to_string(),
            "detail_mtr".into(),
            "detail_ping".into()
        ],
        "detail insert coexists with the original campaign pair"
    );

    // Second call with the same pair: duplicates skip silently.
    let (inserted_again, post_state_again) =
        repo::insert_detail_pairs(&pool, row.id, &[(agent.clone(), dst)])
            .await
            .unwrap();
    assert_eq!(
        inserted_again, 0,
        "duplicate detail pairs skip via ON CONFLICT DO NOTHING"
    );
    assert_eq!(
        post_state_again,
        CampaignState::Running,
        "second call observes the already-running state under the lock"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn insert_detail_pairs_no_op_does_not_flip_state() {
    // A repeat `/detail` call where every requested row already exists
    // (inserted == 0) must NOT transition a completed campaign back to
    // `running`. Re-flipping without new work causes spurious state
    // churn visible in both the API response and the SSE stream.
    let pool = common::shared_migrated_pool().await;

    let agent = format!("det-noop-{}", uuid::Uuid::new_v4().simple());
    let dst = IpAddr::from_str("198.51.100.233").unwrap();
    let mut input = make_input("t-detail-noop");
    input.source_agent_ids = vec![agent.clone()];
    input.destination_ips = vec![dst];
    let row = repo::create(&pool, input).await.unwrap();

    common::mark_completed(&pool, &row.id.to_string()).await;

    // Seed detail rows once (flips completed → running).
    let (first_inserted, first_state) =
        repo::insert_detail_pairs(&pool, row.id, &[(agent.clone(), dst)])
            .await
            .unwrap();
    assert_eq!(first_inserted, 2);
    assert_eq!(first_state, CampaignState::Running);

    // Force back to completed so the second call sees a gateable state.
    common::mark_completed(&pool, &row.id.to_string()).await;

    let (second_inserted, second_state) =
        repo::insert_detail_pairs(&pool, row.id, &[(agent.clone(), dst)])
            .await
            .unwrap();
    assert_eq!(
        second_inserted, 0,
        "all requested pairs already exist — nothing to insert"
    );
    assert_eq!(
        second_state,
        CampaignState::Completed,
        "no inserts → no state flip: observed state is the pre-call Completed"
    );

    let after = repo::get(&pool, row.id)
        .await
        .unwrap()
        .expect("campaign still present");
    assert_eq!(
        after.state,
        CampaignState::Completed,
        "campaign must remain completed when no detail rows are queued"
    );

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_preserves_detail_rows() {
    // `/edit remove_pairs` must only drop baseline (kind='campaign')
    // rows. The T5 UNIQUE key was widened to include `kind`, so a
    // DELETE without a kind filter would wipe detail_ping + detail_mtr
    // rows sharing the same (source, destination) tuple.
    let pool = common::shared_migrated_pool().await;
    common::insert_agent_with_ip(&pool, "ae-edit-a", "192.0.2.101".parse().unwrap()).await;
    common::insert_agent_with_ip(&pool, "ae-edit-b", "192.0.2.102".parse().unwrap()).await;

    let campaign = repo::create(
        &pool,
        CreateInput {
            title: "edit preserves detail".into(),
            notes: String::new(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec!["ae-edit-a".into()],
            destination_ips: vec!["192.0.2.102".parse().unwrap()],
            force_measurement: false,
            probe_count: None,
            probe_count_detail: None,
            timeout_ms: None,
            probe_stagger_ms: None,
            loss_threshold_ratio: None,
            stddev_weight: None,
            evaluation_mode: None,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: None,
            vm_lookback_minutes: None,
            created_by: None,
        },
    )
    .await
    .unwrap();

    let src = "ae-edit-a".to_string();
    let dst: IpAddr = "192.0.2.102".parse().unwrap();

    // Force the campaign to `completed` first so `insert_detail_pairs`'
    // state-transition gate accepts it. The insert flips state back to
    // `running`; `apply_edit` needs `completed|stopped|evaluated`, so
    // force-complete again after seeding.
    common::mark_completed(&pool, &campaign.id.to_string()).await;
    repo::insert_detail_pairs(&pool, campaign.id, &[(src.clone(), dst)])
        .await
        .unwrap();
    common::mark_completed(&pool, &campaign.id.to_string()).await;

    repo::apply_edit(
        &pool,
        campaign.id,
        EditInput {
            remove_pairs: vec![(src.clone(), dst)],
            add_pairs: vec![],
            force_measurement: None,
        },
    )
    .await
    .unwrap();

    let detail_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
          WHERE campaign_id = $1 \
            AND kind IN ('detail_ping','detail_mtr')",
    )
    .bind(campaign.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(detail_rows, 2, "detail rows must survive remove_pairs");

    let baseline_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM campaign_pairs \
          WHERE campaign_id = $1 AND kind = 'campaign'",
    )
    .bind(campaign.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(baseline_rows, 0, "baseline pair should be gone");

    repo::delete(&pool, campaign.id).await.unwrap();
}

#[tokio::test]
async fn apply_edit_force_measurement_preserves_detail_rows() {
    // `force_measurement=true` resets every pair back to pending so the
    // baseline re-runs. Detail rows (kind in {detail_ping, detail_mtr})
    // are independently triggered and must not re-run just because the
    // operator asked to re-run the campaign's baseline measurements.
    let pool = common::shared_migrated_pool().await;
    common::insert_agent_with_ip(&pool, "ae-force-a", "192.0.2.111".parse().unwrap()).await;
    common::insert_agent_with_ip(&pool, "ae-force-b", "192.0.2.112".parse().unwrap()).await;

    let campaign = repo::create(
        &pool,
        CreateInput {
            title: "force preserves detail".into(),
            notes: String::new(),
            protocol: ProbeProtocol::Icmp,
            source_agent_ids: vec!["ae-force-a".into()],
            destination_ips: vec!["192.0.2.112".parse().unwrap()],
            force_measurement: false,
            probe_count: None,
            probe_count_detail: None,
            timeout_ms: None,
            probe_stagger_ms: None,
            loss_threshold_ratio: None,
            stddev_weight: None,
            evaluation_mode: None,
            max_transit_rtt_ms: None,
            max_transit_stddev_ms: None,
            min_improvement_ms: None,
            min_improvement_ratio: None,
            useful_latency_ms: None,
            max_hops: None,
            vm_lookback_minutes: None,
            created_by: None,
        },
    )
    .await
    .unwrap();

    let src = "ae-force-a".to_string();
    let dst: IpAddr = "192.0.2.112".parse().unwrap();

    // Force `completed` first so `insert_detail_pairs` accepts the
    // transition, then reset to `completed` after seeding so the
    // subsequent `apply_edit` is legal.
    common::mark_completed(&pool, &campaign.id.to_string()).await;
    repo::insert_detail_pairs(&pool, campaign.id, &[(src.clone(), dst)])
        .await
        .unwrap();
    common::mark_completed(&pool, &campaign.id.to_string()).await;

    // Manually mark the detail_mtr row as succeeded so force_measurement
    // would have reset it before the fix.
    sqlx::query(
        "UPDATE campaign_pairs SET resolution_state = 'succeeded' \
          WHERE campaign_id = $1 AND kind = 'detail_mtr'",
    )
    .bind(campaign.id)
    .execute(&pool)
    .await
    .unwrap();

    repo::apply_edit(
        &pool,
        campaign.id,
        EditInput {
            remove_pairs: vec![],
            add_pairs: vec![],
            force_measurement: Some(true),
        },
    )
    .await
    .unwrap();

    let detail_state: PairResolutionState = sqlx::query_scalar::<_, PairResolutionState>(
        "SELECT resolution_state FROM campaign_pairs \
          WHERE campaign_id = $1 AND kind = 'detail_mtr'",
    )
    .bind(campaign.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        detail_state,
        PairResolutionState::Succeeded,
        "detail_mtr row must stay succeeded after force_measurement on campaign kind"
    );

    repo::delete(&pool, campaign.id).await.unwrap();
}

#[tokio::test]
async fn campaign_evaluations_cascade_on_campaign_delete() {
    // `campaign_evaluations.campaign_id` has an `ON DELETE CASCADE` FK
    // to `measurement_campaigns.id`. Deleting the parent must drop
    // every historical evaluation row (plus its children via the
    // second cascade chain) in the same transaction — orphan rows
    // would otherwise accumulate and clutter `/evaluation` read
    // attempts on a recreated campaign reusing the same UUID (tests
    // and disaster-recovery paths both rely on reusable ids).
    use meshmon_service::campaign::dto::EvaluationResultsDto;
    use meshmon_service::campaign::eval::{EvaluationOutputs, TripleEvaluationOutputs};
    use meshmon_service::campaign::evaluation_repo;

    let pool = common::shared_migrated_pool().await;
    let row = repo::create(&pool, make_input("t-eval-cascade"))
        .await
        .unwrap();
    // `persist_evaluation` locks the row and rechecks state — force
    // completed so the persistence path opens.
    common::mark_completed(&pool, &row.id.to_string()).await;

    let outputs = EvaluationOutputs::Triple(TripleEvaluationOutputs {
        baseline_pair_count: 1,
        candidates_total: 0,
        candidates_good: 0,
        avg_improvement_ms: None,
        results: EvaluationResultsDto {
            candidates: Vec::new(),
            unqualified_reasons: Default::default(),
        },
        pair_details_by_candidate: Vec::new(),
    });
    evaluation_repo::persist_evaluation(
        &pool,
        row.id,
        &outputs,
        2.0,
        1.0,
        EvaluationMode::Optimization,
        None,
        None,
        None,
        None,
        None,
        1,
        15,
    )
    .await
    .unwrap();

    let before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(before, 1, "baseline: evaluation row exists");

    repo::delete(&pool, row.id).await.unwrap();

    let after: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM campaign_evaluations WHERE campaign_id = $1")
            .bind(row.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        after, 0,
        "ON DELETE CASCADE must drop the evaluation row alongside the campaign"
    );
}

/// Regression guard for the NOT NULL columns bug (T56 Phase G).
///
/// `persist_edge_candidate_evaluation` must write `pairs_improved = 0` and
/// `pairs_total_considered = 0` for every EdgeCandidate row — those columns
/// are NOT NULL on `campaign_evaluation_candidates` and have no DEFAULT.
/// Without this fix every edge_candidate `/evaluate` call fails at runtime
/// with a Postgres NOT NULL constraint violation.
///
/// Uses `create_evaluated_campaign(&h, "edge_candidate")` so the full
/// HTTP evaluate path runs, including the evaluator + persistence. A
/// successful return proves the NOT NULL constraint was satisfied.
/// The assertions then confirm the persisted aggregates are coherent.
#[tokio::test]
async fn persist_edge_candidate_evaluation_satisfies_not_null_constraints() {
    use meshmon_service::campaign::evaluation_repo;
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    // `create_evaluated_campaign` inserts agents + seeds measurements +
    // drives the full /evaluate HTTP path. A panic here means the NOT NULL
    // constraint was violated (the endpoint returns 500) or the test setup
    // itself is broken.
    let campaign_id_str = common::create_evaluated_campaign(&h, "edge_candidate").await;
    let campaign_id: uuid::Uuid = campaign_id_str.parse().expect("parse campaign id");

    // Read back the persisted evaluation. Must be Some — the HTTP path
    // already asserted a 200 response, but re-check at the repo layer.
    let eval = evaluation_repo::latest_evaluation_for_campaign(pool, campaign_id)
        .await
        .expect("db query succeeded")
        .expect("evaluation row exists after /evaluate");

    assert_eq!(
        eval.evaluation_mode,
        meshmon_service::campaign::model::EvaluationMode::EdgeCandidate,
        "evaluation must be in edge_candidate mode"
    );

    // The candidate count must be non-zero (seeded measurements cover
    // at least the 4 destination IPs in the helper fixture).
    assert!(
        eval.candidates_total > 0,
        "edge_candidate evaluation must have at least one candidate; got {}",
        eval.candidates_total
    );

    // Verify pairs_improved and pairs_total_considered are present and
    // valid (not violated by the constraint). The edge_candidate evaluator
    // writes 0 for both — this is the correct sentinel per design.
    let cand_rows: Vec<(i32, i32)> = sqlx::query_as(
        "SELECT pairs_improved, pairs_total_considered \
           FROM campaign_evaluation_candidates c \
           JOIN campaign_evaluations e ON e.id = c.evaluation_id \
          WHERE e.campaign_id = $1",
    )
    .bind(campaign_id)
    .fetch_all(pool)
    .await
    .expect("query candidates");

    assert!(
        !cand_rows.is_empty(),
        "candidate rows must exist in DB after persist"
    );
    for (improved, considered) in &cand_rows {
        assert_eq!(
            *improved, 0,
            "edge_candidate rows must have pairs_improved = 0 (sentinel)"
        );
        assert_eq!(
            *considered, 0,
            "edge_candidate rows must have pairs_total_considered = 0 (sentinel)"
        );
    }

    // Verify edge pair details were persisted (covers the pair-detail loop).
    let edge_pair_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) \
           FROM campaign_evaluation_edge_pair_details epd \
           JOIN campaign_evaluations e ON e.id = epd.evaluation_id \
          WHERE e.campaign_id = $1",
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await
    .expect("count edge pair details");

    assert!(
        edge_pair_count > 0,
        "edge_candidate evaluation must persist at least one edge pair detail row; got {edge_pair_count}"
    );
}

/// `persist_edge_candidate_evaluation` must snapshot the campaign's
/// `max_transit_rtt_ms` and `max_transit_stddev_ms` onto the parent
/// `campaign_evaluations` row. The route enumerator already consumes these
/// caps, so omitting them from the persisted row leaves Settings/audit
/// views with NULL caps that misrepresent what scoring values produced
/// the result.
#[tokio::test]
async fn persist_edge_candidate_evaluation_snapshots_transit_caps() {
    let h = common::HttpHarness::start().await;
    let pool = &h.state.pool;

    let campaign_id_str = common::create_evaluated_campaign(&h, "edge_candidate").await;

    // PATCH the caps onto the campaign — fresh patch dismisses the prior
    // evaluation, so re-evaluate to write a new parent row that picks
    // them up.
    let _: serde_json::Value = h
        .patch_json(
            &format!("/api/campaigns/{campaign_id_str}"),
            &serde_json::json!({
                "max_transit_rtt_ms": 250.0,
                "max_transit_stddev_ms": 12.5,
            }),
        )
        .await;
    let _: serde_json::Value = h
        .post_json_empty(&format!("/api/campaigns/{campaign_id_str}/evaluate"))
        .await;

    let campaign_id: uuid::Uuid = campaign_id_str.parse().expect("parse campaign id");
    let (rtt_cap, stddev_cap): (Option<f64>, Option<f64>) = sqlx::query_as(
        "SELECT max_transit_rtt_ms, max_transit_stddev_ms \
           FROM campaign_evaluations \
          WHERE campaign_id = $1 \
          ORDER BY evaluated_at DESC \
          LIMIT 1",
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await
    .expect("query evaluation caps");

    assert_eq!(
        rtt_cap,
        Some(250.0),
        "max_transit_rtt_ms must round-trip onto the edge_candidate evaluation row"
    );
    assert_eq!(
        stddev_cap,
        Some(12.5),
        "max_transit_stddev_ms must round-trip onto the edge_candidate evaluation row"
    );
}

/// `reverse_direction_measurements_for_campaign` must dedupe to the
/// most recent in-window measurement per `(source_agent_id,
/// destination_ip)` pair so the symmetry-fallback substitution is
/// reproducible. PostgreSQL's row order is implementation-defined
/// without an explicit ORDER BY; pre-fix, multiple in-window samples
/// for the same pair let `LegLookup`'s first-write-wins pick a
/// non-deterministic representative.
#[tokio::test]
async fn reverse_direction_measurements_dedup_to_latest_per_pair() {
    let pool = common::shared_migrated_pool().await;

    // Two agents: A is the campaign source, B is the destination.
    // Reverse direction is B→A (source=B, destination_ip=A.ip).
    let a_ip: std::net::IpAddr = "198.51.100.61".parse().unwrap();
    let b_ip: std::net::IpAddr = "198.51.100.62".parse().unwrap();
    common::insert_agent_with_ip(&pool, "t56-rev-a", a_ip).await;
    common::insert_agent_with_ip(&pool, "t56-rev-b", b_ip).await;

    let mut input = make_input("t-reverse-dedup");
    input.source_agent_ids = vec!["t56-rev-a".into()];
    input.destination_ips = vec![b_ip];
    let row = repo::create(&pool, input).await.unwrap();

    // Two reverse-direction (B → A.ip) measurements within the 24h
    // window — older one first, newer one second. The query must pick
    // the newer one (latency_avg_ms = 99.0) regardless of insert
    // order or PostgreSQL's underlying scan.
    let dst = sqlx::types::ipnetwork::IpNetwork::from(a_ip);
    sqlx::query(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             latency_avg_ms, loss_ratio, measured_at) \
         VALUES ($1, $2, 'icmp', 10, 50.0, 0.0, now() - interval '6 hours')",
    )
    .bind("t56-rev-b")
    .bind(dst)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO measurements \
            (source_agent_id, destination_ip, protocol, probe_count, \
             latency_avg_ms, loss_ratio, measured_at) \
         VALUES ($1, $2, 'icmp', 10, 99.0, 0.0, now() - interval '1 minute')",
    )
    .bind("t56-rev-b")
    .bind(dst)
    .execute(&pool)
    .await
    .unwrap();

    let rev = repo::reverse_direction_measurements_for_campaign(&pool, row.id)
        .await
        .expect("reverse-direction fetch");
    let matching: Vec<_> = rev
        .iter()
        .filter(|m| m.source_agent_id == "t56-rev-b" && m.destination_ip == a_ip)
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "DISTINCT ON must collapse multiple in-window samples to one row: got {matching:?}"
    );
    assert!(
        (matching[0].latency_avg_ms.unwrap_or(0.0) - 99.0).abs() < 1e-3,
        "DISTINCT ON + ORDER BY measured_at DESC must surface the latest sample: got {:?}",
        matching[0].latency_avg_ms
    );

    repo::delete(&pool, row.id).await.unwrap();
    sqlx::query("DELETE FROM measurements WHERE source_agent_id = 't56-rev-b'")
        .execute(&pool)
        .await
        .unwrap();
}
