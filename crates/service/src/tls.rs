//! Rustls `ServerConfig` loader for standalone TLS mode.
//!
//! Called once at startup when `[agent_api.tls]` is configured.  The returned
//! `Arc<ServerConfig>` is handed to `tokio_rustls::TlsAcceptor` inside the
//! per-connection accept loop in `main.rs`.

use crate::error::BootError;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;

/// Load a rustls `ServerConfig` from disk.  ALPN advertises `h2` + `http/1.1`
/// so that both gRPC (HTTP/2) and REST (HTTP/1.1) clients negotiate correctly
/// when dialing the same port directly (no TLS-terminating proxy).
pub fn load_rustls_config(
    cert_path: &Path,
    key_path: &Path,
) -> Result<Arc<ServerConfig>, BootError> {
    let cert_pem = std::fs::read(cert_path).map_err(|e| BootError::Tls {
        reason: format!("read cert {}: {e}", cert_path.display()),
    })?;
    let key_pem = std::fs::read(key_path).map_err(|e| BootError::Tls {
        reason: format!("read key {}: {e}", key_path.display()),
    })?;

    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_slice())
        .collect::<Result<_, _>>()
        .map_err(|e| BootError::Tls {
            reason: format!("parse certs: {e}"),
        })?;
    if certs.is_empty() {
        return Err(BootError::Tls {
            reason: format!("no PEM certificates in {}", cert_path.display()),
        });
    }

    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut key_pem.as_slice())
        .map_err(|e| BootError::Tls {
            reason: format!("parse key: {e}"),
        })?
        .ok_or_else(|| BootError::Tls {
            reason: format!("no PEM private key in {}", key_path.display()),
        })?;

    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| BootError::Tls {
            reason: format!("build ServerConfig: {e}"),
        })?;
    // Advertise both protocols so gRPC (HTTP/2) and REST (HTTP/1.1) clients
    // can negotiate the right version via ALPN.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
