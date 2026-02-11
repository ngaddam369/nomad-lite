use std::net::SocketAddr;
use std::path::PathBuf;

/// Configuration for Docker-based job execution.
///
/// All jobs run in sandboxed Docker containers for security.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Docker image to use for job execution
    pub image: String,
    /// Disable network access in container
    pub network_disabled: bool,
    /// Memory limit (e.g., "256m")
    pub memory_limit: Option<String>,
    /// CPU limit (e.g., "0.5" for half a CPU)
    pub cpu_limit: Option<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            image: "alpine:latest".to_string(),
            network_disabled: true,
            memory_limit: Some("256m".to_string()),
            cpu_limit: Some("0.5".to_string()),
        }
    }
}

/// TLS configuration for secure node communication.
///
/// When enabled, all gRPC communication uses mutual TLS (mTLS):
/// - Servers present their certificate and verify client certificates
/// - Clients present their certificate and verify server certificates
/// - Both sides must have certificates signed by the cluster CA
#[derive(Debug, Clone, Default)]
pub struct TlsConfig {
    /// Enable TLS. If false, all other TLS settings are ignored.
    pub enabled: bool,

    /// Path to the CA certificate (PEM format).
    /// Used to verify peer certificates.
    pub ca_cert_path: Option<PathBuf>,

    /// Path to this node's certificate (PEM format).
    /// Presented to peers during TLS handshake.
    pub cert_path: Option<PathBuf>,

    /// Path to this node's private key (PEM format).
    /// Must match the certificate.
    pub key_path: Option<PathBuf>,

    /// Allow insecure connections for development/testing.
    /// When true and TLS files are missing, runs in plaintext mode with warning.
    /// When false and TLS files are missing, fails to start.
    pub allow_insecure: bool,
}

impl TlsConfig {
    /// Check if TLS is properly configured with all required files.
    pub fn is_complete(&self) -> bool {
        self.enabled
            && self.ca_cert_path.is_some()
            && self.cert_path.is_some()
            && self.key_path.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub node_id: u64,
    pub listen_addr: SocketAddr,
    pub peers: Vec<PeerConfig>,
    pub election_timeout_min_ms: u64,
    pub election_timeout_max_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub sandbox: SandboxConfig,
    pub tls: TlsConfig,
}

#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub node_id: u64,
    pub addr: String, // host:port format, supports both IP and hostnames
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            node_id: 1,
            // SAFETY: This is a hardcoded valid address that will always parse
            listen_addr: "127.0.0.1:50051"
                .parse()
                .expect("default listen address is valid"),
            peers: Vec::new(),
            election_timeout_min_ms: 150,
            election_timeout_max_ms: 300,
            heartbeat_interval_ms: 50,
            sandbox: SandboxConfig::default(),
            tls: TlsConfig::default(),
        }
    }
}

impl NodeConfig {
    pub fn new(node_id: u64, listen_addr: SocketAddr) -> Self {
        Self {
            node_id,
            listen_addr,
            ..Default::default()
        }
    }

    pub fn with_peer(mut self, node_id: u64, addr: String) -> Self {
        self.peers.push(PeerConfig { node_id, addr });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_config_default() {
        let cfg = SandboxConfig::default();
        assert_eq!(cfg.image, "alpine:latest");
        assert!(cfg.network_disabled);
        assert_eq!(cfg.memory_limit.as_deref(), Some("256m"));
        assert_eq!(cfg.cpu_limit.as_deref(), Some("0.5"));
    }

    #[test]
    fn tls_config_default() {
        let cfg = TlsConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.ca_cert_path.is_none());
        assert!(cfg.cert_path.is_none());
        assert!(cfg.key_path.is_none());
        assert!(!cfg.allow_insecure);
    }

    #[test]
    fn tls_config_is_complete_when_all_paths_set() {
        let cfg = TlsConfig {
            enabled: true,
            ca_cert_path: Some(PathBuf::from("/ca.pem")),
            cert_path: Some(PathBuf::from("/cert.pem")),
            key_path: Some(PathBuf::from("/key.pem")),
            allow_insecure: false,
        };
        assert!(cfg.is_complete());
    }

    #[test]
    fn tls_config_is_not_complete_when_disabled() {
        let cfg = TlsConfig {
            enabled: false,
            ca_cert_path: Some(PathBuf::from("/ca.pem")),
            cert_path: Some(PathBuf::from("/cert.pem")),
            key_path: Some(PathBuf::from("/key.pem")),
            allow_insecure: false,
        };
        assert!(!cfg.is_complete());
    }

    #[test]
    fn tls_config_is_not_complete_when_path_missing() {
        let base = TlsConfig {
            enabled: true,
            ca_cert_path: Some(PathBuf::from("/ca.pem")),
            cert_path: Some(PathBuf::from("/cert.pem")),
            key_path: Some(PathBuf::from("/key.pem")),
            allow_insecure: false,
        };

        let mut cfg = base.clone();
        cfg.ca_cert_path = None;
        assert!(!cfg.is_complete());

        let mut cfg = base.clone();
        cfg.cert_path = None;
        assert!(!cfg.is_complete());

        let mut cfg = base;
        cfg.key_path = None;
        assert!(!cfg.is_complete());
    }

    #[test]
    fn node_config_default() {
        let cfg = NodeConfig::default();
        assert_eq!(cfg.node_id, 1);
        assert_eq!(cfg.listen_addr.to_string(), "127.0.0.1:50051");
        assert!(cfg.peers.is_empty());
        assert_eq!(cfg.election_timeout_min_ms, 150);
        assert_eq!(cfg.election_timeout_max_ms, 300);
        assert_eq!(cfg.heartbeat_interval_ms, 50);
    }

    #[test]
    fn node_config_new() {
        let addr: SocketAddr = "10.0.0.1:9000".parse().unwrap();
        let cfg = NodeConfig::new(42, addr);
        assert_eq!(cfg.node_id, 42);
        assert_eq!(cfg.listen_addr, addr);
        assert!(cfg.peers.is_empty());
    }

    #[test]
    fn node_config_with_peer() {
        let cfg = NodeConfig::default()
            .with_peer(2, "127.0.0.1:50052".to_string())
            .with_peer(3, "127.0.0.1:50053".to_string());
        assert_eq!(cfg.peers.len(), 2);
        assert_eq!(cfg.peers[0].node_id, 2);
        assert_eq!(cfg.peers[0].addr, "127.0.0.1:50052");
        assert_eq!(cfg.peers[1].node_id, 3);
        assert_eq!(cfg.peers[1].addr, "127.0.0.1:50053");
    }

    #[test]
    fn peer_config_fields() {
        let peer = PeerConfig {
            node_id: 5,
            addr: "host.example.com:8080".to_string(),
        };
        assert_eq!(peer.node_id, 5);
        assert_eq!(peer.addr, "host.example.com:8080");
    }
}
