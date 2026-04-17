//! Unified error type for the reverse-tunnel layer.

/// Errors that can occur in the reverse-tunnel layer.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// The outer `OpenTunnel` bidi stream closed unexpectedly.
    #[error("outer stream disconnected")]
    Disconnected,

    /// Service-side: the remote peer's bearer + source-id check failed.
    #[error("authentication rejected: {0}")]
    AuthFailed(tonic::Status),

    /// yamux session-level error (frame decode, keepalive, etc.).
    #[error("yamux error: {0}")]
    Yamux(#[from] yamux::ConnectionError),

    /// Low-level IO error while driving the byte adapter.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Error from the tonic transport layer (e.g. Endpoint connection).
    #[error("tonic transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    /// gRPC-level status propagated from a remote call.
    #[error("tonic status: {0}")]
    Status(#[from] tonic::Status),
}
