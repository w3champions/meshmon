//! Integration tests for `/api/campaigns/stream`.
//!
//! Exercised wiring:
//!
//! 1. [`meshmon_service::campaign::listener::spawn_campaign_listener`]
//!    subscribes to the two campaign NOTIFY channels and publishes onto
//!    the [`CampaignBroker`] on [`AppState`].
//! 2. [`meshmon_service::campaign::sse::campaign_stream`] drains that
//!    broker into an SSE response over a real TCP socket (`oneshot`
//!    cannot stream).
//!
//! Per-test isolation: each test binds a fresh Postgres database via
//! [`common::acquire(false)`] so NOTIFY fan-out between concurrent test
//! binaries can't cross-pollinate. Campaign UUIDs are globally unique by
//! construction, but we isolate the database anyway to keep the SSE
//! stream contents scoped to the test.

mod common;

use common::SseStream;
use futures::StreamExt;
use meshmon_service::campaign::events::{EVALUATED_CHANNEL, PAIR_SETTLED_CHANNEL};
use meshmon_service::campaign::listener::spawn_campaign_listener;
use meshmon_service::state::AppState;
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Per-test harness with a real TCP listener and a running campaign SSE
/// listener task. Deliberately not folded into `common::HttpHarness` —
/// every other integration test either skips the listener or stands up
/// its own scheduler; this is the one test binary that needs the
/// listener task alone.
struct CampaignSseHarness {
    addr: SocketAddr,
    client: reqwest::Client,
    cookie: String,
    pool: PgPool,
    shutdown: CancellationToken,
    server_task: JoinHandle<()>,
    listener_task: JoinHandle<()>,
    db: Option<common::TestDb>,
}

impl CampaignSseHarness {
    async fn start() -> Self {
        use arc_swap::ArcSwap;
        use meshmon_service::config::Config;
        use tokio::sync::watch;

        let db = common::acquire(false).await;
        meshmon_service::db::run_migrations(&db.pool)
            .await
            .expect("run migrations for campaigns SSE harness");

        let toml = format!(
            r#"
[database]
url = "postgres://ignored@h/d"

[service]
trust_forwarded_headers = true

[[auth.users]]
username = "admin"
password_hash = "{AUTH_TEST_HASH}"

[probing]
udp_probe_secret = "{TEST_UDP_PROBE_SECRET_TOML}"
"#,
            AUTH_TEST_HASH = common::AUTH_TEST_HASH,
            TEST_UDP_PROBE_SECRET_TOML = common::TEST_UDP_PROBE_SECRET_TOML,
        );
        let cfg = Arc::new(Config::from_str(&toml, "synthetic.toml").expect("parse"));
        let swap = Arc::new(ArcSwap::from(cfg.clone()));
        let (_cfg_tx, cfg_rx) = watch::channel(cfg);
        let ingestion = common::dummy_ingestion(db.pool.clone());
        let registry = common::dummy_registry(db.pool.clone());
        let (hb, hl, hr) = common::test_hostname_fixtures(&db.pool);
        let state = AppState::new(
            swap,
            cfg_rx,
            db.pool.clone(),
            ingestion,
            registry,
            common::test_prometheus_handle().await,
            common::test_enrichment_queue(),
            hb,
            hl,
            hr,
        );
        state.mark_ready();

        // Spawn the campaign SSE listener tied to this state's broker.
        // This is the unit under test; the listener translates NOTIFY
        // wake-ups into broker publishes that the SSE handler forwards.
        let listener_cancel = CancellationToken::new();
        let listener_task = spawn_campaign_listener(
            db.pool.clone(),
            state.campaign_broker.clone(),
            listener_cancel.clone(),
        );

        let app = meshmon_service::http::router(state.clone());
        let cookie = common::login_as_admin(&app, "203.0.113.70").await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind campaigns SSE test listener");
        let addr = listener.local_addr().expect("resolve local addr");

        let shutdown = listener_cancel;
        let server_shutdown = shutdown.clone();
        let server_app = app;
        let server_task = tokio::spawn(async move {
            let _ = axum::serve(listener, server_app)
                .with_graceful_shutdown(async move { server_shutdown.cancelled().await })
                .await;
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("build reqwest client");

        Self {
            addr,
            client,
            cookie,
            pool: db.pool.clone(),
            shutdown,
            server_task,
            listener_task,
            db: Some(db),
        }
    }

    /// Open a long-lived SSE connection to `path` and return a stream of
    /// parsed JSON payloads. Delegates to [`common::subscribe_sse`] so
    /// the connect-and-wrap logic stays shared with `HttpHarness::sse`.
    async fn sse(&self, path: &str) -> SseStream {
        let base_url = format!("http://{}", self.addr);
        common::subscribe_sse(&self.client, &base_url, path, &self.cookie).await
    }

    async fn post_json(&self, path: &str, body: &serde_json::Value) -> serde_json::Value {
        let url = format!("http://{}{path}", self.addr);
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::COOKIE, &self.cookie)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap_or_else(|e| panic!("POST {url} dispatch: {e}"));
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .unwrap_or_else(|e| panic!("POST {url} body read: {e}"));
        assert!(
            status.as_u16() == 200,
            "POST {path} expected 200, got {status}; body = {:?}",
            String::from_utf8_lossy(&bytes),
        );
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|e| panic!("decode {path} body: {e}; raw = {bytes:?}"))
    }

    async fn post_json_empty(&self, path: &str) -> serde_json::Value {
        self.post_json(path, &serde_json::Value::Null).await
    }
}

impl Drop for CampaignSseHarness {
    fn drop(&mut self) {
        // Cancel first so the graceful-shutdown branch of `axum::serve`
        // and the listener's `select!` on `cancel` both observe it.
        self.shutdown.cancel();
        self.server_task.abort();
        self.listener_task.abort();
        let _ = self.db.take();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn state_changed_arrives_on_start() {
    let h = CampaignSseHarness::start().await;

    // Subscribe BEFORE triggering the state transition so no frame can
    // slip past between the POST return and the SSE subscription.
    let mut sse = h.sse("/api/campaigns/stream").await;

    let created = h
        .post_json(
            "/api/campaigns",
            &serde_json::json!({
                "title": "sse-state-changed",
                "protocol": "icmp",
                "source_agent_ids": ["agent-sse-1"],
                "destination_ips": ["198.51.100.10"],
            }),
        )
        .await;
    let id = created["id"].as_str().expect("id is string").to_string();
    assert_eq!(created["state"], "draft", "body = {created}");

    // Draft -> Running. The handler UPDATEs `state`, the migration's
    // trigger fires `NOTIFY campaign_state_changed`, the listener task
    // resolves the new state, and the SSE handler forwards it.
    let started = h
        .post_json_empty(&format!("/api/campaigns/{id}/start"))
        .await;
    assert_eq!(started["state"], "running", "body = {started}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_running = false;
    while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, sse.next()).await {
        let ev = frame.expect("sse frame parse");
        if ev["kind"] == "state_changed" && ev["campaign_id"] == id && ev["state"] == "running" {
            saw_running = true;
            break;
        }
    }
    assert!(
        saw_running,
        "expected state_changed frame for campaign {id} with state=running"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_settled_arrives_when_notify_fires() {
    let h = CampaignSseHarness::start().await;

    // Subscribe BEFORE we fire the NOTIFY so the broadcast-semantics
    // broker can't emit the event before the SSE client exists.
    let mut sse = h.sse("/api/campaigns/stream").await;

    // Pick a synthetic UUID. We don't need a real campaign row — the
    // listener's `PAIR_SETTLED_CHANNEL` branch forwards the UUID as-is
    // without validating it against `measurement_campaigns`. A row-less
    // test keeps the fixture minimal.
    let campaign_id = uuid::Uuid::new_v4();

    // Fire the NOTIFY directly so the listener branch is exercised in
    // isolation from the full dispatch writer path. `pg_notify(channel,
    // payload)` is semantically identical to the writer's internal
    // call; the listener doesn't distinguish origins.
    sqlx::query("SELECT pg_notify($1, $2::text)")
        .bind(PAIR_SETTLED_CHANNEL)
        .bind(campaign_id.to_string())
        .execute(&h.pool)
        .await
        .expect("fire pg_notify");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_settled = false;
    while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, sse.next()).await {
        let ev = frame.expect("sse frame parse");
        if ev["kind"] == "pair_settled" && ev["campaign_id"] == campaign_id.to_string() {
            saw_settled = true;
            break;
        }
    }
    assert!(
        saw_settled,
        "expected pair_settled frame for campaign {campaign_id}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn evaluated_arrives_when_notify_fires() {
    // Cross-instance fan-out for `/evaluate`. The
    // `campaign_evaluations_notify` trigger fires `NOTIFY
    // campaign_evaluated` in the same tx as the UPSERT; the listener
    // translates it to a `CampaignStreamEvent::Evaluated`, which the
    // SSE handler renders as `{"kind":"evaluated","campaign_id":"…"}`.
    // Firing NOTIFY directly isolates the listener branch from the
    // full `/evaluate` handler surface.
    let h = CampaignSseHarness::start().await;

    let mut sse = h.sse("/api/campaigns/stream").await;

    let campaign_id = uuid::Uuid::new_v4();

    sqlx::query("SELECT pg_notify($1, $2::text)")
        .bind(EVALUATED_CHANNEL)
        .bind(campaign_id.to_string())
        .execute(&h.pool)
        .await
        .expect("fire pg_notify");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_evaluated = false;
    while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, sse.next()).await {
        let ev = frame.expect("sse frame parse");
        if ev["kind"] == "evaluated" && ev["campaign_id"] == campaign_id.to_string() {
            saw_evaluated = true;
            break;
        }
    }
    assert!(
        saw_evaluated,
        "expected evaluated frame for campaign {campaign_id}"
    );
}
