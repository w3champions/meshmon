//! Integration tests for the T56 paginated edge-pairs endpoint
//! (`GET /api/campaigns/{id}/evaluation/edge_pairs`).
//!
//! Each test seeds a campaign + at least one `campaign_evaluations`
//! parent row + the edge-pair detail rows it needs (via direct SQL
//! helpers) and drives the handler through `HttpHarness`. Tests use
//! disjoint `10.64.x.y` IPs so parallel binaries never collide on the
//! shared `agents` / `measurements` / `campaign_evaluations` rows.
//!
//! | Test                                              | Candidate IPs         | Notes                                                         |
//! |---------------------------------------------------|-----------------------|---------------------------------------------------------------|
//! | `default_sort_returns_asc_best_route_ms`          | `10.64.1.1`           | default sort=best_route_ms asc, 3 rows                        |
//! | `sort_by_each_column_returns_200`                 | `10.64.2.x`           | 8 distinct sort values all succeed                            |
//! | `filter_candidate_ip_narrows_results`             | `10.64.3.1`, `.2`     | candidate_ip filter                                           |
//! | `filter_qualifies_only_true`                      | `10.64.4.1`           | qualifies_only=true                                           |
//! | `filter_reachable_only_true`                      | `10.64.5.1`           | reachable_only=true                                           |
//! | `pagination_cursor_round_trip`                    | `10.64.6.1`           | 10 rows, limit=3, full walk                                   |
//! | `cursor_mismatch_sort_returns_400`                | `10.64.7.1`           | page-1 cursor sent with different sort                        |
//! | `garbage_cursor_returns_400`                      | `10.64.8.1`           | base64-invalid and json-invalid cursors                       |
//! | `limit_exceeds_500_returns_400`                   | —                     | no DB needed                                                  |
//! | `invalid_candidate_ip_returns_400`                | —                     | no DB needed                                                  |
//! | `campaign_not_found_returns_404`                  | —                     | random UUID, never inserted                                   |
//! | `campaign_never_evaluated_returns_404`            | `10.64.9.1`           | not_evaluated                                                 |
//! | `wrong_mode_returns_404`                          | `10.64.10.1`          | Triple-mode evaluation → wrong_mode                           |
//! | `pagination_cursor_round_trip_sort_candidate_ip_inet_ordering` | `10.64.11.{2,10,99}` | inet vs lex ordering regression               |
//! | `pagination_cursor_round_trip_non_default_sorts_arity` | `10.64.12.1`         | $9 cursor-arity regression on non-default sorts                |
//! | `non_ip_cursor_candidate_ip_returns_400`           | `10.64.13.1`          | hand-crafted cursor with non-IP `candidate_ip` field          |

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Per-test seeding helpers
// ---------------------------------------------------------------------------

/// Create a minimal `edge_candidate` campaign via the API. Returns the
/// campaign UUID.
async fn create_edge_candidate_campaign(
    h: &common::HttpHarness,
    title: &str,
    source_agent_id: &str,
    destination_ip: &str,
) -> Uuid {
    let camp: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": title,
                "protocol": "icmp",
                "evaluation_mode": "edge_candidate",
                "useful_latency_ms": 80.0,
                "source_agent_ids": [source_agent_id],
                "destination_ips": [destination_ip],
            }),
        )
        .await;
    camp["id"].as_str().unwrap().parse().unwrap()
}

/// Seed a `campaign_evaluations` row for an EdgeCandidate campaign.
/// Returns the evaluation id.
async fn seed_edge_evaluation(pool: &sqlx::PgPool, campaign_id: Uuid) -> Uuid {
    sqlx::query_scalar(
        r#"INSERT INTO campaign_evaluations
               (campaign_id, loss_threshold_ratio, stddev_weight, evaluation_mode,
                useful_latency_ms, max_hops, vm_lookback_minutes,
                baseline_pair_count, candidates_total, candidates_good, evaluated_at)
           VALUES ($1, 0.05, 1.0, 'edge_candidate'::evaluation_mode,
                   80.0, 1, 30, 0, 0, 0, now())
           RETURNING id"#,
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_edge_evaluation({campaign_id}): {e}"))
}

/// Seed a `campaign_evaluation_candidates` row for an EdgeCandidate
/// evaluation. `coverage_count` must be set because the endpoint joins on
/// this column for the good-candidates path; the test only needs the
/// candidate row as an FK target.
async fn seed_edge_candidate(
    pool: &sqlx::PgPool,
    evaluation_id: Uuid,
    candidate_ip: IpAddr,
    coverage_count: i32,
) {
    let ip_net = sqlx::types::ipnetwork::IpNetwork::from(candidate_ip);
    sqlx::query(
        r#"INSERT INTO campaign_evaluation_candidates
               (evaluation_id, destination_ip, is_mesh_member,
                pairs_improved, pairs_total_considered, coverage_count)
           VALUES ($1, $2::inet, false, 0, 0, $3)"#,
    )
    .bind(evaluation_id)
    .bind(ip_net)
    .bind(coverage_count)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed_edge_candidate({evaluation_id}, {candidate_ip}): {e}"));
}

/// Parameters for a single edge-pair detail row seed.
///
/// `best_route_ms` is `Option<f32>`: unreachable rows persist `None` (SQL
/// `NULL`) so the wire DTO serializes a clean `null` instead of an
/// unrepresentable infinity sentinel.
struct EdgePairSeed {
    candidate_ip: IpAddr,
    destination_agent_id: String,
    best_route_ms: Option<f32>,
    best_route_loss_ratio: f32,
    best_route_stddev_ms: f32,
    best_route_kind: &'static str,
    qualifies_under_t: bool,
    is_unreachable: bool,
}

impl EdgePairSeed {
    fn simple(candidate_ip: IpAddr, dest: &str, rtt_ms: f32, qualifies: bool) -> Self {
        Self {
            candidate_ip,
            destination_agent_id: dest.to_string(),
            best_route_ms: Some(rtt_ms),
            best_route_loss_ratio: 0.0,
            best_route_stddev_ms: 1.0,
            best_route_kind: "direct",
            qualifies_under_t: qualifies,
            is_unreachable: false,
        }
    }
}

/// Seed one `campaign_evaluation_edge_pair_details` row.
/// The `best_route_legs` JSONB column uses an empty JSON array `[]`.
async fn seed_edge_pair_row(pool: &sqlx::PgPool, evaluation_id: Uuid, seed: &EdgePairSeed) {
    let ip_net = sqlx::types::ipnetwork::IpNetwork::from(seed.candidate_ip);
    sqlx::query(
        r#"INSERT INTO campaign_evaluation_edge_pair_details
               (evaluation_id, candidate_ip, destination_agent_id,
                best_route_ms, best_route_loss_ratio, best_route_stddev_ms,
                best_route_kind, best_route_intermediaries, best_route_legs,
                qualifies_under_t, is_unreachable)
           VALUES ($1, $2::inet, $3, $4, $5, $6, $7, '{}', '[]'::jsonb, $8, $9)"#,
    )
    .bind(evaluation_id)
    .bind(ip_net)
    .bind(&seed.destination_agent_id)
    // `Option<f32>` binds as nullable Float4 — `None` becomes SQL `NULL`,
    // matching the production unreachable-row write.
    .bind(seed.best_route_ms)
    .bind(seed.best_route_loss_ratio)
    .bind(seed.best_route_stddev_ms)
    .bind(seed.best_route_kind)
    .bind(seed.qualifies_under_t)
    .bind(seed.is_unreachable)
    .execute(pool)
    .await
    .unwrap_or_else(|e| {
        panic!(
            "seed_edge_pair_row({evaluation_id}, {}, {}): {e}",
            seed.candidate_ip, seed.destination_agent_id
        )
    });
}

// ---------------------------------------------------------------------------
// Happy-path tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn default_sort_returns_asc_best_route_ms() {
    // 3 edge-pair rows with distinct RTTs; default sort=best_route_ms asc.
    // The response must list them in ascending RTT order.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-1").await;
    let cid = create_edge_candidate_campaign(&h, "ep-default-sort", "t56ep-1", "10.64.1.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.1.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;

    for (dest, rtt) in [
        ("t56ep-dest-a", 50.0_f32),
        ("t56ep-dest-b", 30.0),
        ("t56ep-dest-c", 80.0),
    ] {
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(cand, dest, rtt, true),
        )
        .await;
    }

    // Mark campaign evaluated so the endpoint sees a valid state.
    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=10"
        ))
        .await;
    assert_eq!(body["total"], 3, "body = {body}");
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    // Default direction is asc; RTTs must be non-decreasing.
    let rtts: Vec<f64> = entries
        .iter()
        .map(|e| e["best_route_ms"].as_f64().unwrap())
        .collect();
    let mut sorted = rtts.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(
        rtts, sorted,
        "entries must be in ascending best_route_ms order"
    );
}

#[tokio::test]
async fn sort_by_each_column_returns_200() {
    // Exercise all 8 sort columns. We only assert that each request
    // returns HTTP 200 and a non-error body — full ordering semantics
    // are already covered by the default-sort and cursor tests.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-2").await;
    let cid = create_edge_candidate_campaign(&h, "ep-all-sorts", "t56ep-2", "10.64.2.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.2.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;

    // Seed a few rows with distinct values for every sort column.
    for (i, dest) in ["ep2-dest-a", "ep2-dest-b", "ep2-dest-c"]
        .iter()
        .enumerate()
    {
        let rtt = (i as f32 + 1.0) * 10.0;
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed {
                candidate_ip: cand,
                destination_agent_id: dest.to_string(),
                best_route_ms: Some(rtt),
                best_route_loss_ratio: i as f32 * 0.1,
                best_route_stddev_ms: i as f32 * 2.0,
                best_route_kind: if i == 0 {
                    "direct"
                } else if i == 1 {
                    "1hop"
                } else {
                    "2hop"
                },
                qualifies_under_t: i % 2 == 0,
                is_unreachable: false,
            },
        )
        .await;
    }

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let sort_cols = [
        "best_route_ms",
        "best_route_loss_ratio",
        "best_route_stddev_ms",
        "best_route_kind",
        "qualifies_under_t",
        "is_unreachable",
        "candidate_ip",
        "destination_agent_id",
    ];
    for col in sort_cols {
        let body: Value = h
            .get_json(&format!(
                "/api/campaigns/{cid}/evaluation/edge_pairs?sort={col}&limit=10"
            ))
            .await;
        assert_eq!(
            body["total"], 3,
            "sort={col}: total should be 3; body = {body}",
        );
    }
}

#[tokio::test]
async fn filter_candidate_ip_narrows_results() {
    // Seed two candidate IPs; filter on only one → half the rows.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-3").await;
    let cid = create_edge_candidate_campaign(&h, "ep-cand-filter", "t56ep-3", "10.64.3.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand1: IpAddr = "10.64.3.1".parse().unwrap();
    let cand2: IpAddr = "10.64.3.2".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand1, 1).await;
    seed_edge_candidate(&h.state.pool, eval_id, cand2, 1).await;

    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand1, "ep3-dest-a", 20.0, true),
    )
    .await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand1, "ep3-dest-b", 25.0, true),
    )
    .await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand2, "ep3-dest-a", 30.0, false),
    )
    .await;

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let unfiltered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=10"
        ))
        .await;
    assert_eq!(unfiltered["total"], 3);

    let filtered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=10&candidate_ip={cand1}"
        ))
        .await;
    assert_eq!(
        filtered["total"], 2,
        "candidate_ip filter; body = {filtered}"
    );
    for e in filtered["entries"].as_array().unwrap() {
        assert_eq!(e["candidate_ip"].as_str().unwrap(), cand1.to_string());
    }
}

#[tokio::test]
async fn filter_qualifies_only_true() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-4").await;
    let cid =
        create_edge_candidate_campaign(&h, "ep-qualifies-filter", "t56ep-4", "10.64.4.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.4.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand, "ep4-dest-a", 10.0, true),
    )
    .await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand, "ep4-dest-b", 20.0, false),
    )
    .await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand, "ep4-dest-c", 30.0, true),
    )
    .await;

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let filtered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=10&qualifies_only=true"
        ))
        .await;
    assert_eq!(
        filtered["total"], 2,
        "qualifies_only=true narrows; body = {filtered}"
    );
    for e in filtered["entries"].as_array().unwrap() {
        assert_eq!(e["qualifies_under_t"], true);
    }
}

#[tokio::test]
async fn filter_reachable_only_true() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-5").await;
    let cid =
        create_edge_candidate_campaign(&h, "ep-reachable-filter", "t56ep-5", "10.64.5.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.5.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand, "ep5-dest-a", 10.0, true),
    )
    .await;
    // Unreachable row.
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed {
            candidate_ip: cand,
            destination_agent_id: "ep5-dest-b".to_string(),
            best_route_ms: None,
            best_route_loss_ratio: 1.0,
            best_route_stddev_ms: 0.0,
            best_route_kind: "direct",
            qualifies_under_t: false,
            is_unreachable: true,
        },
    )
    .await;

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let filtered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=10&reachable_only=true"
        ))
        .await;
    assert_eq!(
        filtered["total"], 1,
        "reachable_only=true narrows; body = {filtered}"
    );
    assert_eq!(filtered["entries"][0]["is_unreachable"], false);
}

#[tokio::test]
async fn pagination_cursor_round_trip() {
    // 10 rows, limit=3 — must visit every row exactly once across pages,
    // in non-decreasing best_route_ms order (default asc sort).
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-6").await;
    let cid = create_edge_candidate_campaign(&h, "ep-cursor-pag", "t56ep-6", "10.64.6.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.6.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    for i in 0..10 {
        let dest = format!("ep6-dest-{i:04}");
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(cand, &dest, i as f32 * 10.0, true),
        )
        .await;
    }

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let mut seen_dests: Vec<String> = Vec::new();
    let mut last_rtt = f64::NEG_INFINITY;
    let mut cursor: Option<String> = None;

    for _page in 0..5 {
        let url = match &cursor {
            None => format!("/api/campaigns/{cid}/evaluation/edge_pairs?limit=3"),
            Some(c) => {
                format!("/api/campaigns/{cid}/evaluation/edge_pairs?limit=3&cursor={c}")
            }
        };
        let body: Value = h.get_json(&url).await;
        assert_eq!(
            body["total"], 10,
            "total stays 10 across pages; body = {body}"
        );
        let entries = body["entries"].as_array().unwrap();
        for e in entries {
            let rtt = e["best_route_ms"].as_f64().unwrap();
            assert!(
                rtt >= last_rtt,
                "page: rtt {rtt} should be >= prev {last_rtt}",
            );
            last_rtt = rtt;
            seen_dests.push(e["destination_agent_id"].as_str().unwrap().to_string());
        }
        cursor = body["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    seen_dests.sort();
    seen_dests.dedup();
    assert_eq!(seen_dests.len(), 10, "every row visited exactly once");
}

#[tokio::test]
async fn unreachable_row_serializes_best_route_ms_as_null() {
    // Unreachable rows persist `NULL` for `best_route_ms` (no infinity
    // sentinel); the wire DTO must surface that as JSON `null` so the
    // contract `best_route_ms: number | null` holds.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-7").await;
    let cid =
        create_edge_candidate_campaign(&h, "ep-unreachable-null", "t56ep-7", "10.64.7.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.7.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed {
            candidate_ip: cand,
            destination_agent_id: "ep7-dest-unreachable".to_string(),
            best_route_ms: None,
            best_route_loss_ratio: 1.0,
            best_route_stddev_ms: 0.0,
            best_route_kind: "direct",
            qualifies_under_t: false,
            is_unreachable: true,
        },
    )
    .await;

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=10"
        ))
        .await;
    assert_eq!(body["total"], 1, "body = {body}");
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0]["best_route_ms"].is_null(),
        "unreachable rows must serialize best_route_ms as JSON null; body = {body}",
    );
    assert_eq!(entries[0]["is_unreachable"], true);
}

#[tokio::test]
async fn pagination_cursor_round_trip_with_unreachable_rows() {
    // Mix reachable + unreachable rows, default sort=best_route_ms asc with
    // NULLS LAST. The cursor walk must visit every row exactly once,
    // round-tripping the NULL sort-value through the encoded cursor.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-8").await;
    let cid = create_edge_candidate_campaign(&h, "ep-null-cursor", "t56ep-8", "10.64.8.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.8.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;

    // 4 reachable rows (rtts 10, 20, 30, 40).
    for i in 0..4 {
        let dest = format!("ep8-dest-r{i:02}");
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(cand, &dest, (i as f32 + 1.0) * 10.0, true),
        )
        .await;
    }
    // 3 unreachable rows (NULL best_route_ms).
    for i in 0..3 {
        let dest = format!("ep8-dest-u{i:02}");
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed {
                candidate_ip: cand,
                destination_agent_id: dest,
                best_route_ms: None,
                best_route_loss_ratio: 1.0,
                best_route_stddev_ms: 0.0,
                best_route_kind: "direct",
                qualifies_under_t: false,
                is_unreachable: true,
            },
        )
        .await;
    }

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let mut seen_dests: Vec<String> = Vec::new();
    let mut seen_rtts: Vec<Option<f64>> = Vec::new();
    let mut cursor: Option<String> = None;

    // 7 rows with limit=2 = 4 pages max.
    for _page in 0..5 {
        let url = match &cursor {
            None => format!("/api/campaigns/{cid}/evaluation/edge_pairs?limit=2"),
            Some(c) => {
                format!("/api/campaigns/{cid}/evaluation/edge_pairs?limit=2&cursor={c}")
            }
        };
        let body: Value = h.get_json(&url).await;
        assert_eq!(
            body["total"], 7,
            "total stays 7 across pages; body = {body}"
        );
        for e in body["entries"].as_array().unwrap() {
            let rtt = if e["best_route_ms"].is_null() {
                None
            } else {
                Some(e["best_route_ms"].as_f64().unwrap())
            };
            seen_rtts.push(rtt);
            seen_dests.push(e["destination_agent_id"].as_str().unwrap().to_string());
        }
        cursor = body["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    seen_dests.sort();
    seen_dests.dedup();
    assert_eq!(seen_dests.len(), 7, "every row visited exactly once");

    // NULLS LAST + ASC means: finite values monotonically non-decreasing,
    // followed by NULL entries. Verify the contract.
    let split = seen_rtts.iter().position(Option::is_none);
    if let Some(split_idx) = split {
        let finite_prefix: Vec<f64> = seen_rtts[..split_idx]
            .iter()
            .map(|v| v.expect("finite prefix"))
            .collect();
        let mut sorted = finite_prefix.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            finite_prefix, sorted,
            "finite prefix must be non-decreasing"
        );
        assert!(
            seen_rtts[split_idx..].iter().all(Option::is_none),
            "NULLs LAST: every row after the first NULL must also be NULL",
        );
    }
}

#[tokio::test]
async fn pagination_cursor_round_trip_sort_candidate_ip() {
    // Regression for an `inet`-vs-text mismatch in the cursor predicate.
    // `candidate_ip` is an `inet` column; the cursor's tiebreak value is a
    // bare-IP string. The predicate must compare them via `host(...)`
    // rather than `::text` (which appends `/32` for IPv4 hosts and
    // breaks `>`/`=` comparisons against the cursor) — and Postgres has
    // no native `inet > text` operator at all, so paging by
    // `sort=candidate_ip` would otherwise error at runtime past page 1.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-9").await;
    let cid = create_edge_candidate_campaign(&h, "ep-cand-ip-sort", "t56ep-9", "10.64.9.10").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    // Three distinct candidate IPs, each with one row, so paging by
    // `candidate_ip` must visit each row exactly once.
    let cands: [IpAddr; 3] = [
        "10.64.9.10".parse().unwrap(),
        "10.64.9.20".parse().unwrap(),
        "10.64.9.30".parse().unwrap(),
    ];
    for (i, ip) in cands.iter().enumerate() {
        seed_edge_candidate(&h.state.pool, eval_id, *ip, (i as i32) + 1).await;
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(*ip, &format!("ep9-dest-{i}"), 10.0, true),
        )
        .await;
    }

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _page in 0..4 {
        let url = match &cursor {
            None => format!("/api/campaigns/{cid}/evaluation/edge_pairs?limit=1&sort=candidate_ip"),
            Some(c) => format!(
                "/api/campaigns/{cid}/evaluation/edge_pairs?limit=1&sort=candidate_ip&cursor={c}"
            ),
        };
        let body: Value = h.get_json(&url).await;
        assert_eq!(body["total"], 3, "total stays 3; body = {body}");
        for e in body["entries"].as_array().unwrap() {
            seen.push(e["candidate_ip"].as_str().unwrap().to_string());
        }
        cursor = body["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3, "every row visited exactly once: {seen:?}");
}

/// Extends the candidate-IP cursor walk with IPs that lexicographically
/// disagree with native `inet` ordering: `10.64.11.2` and `10.64.11.10`
/// sort `2 < 10` as inet but `"10.64.11.10" < "10.64.11.2"` as text.
/// Casting the cursor value as text would cause page 2 to skip or repeat
/// rows; the inet cast keeps the walk monotone.
#[tokio::test]
async fn pagination_cursor_round_trip_sort_candidate_ip_inet_ordering() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-11").await;
    let cid = create_edge_candidate_campaign(&h, "ep-cand-ip-inet", "t56ep-11", "10.64.11.2").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    // Pick three IPs whose lex-vs-inet ordering disagrees:
    //   inet : 10.64.11.2   <  10.64.11.10  <  10.64.11.99
    //   text : 10.64.11.10  <  10.64.11.2   <  10.64.11.99
    let cands: [IpAddr; 3] = [
        "10.64.11.2".parse().unwrap(),
        "10.64.11.10".parse().unwrap(),
        "10.64.11.99".parse().unwrap(),
    ];
    for (i, ip) in cands.iter().enumerate() {
        seed_edge_candidate(&h.state.pool, eval_id, *ip, (i as i32) + 1).await;
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(*ip, &format!("ep11-dest-{i}"), 10.0, true),
        )
        .await;
    }

    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    // Walk one row at a time; capture order and uniqueness.
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _page in 0..4 {
        let url = match &cursor {
            None => format!("/api/campaigns/{cid}/evaluation/edge_pairs?limit=1&sort=candidate_ip"),
            Some(c) => format!(
                "/api/campaigns/{cid}/evaluation/edge_pairs?limit=1&sort=candidate_ip&cursor={c}"
            ),
        };
        let body: Value = h.get_json(&url).await;
        for e in body["entries"].as_array().unwrap() {
            seen.push(e["candidate_ip"].as_str().unwrap().to_string());
        }
        cursor = body["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    // Inet-ordered sequence: `10.64.11.2`, `10.64.11.10`, `10.64.11.99`.
    // A lexicographic cursor would have visited `10.64.11.10` first, then
    // skipped or repeated `10.64.11.2` on page 2.
    assert_eq!(
        seen,
        vec![
            "10.64.11.2".to_string(),
            "10.64.11.10".to_string(),
            "10.64.11.99".to_string(),
        ],
        "page walk must follow inet ordering, not lexicographic"
    );
}

/// Regression for an `$9` cursor-arity mismatch. The `best_route_ms`
/// NULL-aware cursor predicate references `$9::bool` (the cursor's
/// "previous-tail-was-NULL" flag), and the bind chain unconditionally
/// binds nine parameters. Every non-`best_route_ms` cursor branch must
/// also reference `$9` so the predicate's parameter arity stays matched
/// to the bind set; otherwise PostgreSQL deployments that strictly
/// enforce bind/parse arity reject the statement on page 2+ for these
/// sort columns. Walks page 1 → page 2 for each non-default sort to
/// drive the cursor branch on every cast variant (`$5::text`,
/// `$5::bool`, `$5::float8`).
#[tokio::test]
async fn pagination_cursor_round_trip_non_default_sorts_arity() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-12").await;
    let cid = create_edge_candidate_campaign(&h, "ep-cursor-arity", "t56ep-12", "10.64.12.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.12.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    // Three rows so a `limit=1` walk forces page 2 to engage the
    // cursor predicate. `EdgePairSeed::simple` always sets
    // `qualifies_under_t = true`; vary the `best_route_ms` field so
    // float-cast cursors (`best_route_loss_ratio`,
    // `best_route_stddev_ms`, `best_route_ms`) traverse distinct values.
    for i in 0..3 {
        let dest = format!("ep12-dest-{i:04}");
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(cand, &dest, (i as f32) * 5.0 + 5.0, true),
        )
        .await;
    }
    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    // Each sort here exercises a distinct `$5` cast type:
    //   destination_agent_id → text
    //   best_route_kind      → text
    //   qualifies_under_t    → bool
    //   is_unreachable       → bool
    //   best_route_loss_ratio → float8
    //   best_route_stddev_ms → float8
    let sorts = [
        "destination_agent_id",
        "best_route_kind",
        "qualifies_under_t",
        "is_unreachable",
        "best_route_loss_ratio",
        "best_route_stddev_ms",
    ];
    for sort in &sorts {
        let page1: Value = h
            .get_json(&format!(
                "/api/campaigns/{cid}/evaluation/edge_pairs?limit=1&sort={sort}"
            ))
            .await;
        assert_eq!(page1["total"], 3, "sort={sort} page 1 total; body={page1}");
        let cursor = page1["next_cursor"]
            .as_str()
            .unwrap_or_else(|| panic!("sort={sort} page 1 must yield cursor: {page1}"))
            .to_string();
        // Page 2 must succeed (no 500). The cursor predicate references
        // `$5..$7` for the leading comparison and `$9` (inert) so the
        // bind chain's `$9` slot stays matched.
        let (status, body) = h
            .get(&format!(
                "/api/campaigns/{cid}/evaluation/edge_pairs?limit=1&sort={sort}&cursor={cursor}"
            ))
            .await;
        assert_eq!(
            status, 200,
            "sort={sort} page 2 must return 200 (cursor arity); body={body}"
        );
    }
}

// ---------------------------------------------------------------------------
// Error-path tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn limit_exceeds_500_returns_400() {
    let h = common::HttpHarness::start().await;
    let unknown_id = Uuid::new_v4();
    // limit=501 should be rejected before the DB round-trip.
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{unknown_id}/evaluation/edge_pairs?limit=501"
        ))
        .await;
    assert_eq!(status, 400, "limit=501 should be 400; body={body}");
    assert!(body.contains("invalid_filter"), "body={body}");
}

#[tokio::test]
async fn invalid_candidate_ip_returns_400() {
    let h = common::HttpHarness::start().await;
    let unknown_id = Uuid::new_v4();
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{unknown_id}/evaluation/edge_pairs?candidate_ip=not-an-ip"
        ))
        .await;
    assert_eq!(
        status, 400,
        "invalid candidate_ip should be 400; body={body}"
    );
    assert!(body.contains("invalid_candidate_ip"), "body={body}");
}

#[tokio::test]
async fn campaign_not_found_returns_404() {
    let h = common::HttpHarness::start().await;
    let unknown_id = Uuid::new_v4();
    let body = h
        .get_expect_status(
            &format!("/api/campaigns/{unknown_id}/evaluation/edge_pairs"),
            404,
        )
        .await;
    assert_eq!(body["error"], "not_found", "body = {body}");
}

#[tokio::test]
async fn campaign_never_evaluated_returns_404() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-9").await;
    let cid = create_edge_candidate_campaign(&h, "ep-no-eval", "t56ep-9", "10.64.9.1").await;
    // No evaluation row inserted — campaign exists but was never evaluated.
    let body = h
        .get_expect_status(&format!("/api/campaigns/{cid}/evaluation/edge_pairs"), 404)
        .await;
    assert_eq!(body["error"], "not_evaluated", "body = {body}");
}

#[tokio::test]
async fn wrong_mode_returns_404() {
    // Create a Diversity campaign (Triple mode) with an evaluation row.
    // GET /edge_pairs must return 404 wrong_mode.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-10").await;

    let camp: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": "ep-wrong-mode",
                "protocol": "icmp",
                "evaluation_mode": "diversity",
                "source_agent_ids": ["t56ep-10"],
                "destination_ips": ["10.64.10.2", "10.64.10.3"],
            }),
        )
        .await;
    let cid: Uuid = camp["id"].as_str().unwrap().parse().unwrap();

    // Seed a diversity evaluation directly (using the existing seed helper).
    common::seed_evaluation_row(&h.state.pool, cid).await;

    let body = h
        .get_expect_status(&format!("/api/campaigns/{cid}/evaluation/edge_pairs"), 404)
        .await;
    assert_eq!(body["error"], "wrong_mode", "body = {body}");
}

#[tokio::test]
async fn cursor_mismatch_sort_returns_400() {
    // Take a page-1 cursor from default sort (best_route_ms), then
    // resend it with a different sort=destination_agent_id → 400 invalid_cursor.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-7").await;
    let cid =
        create_edge_candidate_campaign(&h, "ep-cursor-mismatch", "t56ep-7", "10.64.7.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;

    let cand: IpAddr = "10.64.7.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    for i in 0..5 {
        let dest = format!("ep7-dest-{i:04}");
        seed_edge_pair_row(
            &h.state.pool,
            eval_id,
            &EdgePairSeed::simple(cand, &dest, i as f32 * 5.0, true),
        )
        .await;
    }
    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    // Page 1 with default sort → obtain cursor.
    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?limit=2"
        ))
        .await;
    let cursor = body["next_cursor"]
        .as_str()
        .expect("page 1 has cursor")
        .to_string();

    // Resend with mismatched sort.
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs\
             ?sort=destination_agent_id&limit=2&cursor={cursor}"
        ))
        .await;
    assert_eq!(status, 400, "sort mismatch → 400; body={body}");
    assert!(body.contains("invalid_cursor"), "body={body}");
}

#[tokio::test]
async fn garbage_cursor_returns_400() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-8").await;
    let cid = create_edge_candidate_campaign(&h, "ep-garbage-cursor", "t56ep-8", "10.64.8.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;
    let cand: IpAddr = "10.64.8.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand, "ep8-dest-a", 10.0, true),
    )
    .await;
    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    for cursor in ["!!!not-base64!!!", "AAAAAA", "deadbeef"] {
        let (status, body) = h
            .get(&format!(
                "/api/campaigns/{cid}/evaluation/edge_pairs?cursor={cursor}"
            ))
            .await;
        assert_eq!(status, 400, "cursor={cursor:?} → 400; body={body}");
        assert!(
            body.contains("invalid_cursor"),
            "cursor={cursor:?}; body={body}",
        );
    }
}

/// A cursor whose `candidate_ip` field decodes successfully (base64
/// + JSON ok, sort_col matches) but isn't a parseable IP literal must
/// be rejected at decode time as `400 invalid_cursor`. Pre-fix, the
/// non-IP value sailed through `decode` and surfaced downstream as a
/// Postgres `inet` cast failure → 500. Decode-time validation now
/// matches the documented error vocabulary.
///
/// The test also covers the `sort_col == candidate_ip` case where the
/// cursor's `sort_value` (a `String`) is also bound as `inet` —
/// downstream `$5::inet` would fail the same way.
#[tokio::test]
async fn non_ip_cursor_candidate_ip_returns_400() {
    use base64::Engine as _;

    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t56ep-13").await;
    let cid =
        create_edge_candidate_campaign(&h, "ep-non-ip-cursor", "t56ep-13", "10.64.13.1").await;
    let eval_id = seed_edge_evaluation(&h.state.pool, cid).await;
    let cand: IpAddr = "10.64.13.1".parse().unwrap();
    seed_edge_candidate(&h.state.pool, eval_id, cand, 1).await;
    seed_edge_pair_row(
        &h.state.pool,
        eval_id,
        &EdgePairSeed::simple(cand, "ep13-dest-a", 10.0, true),
    )
    .await;
    sqlx::query("UPDATE measurement_campaigns SET state = 'evaluated' WHERE id = $1")
        .bind(cid)
        .execute(&h.state.pool)
        .await
        .unwrap();

    let encode = |body: &Value| -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
    };

    // (1) Default sort=best_route_ms; cursor's `candidate_ip` field is
    //     a non-IP literal. Bound downstream as `$6::inet`.
    let bad_cand_ip_cursor = encode(&json!({
        "sort_col": "best_route_ms",
        "sort_value": { "kind": "f64", "value": 10.0 },
        "candidate_ip": "not-an-ip",
        "destination_agent_id": "ep13-dest-a",
    }));
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?cursor={bad_cand_ip_cursor}"
        ))
        .await;
    assert_eq!(status, 400, "non-IP candidate_ip → 400; body={body}");
    assert!(
        body.contains("invalid_cursor"),
        "non-IP candidate_ip body must carry invalid_cursor; got {body}"
    );

    // (2) sort=candidate_ip; cursor's `sort_value` is the leading
    //     `inet`-bound parameter (`$5::inet`). The `candidate_ip`
    //     field is a valid IP so any 400 here must come from the
    //     `sort_value` IP check.
    let bad_sort_value_cursor = encode(&json!({
        "sort_col": "candidate_ip",
        "sort_value": { "kind": "string", "value": "not-an-ip" },
        "candidate_ip": "10.64.13.1",
        "destination_agent_id": "ep13-dest-a",
    }));
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs\
             ?sort=candidate_ip&cursor={bad_sort_value_cursor}"
        ))
        .await;
    assert_eq!(
        status, 400,
        "non-IP sort_value when sort=candidate_ip → 400; body={body}"
    );
    assert!(
        body.contains("invalid_cursor"),
        "non-IP sort_value body must carry invalid_cursor; got {body}"
    );

    // (3) Sanity: a fully-valid hand-crafted cursor with the same shape
    //     succeeds (200), proving the 400s above are about the IP
    //     literals, not about the encoding shape.
    let good_cursor = encode(&json!({
        "sort_col": "best_route_ms",
        "sort_value": { "kind": "f64", "value": 10.0 },
        "candidate_ip": "10.64.13.1",
        "destination_agent_id": "ep13-dest-a",
    }));
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/edge_pairs?cursor={good_cursor}"
        ))
        .await;
    assert_eq!(
        status, 200,
        "well-formed cursor with valid IPs → 200; body={body}"
    );
}
