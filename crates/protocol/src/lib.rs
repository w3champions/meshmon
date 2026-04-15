//! Shared Protobuf message types for meshmon.
//!
//! Every Agent↔Service wire message is defined in
//! `proto/meshmon.proto` and compiled by `build.rs` via `prost-build`. The
//! generated module lives inside `OUT_DIR`; this crate re-exports the
//! individual types at the top level so callers write
//! `meshmon_protocol::MetricsBatch` without reaching into the `pb` submodule.
//!
//! IP-address helpers are in the [`ip`] submodule.

#![deny(rust_2018_idioms, unused_must_use)]
#![warn(missing_docs)]

/// Raw generated Protobuf code. Prefer the flat re-exports on this crate's
/// root; the `pb` path is only here so users who need the full generated
/// module (e.g. for debug printing or reflection) can reach it.
pub mod pb {
    #![allow(missing_docs)] // generated code
    include!(concat!(env!("OUT_DIR"), "/meshmon.rs"));
}

pub use pb::{
    AgentMetadata, ConfigResponse, DiffDetection, GetConfigRequest, GetTargetsRequest, HopIp,
    HopSummary, MetricsBatch, PathHealth, PathHealthThresholds, PathMetrics, PathSummary, Protocol,
    ProtocolHealth, ProtocolThresholds, PushMetricsResponse, PushRouteSnapshotResponse, RateEntry,
    RegisterRequest, RegisterResponse, RouteSnapshotRequest, Target, TargetsResponse, Windows,
};

/// Generated tonic server trait + server adapter. Implement [`AgentApi`],
/// then wrap in [`AgentApiServer::new`] / `with_interceptor`.
pub use pb::agent_api_server::{AgentApi, AgentApiServer};

/// Generated tonic client. Used by integration tests and, eventually, by
/// `meshmon-agent` (T11).
pub use pb::agent_api_client::AgentApiClient;

pub mod ip;
