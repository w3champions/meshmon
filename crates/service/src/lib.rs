//! Library surface of the meshmon service.
//!
//! Modules added in T04 (service core — axum shell, config, logging):
//! - [`config`] — `meshmon.toml` parsing + validation.
//! - [`db`] — Postgres pool + migrations (shipped in T03).
//! - [`error`] — boot/startup error types.
//! - [`http`] — axum router assembly, health endpoints, OpenAPI serving.
//! - [`logging`] — tracing-subscriber JSON initializer.
//! - [`shutdown`] — cancellation token + OS signal handlers.
//! - [`state`] — shared `AppState` handle.

#![deny(rust_2018_idioms, unused_must_use)]
#![warn(missing_docs)]

pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod ingestion;
pub mod logging;
pub mod shutdown;
pub mod state;
