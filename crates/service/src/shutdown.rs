//! Shutdown coordination.
//!
//! A single [`tokio_util::sync::CancellationToken`] is shared across:
//! - the axum server via `with_graceful_shutdown(token.cancelled())`;
//! - future worker tasks (ingestion — T07, registry refresh — T08);
//! - health endpoints (`/readyz` flips to 503 once the token is cancelled).
//!
//! On Unix: SIGTERM and SIGINT cancel the token. SIGHUP triggers config
//! reload via the `on_reload` callback and does **not** cancel.
//!
//! On non-Unix (Windows): only ctrl-c cancels; SIGHUP is unavailable and
//! config reload is a no-op. (Windows is not a deployment target for the
//! service, but CI runs tests there, so the stub lets the crate compile.)

use std::future::Future;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Spawn the signal-handling task. Returns immediately with the token that
/// subsystems wire into their shutdown paths.
///
/// `on_reload` is invoked on each SIGHUP. It's boxed and `Send + 'static`
/// because the signal loop runs on a tokio task.
pub fn spawn<F, Fut>(on_reload: F) -> CancellationToken
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let token = CancellationToken::new();
    let child = token.clone();
    tokio::spawn(async move {
        run(child, on_reload).await;
    });
    token
}

#[cfg(unix)]
async fn run<F, Fut>(token: CancellationToken, on_reload: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    use tokio::signal::unix::{signal, SignalKind};

    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to install SIGTERM handler; falling back to ctrl-c only");
            let _ = tokio::signal::ctrl_c().await;
            token.cancel();
            return;
        }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to install SIGINT handler");
            let _ = tokio::signal::ctrl_c().await;
            token.cancel();
            return;
        }
    };
    let mut sighup = match signal(SignalKind::hangup()) {
        Ok(s) => Some(s),
        Err(e) => {
            error!(error = %e, "failed to install SIGHUP handler; config reload disabled");
            None
        }
    };

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("received SIGTERM; shutting down");
                token.cancel();
                return;
            }
            _ = sigint.recv() => {
                info!("received SIGINT; shutting down");
                token.cancel();
                return;
            }
            Some(_) = async {
                match sighup.as_mut() {
                    Some(s) => s.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                info!("received SIGHUP; reloading config");
                on_reload().await;
            }
        }
    }
}

#[cfg(not(unix))]
async fn run<F, Fut>(token: CancellationToken, _on_reload: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let _ = tokio::signal::ctrl_c().await;
    info!("received ctrl-c; shutting down");
    token.cancel();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn on_reload_callback_is_called_on_sighup() {
        let called = Arc::new(AtomicBool::new(false));
        let c = called.clone();
        let token = spawn(move || {
            let c = c.clone();
            async move {
                c.store(true, Ordering::SeqCst);
            }
        });

        // Give the signal handler a moment to install before we raise.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let pid = std::process::id().to_string();
        let status = std::process::Command::new("kill")
            .args(["-HUP", &pid])
            .status()
            .expect("spawn kill");
        assert!(status.success());

        // Poll for up to 1s; the callback runs on a spawned task so the
        // write may lag slightly.
        for _ in 0..20 {
            if called.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(called.load(Ordering::SeqCst), "on_reload was not called");
        assert!(!token.is_cancelled(), "SIGHUP must not cancel the token");
    }
}
