//! IP catalogue subsystem: per-host registry with enrichment, SSE
//! notifications, and an operator HTTP surface. Rows cross-reference
//! `agents` through the `agents_with_catalogue` view so geo / ASN data
//! lives in one authoritative table.

pub mod dto;
pub mod events;
pub mod facets;
pub mod handlers;
pub mod model;
pub mod parse;
pub mod repo;
pub mod sort;
pub(crate) mod sse;
