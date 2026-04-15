//! Service ingestion pipeline: agent payloads → VM/Postgres.
//!
//! See spec 03 §"Ingestion pipeline" and spec 04 §"Schema". This module is
//! the data plane only; HTTP handlers (T06) call into [`IngestionPipeline`]
//! after token + Protobuf decoding.

// Submodules added incrementally in subsequent steps.
pub mod json_shapes;
pub mod last_seen;
pub mod metrics;
pub mod validator;
