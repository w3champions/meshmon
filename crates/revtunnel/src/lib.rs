//! Reverse-tunnel transport for tonic.
//!
//! Agents behind NAT open one `rpc OpenTunnel(stream TunnelFrame) returns
//! (stream TunnelFrame);` against the service. Inside that stream,
//! `rust-yamux` multiplexes virtual substreams; each side treats every
//! substream as an `AsyncRead + AsyncWrite` and runs tonic on top of it
//! — a normal server on the agent, a normal client channel on the
//! service. The result is that service code invokes methods on agents as
//! native gRPC RPCs with native deadlines, status codes, and cancellation.
//!
//! The two public entry points:
//!
//! - [`TunnelClient::open_and_run`] — agent side. Opens the outer stream,
//!   runs yamux in server mode, hosts a tonic server over it.
//! - [`TunnelManager`] — service side. Accepts `OpenTunnel` RPCs, runs
//!   yamux in client mode per agent, exposes a `source_id → Channel`
//!   registry the caller fans out native RPCs across.

#![deny(rust_2018_idioms, unused_must_use)]
#![warn(missing_docs)]

pub mod agent;
pub mod byte_adapter;
pub mod error;
pub mod service;

pub use agent::TunnelClient;
pub use byte_adapter::TunnelIo;
pub use error::TunnelError;
pub use meshmon_protocol::TunnelFrame;
pub use service::TunnelManager;
