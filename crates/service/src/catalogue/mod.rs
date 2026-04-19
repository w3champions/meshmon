//! IP catalogue subsystem: per-host registry with enrichment, SSE
//! notifications, and an operator HTTP surface. Rows cross-reference
//! `agents` through the `agents_with_catalogue` view so geo / ASN data
//! lives in one authoritative table.

pub mod events;
pub mod model;
pub mod parse;
pub mod repo;
pub(crate) mod sse;
