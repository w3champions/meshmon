//! End-to-end test for the catalogue paste pipeline: paste 100 IPs,
//! observe 100 `enrichment_progress` SSE frames, and assert every row
//! lands in `enriched` state.
//!
//! Wiring exercised:
//! 1. `POST /api/catalogue` — paste handler, parse tokens, insert rows,
//!    publish `Created` events, enqueue ids on the enrichment queue.
//! 2. [`meshmon_service::enrichment::runner::Runner`] (50 ms sweep) —
//!    drains the queue, walks the deterministic provider chain, and
//!    writes merged fields back to the row.
//! 3. [`meshmon_service::catalogue::events::CatalogueBroker`] — fans the
//!    runner's `EnrichmentProgress` events out to SSE subscribers.
//! 4. `GET /api/catalogue/stream` — the SSE handler carries those events
//!    to the test over a real TCP socket (not `oneshot`, since SSE is
//!    streaming).
//!
//! Isolation: a fresh Postgres database per test via
//! [`common::acquire(false)`] so the `GET /api/catalogue?limit=500`
//! terminal assertion does not race other binaries' rows.

mod common;

use common::{HttpHarness, TestProviders};
use futures::StreamExt;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn paste_100_ips_and_observe_sse_progress() {
    let h = HttpHarness::start_with_providers(TestProviders::deterministic_city()).await;

    // Subscribe BEFORE the paste so no `enrichment_progress` frame can
    // slip past between the POST return and the SSE subscription. The
    // broker is broadcast-semantics — late subscribers miss prior events.
    let mut sse = h.sse("/api/catalogue/stream").await;

    // TEST-NET-1 (`192.0.2.0/24`, RFC 5737) so the inserts can never
    // collide with other test binaries sharing the Postgres cluster.
    // `acquire(false)` already gives per-test DB isolation, but picking
    // a distinct range keeps the test grep-friendly alongside the
    // `catalogue_http.rs` allocation table.
    let ips: Vec<String> = (1..=100).map(|n| format!("192.0.2.{n}")).collect();
    let resp: serde_json::Value = h
        .post_json("/api/catalogue", &serde_json::json!({ "ips": ips }))
        .await;
    assert_eq!(
        resp["created"].as_array().expect("created is array").len(),
        100,
        "expected 100 new rows, got {resp}"
    );

    // Every row produces exactly one `enrichment_progress` frame with
    // `status == "enriched"` once the runner walks the chain. The 5 s
    // deadline covers deterministic provider latency (none), queue
    // draining, DB round-trips, and SSE flush on CI.
    let mut progress = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while let Ok(Some(res)) = tokio::time::timeout_at(deadline, sse.next()).await {
        let ev = res.expect("sse frame parse");
        if ev["kind"] == "enrichment_progress" && ev["status"] == "enriched" {
            progress += 1;
            if progress >= 100 {
                break;
            }
        }
    }
    assert_eq!(progress, 100, "expected 100 enrichment_progress events");

    // Belt-and-braces: the DB view must also reflect the terminal
    // status. Rules out a scenario where the SSE frame fires but the
    // row-level write somehow regresses.
    let list: serde_json::Value = h.get_json("/api/catalogue?limit=500").await;
    let entries = list["entries"].as_array().expect("entries is array");
    assert_eq!(entries.len(), 100, "expected 100 rows in list response");
    for entry in entries {
        assert_eq!(
            entry["enrichment_status"], "enriched",
            "every row must be enriched; offender = {entry}"
        );
    }
}
