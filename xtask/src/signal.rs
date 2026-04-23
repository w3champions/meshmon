//! Process-wide SIGINT/SIGTERM handler that fans out to a registered
//! teardown closure.
//!
//! The `ctrlc` crate panics if `set_handler` is called twice, so xtask
//! installs exactly one handler per process via [`install_once`] and
//! every owned-resource guard registers its teardown via
//! [`on_signal`]. Registration is FIFO; teardown closures run in the
//! order they were registered.

use std::sync::{Mutex, OnceLock};

type Teardown = Box<dyn Fn() + Send + Sync + 'static>;

static HANDLERS: OnceLock<Mutex<Vec<Teardown>>> = OnceLock::new();

fn handlers() -> &'static Mutex<Vec<Teardown>> {
    HANDLERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Install the process-wide signal handler exactly once. Idempotent —
/// later calls no-op (so every subcommand can call this on entry without
/// coordinating).
pub fn install_once() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let _ = ctrlc::set_handler(|| {
            let guard = handlers().lock().unwrap_or_else(|p| p.into_inner());
            for handler in guard.iter() {
                handler();
            }
            // Match the conventional 130 = 128 + SIGINT exit code so
            // wrapping shells (CI, lefthook) see the right cause.
            std::process::exit(130);
        });
    });
}

/// Register a teardown closure invoked on SIGINT/SIGTERM. Returns
/// nothing — there is no deregistration: the process is exiting.
// Called by resource-owning modules (test_db, etc.) that are wired in later.
#[allow(dead_code)]
pub fn on_signal<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    install_once();
    handlers()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Box::new(f));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn install_once_is_idempotent() {
        install_once();
        install_once();
    }

    #[test]
    fn on_signal_registers_handlers() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = counter.clone();
        on_signal(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });
        let n = handlers().lock().unwrap().len();
        assert!(n >= 1, "at least one handler registered, got {n}");
    }
}
