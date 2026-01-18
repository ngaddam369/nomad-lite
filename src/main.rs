use clap::Parser;
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;

use nomad_lite::config::{NodeConfig, PeerConfig};
use nomad_lite::node::Node;

#[derive(Parser, Debug)]
#[command(name = "nomad-lite")]
#[command(about = "A distributed job scheduler with Raft consensus")]
struct Args {
    /// Node ID (unique identifier for this node)
    #[arg(long, default_value = "1")]
    node_id: u64,

    /// Port to listen on for gRPC
    #[arg(long, default_value = "50051")]
    port: u16,

    /// Port for the web dashboard (optional)
    #[arg(long)]
    dashboard_port: Option<u16>,

    /// Peer addresses (comma-separated, format: "id:host:port")
    /// Example: "2:127.0.0.1:50052,3:127.0.0.1:50053"
    #[arg(long, default_value = "")]
    peers: String,
}

fn parse_peers(peers_str: &str) -> Vec<PeerConfig> {
    if peers_str.is_empty() {
        return Vec::new();
    }

    peers_str
        .split(',')
        .filter_map(|peer| {
            let parts: Vec<&str> = peer.trim().split(':').collect();
            if parts.len() == 3 {
                let node_id: u64 = parts[0].parse().ok()?;
                let host = parts[1];
                let port = parts[2];
                let addr = format!("{}:{}", host, port);
                Some(PeerConfig { node_id, addr })
            } else {
                tracing::warn!(peer, "Invalid peer format, expected id:host:port");
                None
            }
        })
        .collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let listen_addr: SocketAddr = format!("0.0.0.0:{}", args.port).parse()?;
    let dashboard_addr: Option<SocketAddr> = match args.dashboard_port {
        Some(p) => Some(format!("0.0.0.0:{}", p).parse()?),
        None => None,
    };
    let peers = parse_peers(&args.peers);

    let config = NodeConfig {
        node_id: args.node_id,
        listen_addr,
        peers,
        election_timeout_min_ms: 150,
        election_timeout_max_ms: 300,
        heartbeat_interval_ms: 50,
    };

    tracing::info!(
        node_id = config.node_id,
        listen_addr = %config.listen_addr,
        dashboard_addr = ?dashboard_addr,
        peers = ?config.peers.iter().map(|p| format!("{}:{}", p.node_id, p.addr)).collect::<Vec<_>>(),
        "Starting nomad-lite node"
    );

    let (node, raft_rx) = Node::new(config, dashboard_addr);
    node.run(raft_rx).await?;

    Ok(())
}
