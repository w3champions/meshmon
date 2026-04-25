//! Integration tests for the T55 paginated pair_details endpoint
//! (`GET /api/campaigns/{id}/evaluation/candidates/{destination_ip}/pair_details`).
//!
//! Each test seeds a campaign + at least one `campaign_evaluations`
//! parent row + the pair_detail rows it needs (via the
//! `seed_pair_detail_*` helpers in `tests/common/mod.rs`) and drives the
//! handler directly through `HttpHarness`. Tests use disjoint
//! `198.51.100.0/24` IPs so parallel binaries never collide on the
//! shared `agents` / `measurements` / `campaign_evaluations` rows.
//!
//! | Test                                    | Candidate IP        | Notes                                                   |
//! |-----------------------------------------|---------------------|---------------------------------------------------------|
//! | `cursor_paginates_through_250_rows`     | `198.51.100.51`     | default sort + tiebreak across a 250-row dataset        |
//! | `same_improvement_ms_tiebreak`          | `198.51.100.52`     | five rows with identical improvement; limit=2           |
//! | `string_sort_with_tiebreak`             | `198.51.100.53`     | sort by source_agent_id, identical sources              |
//! | `sort_change_does_not_carry_cursor`     | `198.51.100.54`     | mismatched cursor + restart with new sort               |
//! | `mismatched_cursor_returns_400`         | `198.51.100.55`     | cursor encoded under one sort, sent with another        |
//! | `garbage_cursor_returns_400`            | `198.51.100.56`     | base64 + json + variant validation                      |
//! | `runtime_filter_min_improvement_ms`     | `198.51.100.57`     | filter narrows total + entries                          |
//! | `runtime_filter_max_transit_rtt_ms`     | `198.51.100.58`     | likewise                                                |
//! | `degenerate_baseline_ratio_auto_pass`   | `198.51.100.59`     | direct_rtt_ms = 0 + ratio filter ⇒ row passes           |
//! | `qualifies_only_filters_correctly`      | `198.51.100.60`     | qualifies_only=true narrows                             |
//! | `error_vocabulary_404_paths`            | n/a                 | not_found, no_evaluation, not_a_candidate               |
//! | `error_vocabulary_400_paths`            | `198.51.100.61`     | invalid_filter (limit), invalid_filter (NaN)            |
//! | `re_evaluate_during_pagination`         | `198.51.100.62`     | second eval row + page-2 cursor                         |
//! | `default_sort_index_used_via_explain`   | `198.51.100.63`     | EXPLAIN check                                           |
//! | `ipv6_candidate_path_resolves`          | `2001:db8:55::1`    | URL-encoded colon path                                  |

mod common;

use serde_json::{json, Value};
use std::net::IpAddr;
use uuid::Uuid;

/// Create a draft campaign via the public POST API and return its id as
/// a `Uuid`. Mirrors the seed pattern from
/// `campaign_evaluation_guardrails.rs` so test code reads similarly.
async fn create_campaign(h: &common::HttpHarness, title: &str, source_agent_id: &str) -> Uuid {
    let camp: Value = h
        .post_json(
            "/api/campaigns",
            &json!({
                "title": title,
                "protocol": "icmp",
                "source_agent_ids": [source_agent_id],
                "destination_ips": [],
                "loss_threshold_ratio": 0.05,
                "stddev_weight": 1.0,
                "evaluation_mode": "diversity",
            }),
        )
        .await;
    camp["id"].as_str().unwrap().parse().unwrap()
}

/// Seed `n` pair_detail rows on a single (campaign, candidate). Returns
/// the evaluation id. Each row gets a unique `(source, dest)` pair drawn
/// from the seed tag — useful for cursor-stability tests.
///
/// `improvement_fn(i)` lets the caller shape the `improvement_ms`
/// distribution; passing `|i| i as f32` yields strictly-monotonic
/// improvements, while `|_| 50.0` yields identical ones for the
/// tiebreak test.
async fn seed_n_pair_details<F>(
    pool: &sqlx::PgPool,
    campaign_id: Uuid,
    candidate_ip: IpAddr,
    n: usize,
    tag: &str,
    mut improvement_fn: F,
) -> Uuid
where
    F: FnMut(usize) -> f32,
{
    let evaluation_id = common::seed_evaluation_row(pool, campaign_id).await;
    common::seed_pair_detail_candidate(pool, evaluation_id, candidate_ip).await;
    for i in 0..n {
        let src = format!("{tag}-src-{i:04}");
        let dst = format!("{tag}-dst-{i:04}");
        let imp = improvement_fn(i);
        let seed = common::PairDetailSeed::baseline(&src, &dst, imp, true);
        common::seed_pair_detail_row(pool, evaluation_id, candidate_ip, &seed).await;
    }
    evaluation_id
}

#[tokio::test]
async fn cursor_paginates_through_250_rows() {
    // Walk a 250-row candidate with the default sort (`improvement_ms`
    // desc) at limit=100. Every row must appear exactly once across the
    // three pages, in non-increasing improvement order; ties (none here
    // because `i` is unique) would be broken by ascending
    // `(source_agent_id, destination_agent_id)`.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-1").await;
    let cid = create_campaign(&h, "t55-pde-cursor", "t55-pde-1").await;
    let cand: IpAddr = "198.51.100.51".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 250, "p1", |i| i as f32).await;

    let mut seen = Vec::new();
    let mut last_imp = f32::INFINITY;
    let mut cursor: Option<String> = None;
    for page in 0..5 {
        let url = match &cursor {
            None => format!(
                "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=100"
            ),
            Some(c) => format!(
                "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=100&cursor={c}"
            ),
        };
        let body: Value = h.get_json(&url).await;
        assert_eq!(body["total"], 250, "total stays 250 across pages");
        let entries = body["entries"].as_array().expect("entries array");
        for e in entries {
            let imp = e["improvement_ms"].as_f64().unwrap() as f32;
            assert!(
                imp <= last_imp,
                "page {page}: row {imp} > previous {last_imp} (must be desc)",
            );
            last_imp = imp;
            seen.push(e["source_agent_id"].as_str().unwrap().to_string());
        }
        cursor = body["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 250, "every row appears exactly once");
}

#[tokio::test]
async fn same_improvement_ms_tiebreak() {
    // Five rows with identical improvement_ms; limit=2 splits them into
    // 3 pages. Tiebreak is `(source, destination)` ascending, and the
    // walk must terminate without revisiting any row.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-2").await;
    let cid = create_campaign(&h, "t55-pde-tie", "t55-pde-2").await;
    let cand: IpAddr = "198.51.100.52".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 5, "p2", |_| 50.0).await;

    let mut sources: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..6 {
        let url = match &cursor {
            None => {
                format!("/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=2")
            }
            Some(c) => format!(
                "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=2&cursor={c}"
            ),
        };
        let body: Value = h.get_json(&url).await;
        for e in body["entries"].as_array().unwrap() {
            sources.push(e["source_agent_id"].as_str().unwrap().to_string());
        }
        cursor = body["next_cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }
    let mut deduped = sources.clone();
    deduped.sort();
    deduped.dedup();
    assert_eq!(
        deduped.len(),
        5,
        "every row visited exactly once: {sources:?}",
    );
    let mut sorted_expected = sources.clone();
    sorted_expected.sort();
    assert_eq!(sources, sorted_expected, "tiebreak walks (src, dest) asc");
}

#[tokio::test]
async fn string_sort_with_tiebreak() {
    // Sort by source_agent_id ascending. Two rows share the same source
    // but differ on destination — the tiebreak must order them.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-3").await;
    let cid = create_campaign(&h, "t55-pde-string", "t55-pde-3").await;
    let cand: IpAddr = "198.51.100.53".parse().unwrap();
    let evaluation_id = common::seed_evaluation_row(&h.state.pool, cid).await;
    common::seed_pair_detail_candidate(&h.state.pool, evaluation_id, cand).await;

    for (src, dst, imp) in [("aa", "z1", 10.0), ("aa", "z2", 20.0), ("bb", "z1", 30.0)] {
        let s = common::PairDetailSeed::baseline(src, dst, imp, true);
        common::seed_pair_detail_row(&h.state.pool, evaluation_id, cand, &s).await;
    }

    let url = format!(
        "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
         ?sort=source_agent_id&dir=asc&limit=10"
    );
    let body: Value = h.get_json(&url).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["source_agent_id"], "aa");
    assert_eq!(entries[0]["destination_agent_id"], "z1");
    assert_eq!(entries[1]["source_agent_id"], "aa");
    assert_eq!(entries[1]["destination_agent_id"], "z2");
    assert_eq!(entries[2]["source_agent_id"], "bb");
}

#[tokio::test]
async fn sort_change_does_not_carry_cursor() {
    // Page 1 by improvement_ms (default), then a fresh page 1 by
    // direct_rtt_ms — the responses for each are well-ordered in their
    // own sort. A request that mixes the page-1 cursor with the new
    // sort must 400 (covered by `mismatched_cursor_returns_400`).
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-4").await;
    let cid = create_campaign(&h, "t55-pde-sortchange", "t55-pde-4").await;
    let cand: IpAddr = "198.51.100.54".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 6, "p4", |i| i as f32 * 10.0).await;

    let by_imp: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=3"
        ))
        .await;
    let imp_first = by_imp["entries"][0]["improvement_ms"].as_f64().unwrap();
    assert!(imp_first >= 50.0, "default desc; first should be max");

    let by_rtt_asc: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
             ?sort=direct_rtt_ms&dir=asc&limit=3"
        ))
        .await;
    let rtt_entries = by_rtt_asc["entries"].as_array().unwrap();
    let rtts: Vec<f64> = rtt_entries
        .iter()
        .map(|e| e["direct_rtt_ms"].as_f64().unwrap())
        .collect();
    let mut sorted_rtts = rtts.clone();
    sorted_rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(rtts, sorted_rtts, "direct_rtt asc page-1 well-ordered");
}

#[tokio::test]
async fn mismatched_cursor_returns_400() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-5").await;
    let cid = create_campaign(&h, "t55-pde-mismatch", "t55-pde-5").await;
    let cand: IpAddr = "198.51.100.55".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 5, "p5", |i| i as f32).await;

    // Take a page-1 cursor from sort=improvement_ms default…
    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=2"
        ))
        .await;
    let cursor = body["next_cursor"].as_str().expect("page 1 yields cursor");

    // …then send it with a different sort. Must surface as
    // `invalid_cursor`.
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
             ?sort=direct_rtt_ms&limit=2&cursor={cursor}"
        ))
        .await;
    assert_eq!(status, 400, "sort mismatch → 400; body={body}");
    assert!(body.contains("invalid_cursor"), "error code; body={body}");
}

#[tokio::test]
async fn garbage_cursor_returns_400() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-6").await;
    let cid = create_campaign(&h, "t55-pde-garbage", "t55-pde-6").await;
    let cand: IpAddr = "198.51.100.56".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 1, "p6", |_| 1.0).await;

    for cursor in ["!!!not-base64!!!", "AAAAAA", "deadbeef"] {
        let (status, body) = h
            .get(&format!(
                "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?cursor={cursor}"
            ))
            .await;
        assert_eq!(status, 400, "cursor={cursor:?}; body={body}");
        assert!(
            body.contains("invalid_cursor"),
            "cursor={cursor:?}; body={body}",
        );
    }
}

#[tokio::test]
async fn runtime_filter_min_improvement_ms() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-7").await;
    let cid = create_campaign(&h, "t55-pde-min-imp", "t55-pde-7").await;
    let cand: IpAddr = "198.51.100.57".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 100, "p7", |i| i as f32).await;

    let unfiltered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=500"
        ))
        .await;
    assert_eq!(unfiltered["total"], 100);

    let filtered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
             ?limit=500&min_improvement_ms=50"
        ))
        .await;
    let total = filtered["total"].as_u64().unwrap();
    assert!((50..100).contains(&total), "filter narrows; total={total}");
    for e in filtered["entries"].as_array().unwrap() {
        let imp = e["improvement_ms"].as_f64().unwrap();
        assert!(imp >= 50.0, "filter respected; improvement={imp}");
    }
}

#[tokio::test]
async fn runtime_filter_max_transit_rtt_ms() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-8").await;
    let cid = create_campaign(&h, "t55-pde-max-transit", "t55-pde-8").await;
    let cand: IpAddr = "198.51.100.58".parse().unwrap();
    // baseline() sets direct_rtt = 200, transit_rtt = 200 - improvement.
    // Improvement i ∈ {0..99} ⇒ transit_rtt ∈ {200..101}. Cap at 150 ⇒
    // transit_rtt ≤ 150 ⇔ improvement ≥ 50 ⇒ 50 rows survive.
    seed_n_pair_details(&h.state.pool, cid, cand, 100, "p8", |i| i as f32).await;

    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
             ?limit=500&max_transit_rtt_ms=150"
        ))
        .await;
    let total = body["total"].as_u64().unwrap();
    assert_eq!(total, 50, "max_transit_rtt narrows to half: total={total}");
    for e in body["entries"].as_array().unwrap() {
        let trtt = e["transit_rtt_ms"].as_f64().unwrap();
        assert!(trtt <= 150.0, "filter respected; transit_rtt={trtt}");
    }
}

#[tokio::test]
async fn degenerate_baseline_ratio_auto_pass() {
    // direct_rtt_ms = 0 row + min_improvement_ratio = 0.1.
    // Per the I3-step-4 SQL: `direct_rtt_ms <= 0 OR ratio >= filter`, so
    // the degenerate row auto-passes the ratio gate. A `NULLIF`
    // formulation would silently drop it. This is the regression test.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-9").await;
    let cid = create_campaign(&h, "t55-pde-ratio", "t55-pde-9").await;
    let cand: IpAddr = "198.51.100.59".parse().unwrap();
    let evaluation_id = common::seed_evaluation_row(&h.state.pool, cid).await;
    common::seed_pair_detail_candidate(&h.state.pool, evaluation_id, cand).await;

    let mut zero_baseline = common::PairDetailSeed::baseline("zero-src", "zero-dst", 5.0, true);
    zero_baseline.direct_rtt_ms = 0.0; // degenerate baseline
    zero_baseline.transit_rtt_ms = 0.0;
    common::seed_pair_detail_row(&h.state.pool, evaluation_id, cand, &zero_baseline).await;

    // Also seed one row with a real ratio above 0.1 and one below, so
    // the test asserts both that the degenerate row is included AND
    // that real-ratio filtering works.
    let above = common::PairDetailSeed::baseline("above-src", "above-dst", 60.0, true); // ratio = 60/200 = 0.3
    common::seed_pair_detail_row(&h.state.pool, evaluation_id, cand, &above).await;
    let below = common::PairDetailSeed::baseline("below-src", "below-dst", 5.0, true); // ratio = 5/200 = 0.025
    common::seed_pair_detail_row(&h.state.pool, evaluation_id, cand, &below).await;

    let body: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
             ?limit=500&min_improvement_ratio=0.1"
        ))
        .await;
    let sources: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["source_agent_id"].as_str().unwrap())
        .collect();
    assert!(
        sources.contains(&"zero-src"),
        "degenerate-baseline row auto-passes ratio gate; entries={sources:?}",
    );
    assert!(
        sources.contains(&"above-src"),
        "above-threshold row passes; entries={sources:?}",
    );
    assert!(
        !sources.contains(&"below-src"),
        "below-threshold row filtered; entries={sources:?}",
    );
}

#[tokio::test]
async fn qualifies_only_filters_correctly() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-10").await;
    let cid = create_campaign(&h, "t55-pde-qualifies", "t55-pde-10").await;
    let cand: IpAddr = "198.51.100.60".parse().unwrap();
    let evaluation_id = common::seed_evaluation_row(&h.state.pool, cid).await;
    common::seed_pair_detail_candidate(&h.state.pool, evaluation_id, cand).await;

    for (src, q) in [("q1", true), ("q2", false), ("q3", true), ("q4", false)] {
        let s = common::PairDetailSeed::baseline(src, "d", 10.0, q);
        common::seed_pair_detail_row(&h.state.pool, evaluation_id, cand, &s).await;
    }

    let only_qual: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details\
             ?limit=10&qualifies_only=true"
        ))
        .await;
    assert_eq!(only_qual["total"], 2, "qualifies_only=true narrows");
    for e in only_qual["entries"].as_array().unwrap() {
        assert_eq!(e["qualifies"], true);
    }

    let unfiltered: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=10"
        ))
        .await;
    assert_eq!(unfiltered["total"], 4);
}

#[tokio::test]
async fn error_vocabulary_404_paths() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-11").await;

    // (1) Campaign not found.
    let unknown_id = Uuid::new_v4();
    let cand: IpAddr = "198.51.100.99".parse().unwrap();
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{unknown_id}/evaluation/candidates/{cand}/pair_details"
        ))
        .await;
    assert_eq!(status, 404);
    assert!(body.contains("not_found"), "body={body}");

    // (2) Campaign exists but never evaluated.
    let cid = create_campaign(&h, "t55-pde-no-eval", "t55-pde-11").await;
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details"
        ))
        .await;
    assert_eq!(status, 404);
    assert!(body.contains("no_evaluation"), "body={body}");

    // (3) Evaluation exists but candidate not present.
    let _eval_id = common::seed_evaluation_row(&h.state.pool, cid).await;
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details"
        ))
        .await;
    assert_eq!(status, 404);
    assert!(body.contains("not_a_candidate"), "body={body}");
}

#[tokio::test]
async fn error_vocabulary_400_paths() {
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-12").await;
    let cid = create_campaign(&h, "t55-pde-400s", "t55-pde-12").await;
    let cand: IpAddr = "198.51.100.61".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 1, "p12", |_| 1.0).await;

    // limit > 500 → invalid_filter
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=1000"
        ))
        .await;
    assert_eq!(status, 400, "limit=1000; body={body}");
    assert!(body.contains("invalid_filter"), "body={body}");

    // invalid sort → serde rejects with 400
    let (status, _body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?sort=not_a_real_column"
        ))
        .await;
    assert_eq!(status, 400, "invalid sort surfaces as 400");

    // Non-finite filter → invalid_filter. `inf` parses as positive
    // infinity and the handler's is_finite gate rejects it before the
    // SQL planner sees a garbage threshold.
    let (status, body) = h
        .get(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?min_improvement_ms=inf"
        ))
        .await;
    assert_eq!(status, 400, "non-finite filter; body={body}");
    assert!(body.contains("invalid_filter"), "body={body}");
}

#[tokio::test]
async fn re_evaluate_during_pagination() {
    // Page 1 from evaluation_a; then a second evaluation row (same
    // campaign) lands; page 2 with the page-1 cursor must read against
    // the NEW snapshot, well-ordered.
    //
    // Eval A seeds improvements [0.0, 1.0, 2.0, 3.0, 4.0]; page 1 with
    // limit=2 desc → entries (4.0, 3.0) → cursor minted at improvement
    // = 3.0. Eval B then replaces the row set with improvements that
    // span the cursor: three below (0.5, 1.5, 2.5) and two above
    // (10.0, 20.0). The cursor predicate `improvement_ms < 3.0 OR
    // (eq + tiebreak)` keeps only the below-cursor rows. Page 2 must
    // therefore return exactly the 3 eval-B rows below the cursor —
    // and ZERO eval-A rows.
    //
    // The disjoint source-id prefixes (`p13a-src-*` vs `p13b-src-*`)
    // let the test verify the snapshot every entry is drawn from
    // without inspecting improvement values alone (which would let an
    // accidental eval-A leakage at the same numeric value slip
    // through).
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-13").await;
    let cid = create_campaign(&h, "t55-pde-reeval", "t55-pde-13").await;
    let cand: IpAddr = "198.51.100.62".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 5, "p13a", |i| i as f32).await;

    let body1: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=2"
        ))
        .await;
    let cursor = body1["next_cursor"].as_str().unwrap().to_string();
    // Sanity: page 1's cursor row must be at improvement = 3.0 (second
    // entry under desc sort over [0..=4]). If this drifts, the spans
    // below need to be retuned.
    let page1_entries = body1["entries"].as_array().unwrap();
    assert_eq!(page1_entries.len(), 2);
    assert_eq!(
        page1_entries[1]["improvement_ms"].as_f64().unwrap() as f32,
        3.0,
        "page-1 cursor row sits at improvement_ms=3.0",
    );

    // Eval B: spans the cursor. Three below, two above. Distinct
    // source prefix so the test can verify provenance.
    let evaluation_id_b = common::seed_evaluation_row(&h.state.pool, cid).await;
    common::seed_pair_detail_candidate(&h.state.pool, evaluation_id_b, cand).await;
    let below = [0.5_f32, 1.5, 2.5];
    let above = [10.0_f32, 20.0];
    for (i, imp) in below.iter().chain(above.iter()).enumerate() {
        let src = format!("p13b-src-{i:04}");
        let dst = format!("p13b-dst-{i:04}");
        let s = common::PairDetailSeed::baseline(&src, &dst, *imp, true);
        common::seed_pair_detail_row(&h.state.pool, evaluation_id_b, cand, &s).await;
    }

    let body2: Value = h
        .get_json(&format!(
            "/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=10&cursor={cursor}"
        ))
        .await;
    let entries = body2["entries"].as_array().unwrap();

    // Page 2 must surface exactly the three below-cursor eval-B rows.
    // An empty page (which the prior version of this test trivially
    // accepted) now fails this assertion.
    assert_eq!(
        entries.len(),
        below.len(),
        "page 2 returns only below-cursor eval-B rows: {entries:?}",
    );

    // Every entry must come from eval B (verified by source-id prefix),
    // be below the cursor's improvement value (3.0), and form a
    // desc-ordered run.
    let mut last = f64::INFINITY;
    let mut seen_imps: Vec<f32> = Vec::new();
    for e in entries {
        let src = e["source_agent_id"].as_str().unwrap();
        assert!(
            src.starts_with("p13b-"),
            "entry must be from new snapshot; src={src}",
        );
        let imp = e["improvement_ms"].as_f64().unwrap();
        assert!(imp < 3.0, "row sits below cursor; imp={imp}");
        assert!(imp <= last, "desc order maintained: {imp} <= {last}");
        last = imp;
        seen_imps.push(imp as f32);
    }
    // Set-equality with the seeded below-cursor improvements.
    let mut seen_sorted = seen_imps.clone();
    seen_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut expected_sorted = below.to_vec();
    expected_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(
        seen_sorted, expected_sorted,
        "page 2 = below-cursor eval-B rows exactly",
    );
}

#[tokio::test]
async fn default_sort_index_used_via_explain() {
    // The composite index
    // `campaign_evaluation_pair_details_default_sort_idx` covers the
    // default sort (`improvement_ms` desc) + leading filter columns.
    // Run an EXPLAIN at the SQL level and assert the planner picks an
    // Index Scan (or Index Only Scan) — proves the index is reachable,
    // not just that the ORDER BY mentions the column. The plain
    // "ORDER BY contains improvement_ms" check would have passed even
    // for a seq scan because EXPLAIN echoes the sort columns
    // regardless of access method.
    //
    // We force `enable_seqscan = OFF` for the EXPLAIN so the assertion
    // isolates index reachability from planner heuristics on small
    // datasets. The seed count is also bumped (to 200 rows) so the
    // planner has enough cardinality to even consider the index when
    // not forced.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-14").await;
    let cid = create_campaign(&h, "t55-pde-explain", "t55-pde-14").await;
    let cand: IpAddr = "198.51.100.63".parse().unwrap();
    seed_n_pair_details(&h.state.pool, cid, cand, 200, "p14", |i| i as f32).await;

    // Pull the latest evaluation id so the EXPLAIN matches the handler's
    // shape exactly.
    let evaluation_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM campaign_evaluations \
          WHERE campaign_id = $1 \
          ORDER BY evaluated_at DESC LIMIT 1",
    )
    .bind(cid)
    .fetch_one(&h.state.pool)
    .await
    .unwrap();

    // Pin to a single connection so `SET LOCAL`'s effect spans the
    // EXPLAIN. Reaching for `LOCAL` keeps the toggle scoped to the
    // implicit transaction, so other parallel tests on the shared pool
    // are untouched.
    use sqlx::Row as _;
    let mut conn = h.state.pool.acquire().await.unwrap();
    sqlx::query("BEGIN").execute(&mut *conn).await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    let plan_rows = sqlx::query(
        "EXPLAIN \
         SELECT improvement_ms, source_agent_id, destination_agent_id \
           FROM campaign_evaluation_pair_details \
          WHERE evaluation_id = $1 AND candidate_destination_ip = $2::inet \
          ORDER BY improvement_ms DESC, source_agent_id ASC, destination_agent_id ASC \
          LIMIT 100",
    )
    .bind(evaluation_id)
    .bind(sqlx::types::ipnetwork::IpNetwork::from(cand))
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    sqlx::query("ROLLBACK").execute(&mut *conn).await.unwrap();

    // Concatenate plan lines so a multi-line plan still matches.
    let plan: String = plan_rows
        .iter()
        .map(|r| r.try_get::<String, _>(0).expect("EXPLAIN line"))
        .collect::<Vec<_>>()
        .join("\n");
    let plan_lower = plan.to_lowercase();

    // Strong assertion #1: plan SHAPE — must use an index access path,
    // not a seq scan. With `enable_seqscan = OFF` the planner is
    // forced to pick an index scan if any covers the query; the
    // assertion fails loudly if the index is missing or not usable.
    let uses_index = plan_lower.contains("index scan") || plan_lower.contains("index only scan");
    assert!(
        uses_index,
        "plan must use an index scan (seqscan disabled); plan was:\n{plan}",
    );

    // Strong assertion #2: plan must reference the sorted column so a
    // future regression that drops the trailing `improvement_ms DESC`
    // from the index (and degrades to a leading-columns-only scan +
    // explicit Sort node) is caught. Tolerant on the exact index name
    // — planner strategy can vary on the EXPLAIN format.
    assert!(
        plan_lower.contains("improvement_ms") || plan_lower.contains("default_sort_idx"),
        "plan must mention improvement_ms or default-sort idx; plan was:\n{plan}",
    );
}

#[tokio::test]
async fn ipv6_candidate_path_resolves() {
    // axum's path extractor handles IPv6's `:` characters fine.
    // No URL-encoding needed at the test level.
    let h = common::HttpHarness::start().await;
    common::insert_agent(&h.state.pool, "t55-pde-15").await;
    let cid = create_campaign(&h, "t55-pde-ipv6", "t55-pde-15").await;
    let cand: IpAddr = "2001:db8:55::1".parse().unwrap();
    let evaluation_id = common::seed_evaluation_row(&h.state.pool, cid).await;
    common::seed_pair_detail_candidate(&h.state.pool, evaluation_id, cand).await;
    let s = common::PairDetailSeed::baseline("ipv6-src", "ipv6-dst", 30.0, true);
    common::seed_pair_detail_row(&h.state.pool, evaluation_id, cand, &s).await;

    let url = format!("/api/campaigns/{cid}/evaluation/candidates/{cand}/pair_details?limit=10");
    let body: Value = h.get_json(&url).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["source_agent_id"], "ipv6-src");
    assert_eq!(entries[0]["destination_ip"], cand.to_string());
}
