//! TLS utilities for loading certificates and configuring mTLS.
//!
//! This module provides utilities for loading TLS certificates and creating
//! server/client TLS configurations for mutual TLS (mTLS) authentication.

use std::path::PathBuf;

use tokio::fs;
use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

use crate::config::TlsConfig;

/// Error type for TLS configuration issues.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("CA certificate path not configured")]
    MissingCaCert,

    #[error("Node certificate path not configured")]
    MissingCert,

    #[error("Private key path not configured")]
    MissingKey,

    #[error("CA certificate not found: {0}")]
    CaCertNotFound(PathBuf),

    #[error("Node certificate not found: {0}")]
    CertNotFound(PathBuf),

    #[error("Private key not found: {0}")]
    KeyNotFound(PathBuf),

    #[error("Failed to read file: {0}")]
    IoError(#[from] std::io::Error),
}

/// Loaded TLS materials ready for use with tonic.
///
/// Contains both the node's identity (certificate + private key) and the
/// CA certificate used to verify peer certificates.
#[derive(Clone)]
pub struct TlsIdentity {
    /// This node's identity (certificate + private key)
    identity: Identity,
    /// CA certificate for verifying peers
    ca_cert: Certificate,
}

impl TlsIdentity {
    /// Load TLS materials from file paths specified in the config.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Any required path is not configured
    /// - Any file does not exist or cannot be read
    pub async fn load(config: &TlsConfig) -> Result<Self, TlsError> {
        let ca_cert_path = config
            .ca_cert_path
            .as_ref()
            .ok_or(TlsError::MissingCaCert)?;
        let cert_path = config.cert_path.as_ref().ok_or(TlsError::MissingCert)?;
        let key_path = config.key_path.as_ref().ok_or(TlsError::MissingKey)?;

        // Validate paths exist before reading
        if !ca_cert_path.exists() {
            return Err(TlsError::CaCertNotFound(ca_cert_path.clone()));
        }
        if !cert_path.exists() {
            return Err(TlsError::CertNotFound(cert_path.clone()));
        }
        if !key_path.exists() {
            return Err(TlsError::KeyNotFound(key_path.clone()));
        }

        // Read certificate and key files
        let ca_pem = fs::read(ca_cert_path).await?;
        let cert_pem = fs::read(cert_path).await?;
        let key_pem = fs::read(key_path).await?;

        // Create tonic TLS types
        let ca_cert = Certificate::from_pem(ca_pem);
        let identity = Identity::from_pem(cert_pem, key_pem);

        Ok(Self { identity, ca_cert })
    }

    /// Create server TLS config with client certificate verification (mTLS).
    ///
    /// The returned config:
    /// - Presents this node's certificate to clients
    /// - Requires clients to present a valid certificate
    /// - Verifies client certificates against the CA
    pub fn server_tls_config(&self) -> ServerTlsConfig {
        ServerTlsConfig::new()
            .identity(self.identity.clone())
            .client_ca_root(self.ca_cert.clone())
    }

    /// Create client TLS config for connecting to peers.
    ///
    /// The returned config:
    /// - Presents this node's certificate to the server
    /// - Verifies the server's certificate against the CA
    /// - Uses a generic domain name for internal cluster communication
    ///   since nodes connect by IP address
    pub fn client_tls_config(&self) -> ClientTlsConfig {
        ClientTlsConfig::new()
            // Use a generic domain name since nodes connect by IP
            // Certificate validation is based on CA trust, not hostname
            .domain_name("nomad-lite-cluster")
            .ca_certificate(self.ca_cert.clone())
            .identity(self.identity.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_config_is_complete() {
        let mut config = TlsConfig::default();
        assert!(!config.is_complete());

        config.enabled = true;
        assert!(!config.is_complete());

        config.ca_cert_path = Some(PathBuf::from("/tmp/ca.crt"));
        assert!(!config.is_complete());

        config.cert_path = Some(PathBuf::from("/tmp/node.crt"));
        assert!(!config.is_complete());

        config.key_path = Some(PathBuf::from("/tmp/node.key"));
        assert!(config.is_complete());
    }

    #[tokio::test]
    async fn test_load_missing_paths() {
        let config = TlsConfig {
            enabled: true,
            ca_cert_path: None,
            cert_path: None,
            key_path: None,
            allow_insecure: false,
        };

        let result = TlsIdentity::load(&config).await;
        assert!(matches!(result, Err(TlsError::MissingCaCert)));
    }

    #[tokio::test]
    async fn test_load_nonexistent_files() {
        let config = TlsConfig {
            enabled: true,
            ca_cert_path: Some(PathBuf::from("/nonexistent/ca.crt")),
            cert_path: Some(PathBuf::from("/nonexistent/node.crt")),
            key_path: Some(PathBuf::from("/nonexistent/node.key")),
            allow_insecure: false,
        };

        let result = TlsIdentity::load(&config).await;
        assert!(matches!(result, Err(TlsError::CaCertNotFound(_))));
    }
}
