//! Library surface of the meshmon service.
//!
//! Per-module responsibilities:
//! - [`campaign`] — measurement-campaign data model, scheduler, and operator API.
//! - [`catalogue`] — IP catalogue registry, paste parser, enrichment wiring.
//! - [`config`] — `meshmon.toml` parsing + validation.
//! - [`db`] — Postgres pool + migrations.
//! - [`enrichment`] — pluggable provider chain for catalogue enrichment.
//! - [`error`] — boot/startup error types.
//! - [`grpc`] — tonic gRPC service assembly.
//! - [`hostname`] — IP→hostname reverse-DNS cache + session-scoped SSE.
//! - [`http`] — axum router assembly, health endpoints, OpenAPI serving.
//! - [`logging`] — tracing-subscriber JSON initializer.
//! - [`metrics`] — central metric registry (names + typed accessors + describe).
//! - [`probing`] — probing configuration types + spec-02 defaults.
//! - [`registry`] — in-memory agent registry snapshot.
//! - [`shutdown`] — cancellation token + OS signal handlers.
//! - [`state`] — shared `AppState` handle.
//! - [`vm_query`] — server-side PromQL read client for VictoriaMetrics,
//!   used by the campaign evaluator to pull agent-mesh baselines.

#![deny(rust_2018_idioms, unused_must_use)]
#![warn(missing_docs)]

pub mod campaign;
pub mod catalogue;
pub mod commands;
pub mod config;
pub mod db;
pub mod enrichment;
pub mod error;
pub mod grpc;
pub mod hostname;
pub mod http;
pub mod ingestion;
pub mod logging;
pub mod metrics;
pub mod probing;
pub mod registry;
pub mod shutdown;
pub mod state;
pub mod tls;
pub mod vm_query;
