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
use std::{net::IpAddr, sync::Arc, time::Duration};

/// Deterministic provider double that returns a fixed set of fields.
struct FakeProvider {
    id: &'static str,
    fields: Vec<(Field, FieldValue)>,
}

#[async_trait]
impl EnrichmentProvider for FakeProvider {
    fn id(&self) -> &'static str {
        self.id
    }
    fn supported(&self) -> &'static [Field] {
        &[]
    }
    async fn lookup(&self, _ip: IpAddr) -> Result<EnrichmentResult, EnrichmentError> {
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
    let chain: Vec<Arc<dyn EnrichmentProvider>> = vec![Arc::new(FakeProvider {
        id: "ipgeo-fake",
        fields: vec![
            (Field::City, FieldValue::Text("Sofia".into())),
            (Field::Latitude, FieldValue::F64(42.7)),
        ],
    })];
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
    let chain: Vec<Arc<dyn EnrichmentProvider>> = vec![Arc::new(FakeProvider {
        id: "ipgeo-fake",
        fields: vec![(Field::City, FieldValue::Text("Detroit".into()))],
    })];
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
