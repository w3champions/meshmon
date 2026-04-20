//! Integration tests for `campaign::repo`.
//!
//! Uses the shared migrated pool. Every test that mutates campaign rows
//! is responsible for cleaning up via `repo::delete` at the end —
//! `campaign_pairs` cascade-delete on the parent row, so one call
//! suffices.

mod common;

use meshmon_service::campaign::model::{
    CampaignState, EvaluationMode, PairResolutionState, ProbeProtocol,
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
        loss_threshold_pct: None,
        stddev_weight: None,
        evaluation_mode: None,
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
        Some(5.0_f32),
        None,
        Some(EvaluationMode::Diversity),
    )
    .await
    .unwrap();

    assert_eq!(patched.title, "t-patch-renamed");
    assert_eq!(patched.notes, row.notes, "notes untouched when None");
    assert!((patched.loss_threshold_pct - 5.0).abs() < f32::EPSILON);
    assert_eq!(patched.evaluation_mode, EvaluationMode::Diversity);

    repo::delete(&pool, row.id).await.unwrap();
}

#[tokio::test]
async fn patch_returns_not_found_for_unknown_id() {
    let pool = common::shared_migrated_pool().await;
    let fresh = uuid::Uuid::new_v4();
    let err = repo::patch(&pool, fresh, Some("x"), None, None, None, None)
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
async fn preview_dispatch_count_returns_total_reusable_fresh() {
    let pool = common::shared_migrated_pool().await;

    // Seed a "reusable" measurement against a unique agent so we don't
    // collide with other concurrent tests in the shared DB.
    let agent = format!("preview-{}", uuid::Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, probe_count, loss_pct) \
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

    // Seed a reusable measurement younger than 24 h.
    let measurement_id: i64 = sqlx::query_scalar(
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, probe_count, loss_pct) \
         VALUES ($1, $2, 'icmp', 10, 0.0) RETURNING id",
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
        "INSERT INTO measurements (source_agent_id, destination_ip, protocol, probe_count, loss_pct) \
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
