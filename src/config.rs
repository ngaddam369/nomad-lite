use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub node_id: u64,
    pub listen_addr: SocketAddr,
    pub peers: Vec<PeerConfig>,
    pub election_timeout_min_ms: u64,
    pub election_timeout_max_ms: u64,
    pub heartbeat_interval_ms: u64,
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
            listen_addr: "127.0.0.1:50051".parse().unwrap(),
            peers: Vec::new(),
            election_timeout_min_ms: 150,
            election_timeout_max_ms: 300,
            heartbeat_interval_ms: 50,
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
