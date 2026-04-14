//! Tracing subscriber initialization.
//!
//! One subscriber is installed for the lifetime of the process. Format
//! (JSON vs compact) and filter (`info`, `debug,sqlx=warn`, etc.) come
//! from [`crate::config::LoggingSection`]. Downstream subsystems use the
//! `tracing` macros directly — no per-module configuration.
//!
//! The `RUST_LOG` environment variable overrides the configured filter when
//! set. This lets operators crank verbosity without editing `meshmon.toml`.

use crate::config::{LogFormat, LoggingSection};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Install a global tracing subscriber. Idempotency: only the **first** call
/// per process succeeds; later calls are silently ignored (matches the
/// `tracing-subscriber` default).
///
/// Errors from the subscriber installation are swallowed because the main
/// use-case for multiple calls is tests (via a `ctor`-style helper), not
/// production code.
pub fn init(cfg: &LoggingSection) {
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(cfg.filter.as_str()));
    let registry = tracing_subscriber::registry().with(env_filter);

    match cfg.format {
        LogFormat::Json => {
            let _ = registry
                .with(fmt::layer().json().flatten_event(true))
                .try_init();
        }
        LogFormat::Compact => {
            let _ = registry.with(fmt::layer().compact()).try_init();
        }
    }
}
