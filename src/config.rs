use std::net::SocketAddr;

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

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub node_id: u64,
    pub listen_addr: SocketAddr,
    pub peers: Vec<PeerConfig>,
    pub election_timeout_min_ms: u64,
    pub election_timeout_max_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub sandbox: SandboxConfig,
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
