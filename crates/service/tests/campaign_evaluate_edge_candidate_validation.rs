//! T56 — API-layer validation for new knobs and dismissal on knob changes.
//!
//! # Coverage
//!
//! | Test                                                 | Agents          | IPs (TEST-NET-2) |
//! |------------------------------------------------------|-----------------|------------------|
//! | `create_campaign_edge_candidate_without_useful_latency_returns_400` | — | — |
//! | `create_campaign_useful_latency_zero_returns_400`    | —               | —                |
//! | `create_campaign_diversity_with_max_hops_zero_returns_400` | —         | —                |
//! | `create_campaign_max_hops_three_returns_400`         | —               | —                |
//! | `create_campaign_vm_lookback_zero_returns_400`       | —               | —                |
//! | `create_campaign_vm_lookback_too_large_returns_400`  | —               | —                |
//! | `patch_max_hops_dismisses_evaluation`                | `t56v-{a,b,c}`  | `198.51.100.{71,.72,.73,.79}` |

mod common;
use common::HttpHarness;
use serde_json::json;

// ---------------------------------------------------------------------------
// B1 — CREATE validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_campaign_edge_candidate_without_useful_latency_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "no useful latency",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"]
                // useful_latency_ms intentionally omitted
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "useful_latency_required", "body = {body}");
}

#[tokio::test]
async fn create_campaign_useful_latency_zero_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "zero useful latency",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "useful_latency_ms": 0.0,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "useful_latency_invalid", "body = {body}");
}

#[tokio::test]
async fn create_campaign_diversity_with_max_hops_zero_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "diversity zero hops",
                "evaluation_mode": "diversity",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "max_hops": 0,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "max_hops_invalid_for_mode", "body = {body}");
}

#[tokio::test]
async fn create_campaign_max_hops_three_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "max hops three",
                "evaluation_mode": "edge_candidate",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "useful_latency_ms": 80.0,
                "max_hops": 3,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "max_hops_out_of_range", "body = {body}");
}

#[tokio::test]
async fn create_campaign_vm_lookback_zero_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "vm lookback zero",
                "evaluation_mode": "optimization",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "vm_lookback_minutes": 0,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "vm_lookback_out_of_range", "body = {body}");
}

#[tokio::test]
async fn create_campaign_vm_lookback_too_large_returns_400() {
    let h = HttpHarness::start().await;
    let body = h
        .post_expect_status(
            "/api/campaigns",
            &json!({
                "title": "vm lookback too large",
                "evaluation_mode": "optimization",
                "protocol": "icmp",
                "source_agent_ids": ["sa1"],
                "destination_ips": ["198.51.100.1"],
                "vm_lookback_minutes": 1441,
            }),
            400,
        )
        .await;
    assert_eq!(body["error"], "vm_lookback_out_of_range", "body = {body}");
}

// ---------------------------------------------------------------------------
// B2 — PATCH dismisses evaluation on knob changes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_max_hops_dismisses_evaluation() {
    let h = HttpHarness::start().await;
    let id = common::create_evaluated_campaign(&h, "diversity").await;

    // Change max_hops — evaluation must be dismissed.
    let _: serde_json::Value = h
        .patch_json(&format!("/api/campaigns/{id}"), &json!({"max_hops": 1}))
        .await;

    let eval = h
        .get_expect_status(&format!("/api/campaigns/{id}/evaluation"), 404)
        .await;
    assert_eq!(
        eval["error"], "not_evaluated",
        "evaluation should be dismissed after max_hops change; body = {eval}"
    );
}
