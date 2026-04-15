//! Debounced `agents.last_seen_at` (and `agent_version`) updater.
//!
//! Spec 03 mandates "at most one update per agent per 30s". We collapse
//! per-batch touches into one DB write per debounce window via skip-if-
//! recent state in an in-memory `HashMap<String, Instant>`.

use crate::ingestion::metrics::{ingest_dropped, last_seen_writes, DropSource};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Handle to a spawned last-seen updater task.
///
/// Clone-cheap: all state is behind `Arc` / channels.
#[derive(Clone)]
pub struct LastSeenUpdater {
    tx: mpsc::Sender<Touch>,
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
}

#[derive(Debug)]
struct Touch {
    agent_id: String,
    version: Option<String>,
}

impl LastSeenUpdater {
    /// Spawn the updater task and return its handle. The task runs until
    /// `token` is cancelled, at which point it best-effort drains any
    /// already-queued touches via `try_recv` before returning.
    pub fn spawn(pool: PgPool, debounce: Duration, token: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel::<Touch>(1024);
        let join = tokio::spawn(run(rx, pool, debounce, token));
        Self {
            tx,
            join: Arc::new(Mutex::new(Some(join))),
        }
    }

    /// Enqueue a touch for `agent_id`, optionally updating `agent_version`.
    /// Non-blocking: backed by a bounded mpsc via `try_send`. If the channel
    /// is full the touch is dropped and the
    /// `meshmon_service_ingest_dropped_total{source="touch"}` counter is
    /// incremented.
    pub fn touch(&self, agent_id: &str, version: Option<String>) {
        let touch = Touch {
            agent_id: agent_id.to_string(),
            version,
        };
        if let Err(e) = self.tx.try_send(touch) {
            ingest_dropped(DropSource::Touch).increment(1);
            debug!(error = %e, agent = %agent_id, "last_seen touch dropped");
        }
    }

    /// Wait for the updater task to exit. Safe to call multiple times.
    pub async fn join(&self) {
        let mut g = self.join.lock().await;
        if let Some(h) = g.take() {
            let _ = h.await;
        }
    }
}

async fn run(
    mut rx: mpsc::Receiver<Touch>,
    pool: PgPool,
    debounce: Duration,
    token: CancellationToken,
) {
    let mut last_write: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => {
                while let Ok(touch) = rx.try_recv() {
                    apply(&pool, &mut last_write, debounce, touch).await;
                }
                break;
            }
            maybe = rx.recv() => match maybe {
                Some(touch) => apply(&pool, &mut last_write, debounce, touch).await,
                None => break,
            },
        }
    }
}

async fn apply(
    pool: &PgPool,
    last_write: &mut HashMap<String, Instant>,
    debounce: Duration,
    touch: Touch,
) {
    let now = Instant::now();
    if let Some(prev) = last_write.get(&touch.agent_id) {
        if now.duration_since(*prev) < debounce {
            return;
        }
    }
    let result = match touch.version.as_deref() {
        Some(version) => {
            sqlx::query!(
                "UPDATE agents SET last_seen_at = NOW(), agent_version = $2 WHERE id = $1",
                touch.agent_id,
                version,
            )
            .execute(pool)
            .await
        }
        None => {
            sqlx::query!(
                "UPDATE agents SET last_seen_at = NOW() WHERE id = $1",
                touch.agent_id,
            )
            .execute(pool)
            .await
        }
    };
    match result {
        Ok(res) if res.rows_affected() > 0 => {
            last_seen_writes().increment(1);
            last_write.insert(touch.agent_id, now);
        }
        Ok(_) => debug!(agent = %touch.agent_id, "last_seen touch hit no row"),
        Err(e) => warn!(error = %e, agent = %touch.agent_id, "last_seen update failed"),
    }
}
