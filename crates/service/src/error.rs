//! Shared error types for the meshmon service.
//!
//! Config-load and startup errors funnel through [`BootError`]; that's what
//! `main()` returns. Request-handler errors go through [`ApiError`] once
//! handlers land (T05 introduces the first real use).

use thiserror::Error;

/// Errors produced while loading config or starting the service.
///
/// `main()` converts this into a non-zero exit with a human-readable message.
/// Message text is load-bearing — CI checks (and future deployment tooling)
/// grep for the "meshmon:" prefix and the inner description.
#[derive(Debug, Error)]
pub enum BootError {
    /// Config file could not be read from disk (missing, permission denied).
    #[error("read config file {path}: {source}")]
    ConfigRead {
        /// Filesystem path we attempted to read.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Config file contents did not parse as TOML.
    #[error("parse config file {path}: {source}")]
    ConfigParse {
        /// Filesystem path of the offending config file.
        path: String,
        /// Underlying TOML deserialization error.
        #[source]
        source: toml::de::Error,
    },
    /// Config parsed but failed a validation rule.
    #[error("invalid config in {path}: {reason}")]
    ConfigInvalid {
        /// Filesystem path of the offending config file.
        path: String,
        /// Human-readable explanation of which rule was violated.
        reason: String,
    },
    /// Required env var is missing.
    #[error("env var {name} required by config key {key} is not set")]
    EnvMissing {
        /// Name of the environment variable that was not set.
        name: String,
        /// Config key (dotted path) that referenced the env var.
        key: String,
    },
    /// Postgres pool could not be created or migrations failed.
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    /// Migration failed.
    #[error("run migrations: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// HTTP listener could not be bound.
    #[error("bind HTTP listener on {addr}: {source}")]
    Bind {
        /// Socket address we attempted to bind.
        addr: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// HTTP server task failed while running.
    #[error("HTTP server: {0}")]
    Serve(#[from] std::io::Error),
}
