//! Library surface of the meshmon service.
//!
//! Modules added in T04 (service core — axum shell, config, logging):
//! - [`config`] — `meshmon.toml` parsing + validation.
//! - [`db`] — Postgres pool + migrations (shipped in T03).
//! - [`error`] — boot/startup error types.
//! - [`grpc`] — tonic gRPC service assembly (AgentApi stub, added T06).
//! - [`http`] — axum router assembly, health endpoints, OpenAPI serving.
//! - [`logging`] — tracing-subscriber JSON initializer.
//! - [`metrics`] — central metric registry (names + typed accessors + describe).
//! - [`probing`] — probing configuration types + spec-02 defaults.
//! - [`registry`] — in-memory agent registry snapshot.
//! - [`shutdown`] — cancellation token + OS signal handlers.
//! - [`state`] — shared `AppState` handle.

#![deny(rust_2018_idioms, unused_must_use)]
#![warn(missing_docs)]

pub mod commands;
pub mod config;
pub mod db;
pub mod error;
pub mod grpc;
pub mod http;
pub mod ingestion;
pub mod logging;
pub mod metrics;
pub mod probing;
pub mod registry;
pub mod shutdown;
pub mod state;
pub mod tls;
