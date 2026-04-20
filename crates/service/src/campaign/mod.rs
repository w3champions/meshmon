//! Measurement-campaign subsystem.
//!
//! Owns the campaign data model, the 24 h reuse lookup, the fair
//! round-robin scheduler (single tokio task), and the operator HTTP
//! surface. The dispatch transport itself (RPC to agents) lives in
//! `crates/agent` + `crates/service/src/campaign/dispatch.rs`'s real
//! implementation, to be shipped by T45.

pub mod dispatch;
pub mod dto;
pub mod events;
pub mod handlers;
pub mod model;
pub mod repo;
pub mod scheduler;
pub mod writer;
