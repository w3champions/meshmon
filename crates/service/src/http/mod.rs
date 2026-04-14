//! Axum router assembly.
//!
//! Health endpoints (`/healthz`, `/readyz`, `/metrics`) live in
//! [`health`]. The `/api/*` router and assembled [`router`] are added in
//! the next task.

pub mod health;
