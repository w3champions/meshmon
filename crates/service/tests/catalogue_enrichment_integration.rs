//! Integration tests for [`meshmon_service::enrichment::runner`].
//!
//! Covers the end-to-end enrichment loop: enqueue a row, let the runner
//! walk a fake provider chain, and assert the DB row reflects the merge
//! outcome. Uses `common::acquire(false)` for isolation since enrichment
//! writes touch the shared `ip_catalogue.ip` unique constraint.

#[path = "common/mod.rs"]
mod common;

use async_trait::async_trait;
use meshmon_service::{
    catalogue::{
        events::CatalogueBroker,
        model::{CatalogueSource, EnrichmentStatus, Field},
        repo,
    },
    enrichment::{
        runner::{EnrichmentQueue, Runner},
        EnrichmentError, EnrichmentProvider, EnrichmentResult, FieldValue,
    },
};
use std::{
    net::IpAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

/// Deterministic provider double that returns a fixed set of fields.
///
/// `supported` advertises the exact set the runner can rely on to
/// short-circuit. `call_count` records every `lookup()` so tests can
/// assert the runner actually skipped a provider.
struct FakeProvider {
    id: &'static str,
    supported: &'static [Field],
    fields: Vec<(Field, FieldValue)>,
    call_count: Arc<AtomicUsize>,
}

impl FakeProvider {
    fn new(
        id: &'static str,
        supported: &'static [Field],
        fields: Vec<(Field, FieldValue)>,
    ) -> Self {
        Self {
            id,
            supported,
            fields,
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl EnrichmentProvider for FakeProvider {
    fn id(&self) -> &'static str {
        self.id
    }
    fn supported(&self) -> &'static [Field] {
        self.supported
    }
    async fn lookup(&self, _ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let mut r = EnrichmentResult::default();
        for (f, v) in &self.fields {
            r.fields.insert(*f, v.clone());
        }
        Ok(r)
    }
}

#[tokio::test]
async fn runner_enriches_a_pending_row_and_broadcasts() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["1.2.3.4".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;

    let broker = CatalogueBroker::new(16);
    let mut ev_rx = broker.subscribe();
    let (queue, rx) = EnrichmentQueue::new(1024);
    let chain: Vec<Arc<dyn EnrichmentProvider>> = vec![Arc::new(FakeProvider::new(
        "ipgeo-fake",
        &[Field::City, Field::Latitude],
        vec![
            (Field::City, FieldValue::Text("Sofia".into())),
            (Field::Latitude, FieldValue::F64(42.7)),
        ],
    ))];
    let runner = Runner::new(
        db.pool.clone(),
        chain,
        broker.clone(),
        rx,
        Duration::from_millis(50),
    );
    let handle = tokio::spawn(runner.run());

    let _ = queue.enqueue(id);

    let ev = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("broker receive timed out")
        .expect("broker recv failed");
    match ev {
        meshmon_service::catalogue::events::CatalogueEvent::EnrichmentProgress {
            id: got,
            status,
        } => {
            assert_eq!(got, id, "event id must match");
            assert_eq!(status, EnrichmentStatus::Enriched);
        }
        other => panic!("unexpected event variant: {other:?}"),
    }

    let entry = repo::find_by_id(&db.pool, id).await.unwrap().unwrap();
    assert_eq!(entry.enrichment_status, EnrichmentStatus::Enriched);
    assert_eq!(entry.city.as_deref(), Some("Sofia"));
    assert_eq!(entry.latitude, Some(42.7));

    handle.abort();
    db.close().await;
}

#[tokio::test]
async fn runner_skips_operator_edited_fields() {
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["5.6.7.8".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;

    repo::patch(
        &db.pool,
        id,
        repo::PatchSet {
            city: Some(Some("Tokyo".into())),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let broker = CatalogueBroker::new(16);
    let (queue, rx) = EnrichmentQueue::new(1024);
    let chain: Vec<Arc<dyn EnrichmentProvider>> = vec![Arc::new(FakeProvider::new(
        "ipgeo-fake",
        &[Field::City],
        vec![(Field::City, FieldValue::Text("Detroit".into()))],
    ))];
    let handle = tokio::spawn(
        Runner::new(
            db.pool.clone(),
            chain,
            broker,
            rx,
            Duration::from_millis(50),
        )
        .run(),
    );

    let _ = queue.enqueue(id);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let entry = repo::find_by_id(&db.pool, id).await.unwrap().unwrap();
    assert_eq!(
        entry.city.as_deref(),
        Some("Tokyo"),
        "operator-edited City must survive enrichment"
    );

    handle.abort();
    db.close().await;
}

#[tokio::test]
async fn runner_skips_provider_when_all_supported_fields_are_settled() {
    // Two providers advertise overlapping `supported` sets. The first
    // fills every field in the second's supported list, so the runner
    // must skip the second's `lookup()` call entirely — otherwise the
    // chain burns quota on a provider that can never produce a write.
    let db = common::acquire(false).await;
    meshmon_service::db::run_migrations(&db.pool).await.unwrap();

    let ips: Vec<IpAddr> = vec!["9.9.9.9".parse().unwrap()];
    let ins = repo::insert_many(&db.pool, &ips, CatalogueSource::Operator, None)
        .await
        .unwrap();
    let id = ins.created[0].id;

    let primary = Arc::new(FakeProvider::new(
        "primary",
        &[Field::City, Field::CountryCode],
        vec![
            (Field::City, FieldValue::Text("Berlin".into())),
            (Field::CountryCode, FieldValue::Text("DE".into())),
        ],
    ));
    // Secondary advertises a subset of what primary already filled.
    // needs_provider() should return false and skip lookup().
    let secondary = Arc::new(FakeProvider::new(
        "secondary",
        &[Field::City],
        vec![(Field::City, FieldValue::Text("Paris".into()))],
    ));
    let secondary_calls = secondary.call_count.clone();
    let primary_calls = primary.call_count.clone();

    let broker = CatalogueBroker::new(16);
    let mut ev_rx = broker.subscribe();
    let (queue, rx) = EnrichmentQueue::new(1024);
    let chain: Vec<Arc<dyn EnrichmentProvider>> =
        vec![primary as Arc<dyn EnrichmentProvider>, secondary as _];
    let handle = tokio::spawn(
        Runner::new(
            db.pool.clone(),
            chain,
            broker,
            rx,
            Duration::from_millis(50),
        )
        .run(),
    );

    let _ = queue.enqueue(id);

    let _ = tokio::time::timeout(Duration::from_secs(2), ev_rx.recv())
        .await
        .expect("broker receive timed out")
        .expect("broker recv failed");

    assert_eq!(
        primary_calls.load(Ordering::SeqCst),
        1,
        "primary provider must be called"
    );
    assert_eq!(
        secondary_calls.load(Ordering::SeqCst),
        0,
        "secondary provider must be skipped — primary filled every supported field"
    );

    let entry = repo::find_by_id(&db.pool, id).await.unwrap().unwrap();
    assert_eq!(entry.city.as_deref(), Some("Berlin"));
    assert_eq!(entry.country_code.as_deref(), Some("DE"));

    handle.abort();
    db.close().await;
}
