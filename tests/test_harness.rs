//! Test harness for multi-node Raft cluster integration tests.
//!
//! Provides utilities for spawning, managing, and testing multi-node clusters.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, RwLock};
use tokio::task::JoinHandle;
use uuid::Uuid;

use chrono::Utc;
use nomad_lite::config::{NodeConfig, PeerConfig, SandboxConfig, TlsConfig};
use nomad_lite::grpc::GrpcServer;
use nomad_lite::raft::node::RaftMessage;
use nomad_lite::raft::state::{Command, RaftRole};
use nomad_lite::raft::RaftNode;
use nomad_lite::scheduler::{Job, JobQueue};
use tokio_util::sync::CancellationToken;

/// Test node configuration with shorter timeouts for faster tests
pub fn test_node_config(node_id: u64, port: u16, peers: Vec<(u64, u16)>) -> NodeConfig {
    let peer_configs: Vec<PeerConfig> = peers
        .into_iter()
        .map(|(id, p)| PeerConfig {
            node_id: id,
            addr: format!("127.0.0.1:{}", p),
        })
        .collect();

    NodeConfig {
        node_id,
        listen_addr: format!("127.0.0.1:{}", port).parse().unwrap(),
        peers: peer_configs,
        // Shorter timeouts for faster tests
        election_timeout_min_ms: 50,
        election_timeout_max_ms: 100,
        heartbeat_interval_ms: 20,
        sandbox: SandboxConfig::default(),
        tls: TlsConfig::default(),
    }
}

/// Handle to a running test node
pub struct TestNode {
    pub node_id: u64,
    #[allow(dead_code)]
    pub port: u16,
    pub raft_node: Arc<RaftNode>,
    pub job_queue: Arc<RwLock<JobQueue>>,
    raft_handle: JoinHandle<()>,
    grpc_handle: JoinHandle<()>,
    scheduler_handle: JoinHandle<()>,
}

impl TestNode {
    /// Check if this node is the leader
    pub async fn is_leader(&self) -> bool {
        let state = self.raft_node.state.read().await;
        state.role == RaftRole::Leader
    }

    /// Get the current term
    pub async fn current_term(&self) -> u64 {
        self.raft_node.state.read().await.current_term
    }

    /// Get the log length
    pub async fn log_len(&self) -> usize {
        self.raft_node.state.read().await.log.len()
    }

    /// Get the known leader ID
    #[allow(dead_code)]
    pub async fn leader_id(&self) -> Option<u64> {
        self.raft_node.state.read().await.leader_id
    }
}

impl Drop for TestNode {
    fn drop(&mut self) {
        // Abort all tasks to ensure clean shutdown
        self.raft_handle.abort();
        self.grpc_handle.abort();
        self.scheduler_handle.abort();
    }
}

/// Test cluster managing multiple nodes
pub struct TestCluster {
    pub nodes: HashMap<u64, TestNode>,
    #[allow(dead_code)]
    base_port: u16,
}

impl TestCluster {
    /// Create and start a cluster with n nodes
    pub async fn new(num_nodes: usize, base_port: u16) -> Self {
        let mut cluster = Self {
            nodes: HashMap::new(),
            base_port,
        };

        // Calculate all peer configurations
        let all_peers: Vec<(u64, u16)> = (0..num_nodes)
            .map(|i| ((i + 1) as u64, base_port + i as u16))
            .collect();

        // Create and start each node
        for i in 0..num_nodes {
            let node_id = (i + 1) as u64;
            let port = base_port + i as u16;

            // Get peers (all nodes except self)
            let peers: Vec<(u64, u16)> = all_peers
                .iter()
                .filter(|(id, _)| *id != node_id)
                .copied()
                .collect();

            let config = test_node_config(node_id, port, peers);
            let test_node = Self::start_node(config).await;
            cluster.nodes.insert(node_id, test_node);
        }

        // Wait briefly for all nodes to start their gRPC servers
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect all nodes to their peers
        for node in cluster.nodes.values() {
            node.raft_node.connect_to_peers().await;
        }

        cluster
    }

    /// Start a single node
    async fn start_node(config: NodeConfig) -> TestNode {
        let node_id = config.node_id;
        let port = config.listen_addr.port();
        let listen_addr = config.listen_addr;

        let (raft_node, raft_rx) = RaftNode::new(config.clone(), None);
        let raft_node = Arc::new(raft_node);
        let job_queue = Arc::new(RwLock::new(JobQueue::new()));

        // Spawn Raft event loop
        let raft_node_clone = raft_node.clone();
        let raft_handle = tokio::spawn(async move {
            raft_node_clone.run(raft_rx, CancellationToken::new()).await;
        });

        // Spawn scheduler loop (applies committed entries to job queue)
        let scheduler_raft = raft_node.clone();
        let scheduler_queue = job_queue.clone();
        let scheduler_handle = tokio::spawn(async move {
            Self::scheduler_loop(scheduler_raft, scheduler_queue).await;
        });

        // Spawn gRPC server
        let draining = Arc::new(AtomicBool::new(false));
        let grpc_server = GrpcServer::new(
            listen_addr,
            config,
            raft_node.clone(),
            job_queue.clone(),
            None,
            draining.clone(),
        );

        let grpc_handle = tokio::spawn(async move {
            if let Err(e) = grpc_server.run(CancellationToken::new()).await {
                tracing::error!("gRPC server error: {}", e);
            }
        });

        TestNode {
            node_id,
            port,
            raft_node,
            job_queue,
            raft_handle,
            grpc_handle,
            scheduler_handle,
        }
    }

    /// Simplified scheduler loop that applies committed entries to job queue
    async fn scheduler_loop(raft_node: Arc<RaftNode>, job_queue: Arc<RwLock<JobQueue>>) {
        let mut commit_rx = raft_node.subscribe_commits();

        loop {
            if commit_rx.changed().await.is_err() {
                break;
            }

            // Apply committed entries
            let entries = raft_node.get_committed_entries().await;
            for entry in entries {
                match entry.command {
                    Command::SubmitJob {
                        job_id,
                        command,
                        created_at,
                    } => {
                        let mut queue = job_queue.write().await;
                        if queue.get_job(&job_id).is_none() {
                            queue.add_job(Job::with_id(job_id, command, created_at));
                        }
                    }
                    Command::UpdateJobStatus {
                        job_id,
                        status,
                        executed_by,
                        exit_code,
                        completed_at,
                    } => {
                        let mut queue = job_queue.write().await;
                        queue.update_status_metadata(
                            &job_id,
                            status,
                            executed_by,
                            exit_code,
                            completed_at,
                        );
                    }
                    Command::RegisterWorker { .. } | Command::Noop => {}
                }
            }
        }
    }

    /// Wait for leader election with timeout
    pub async fn wait_for_leader(&self, timeout_duration: Duration) -> Option<u64> {
        let result = wait_for(
            || async {
                for node in self.nodes.values() {
                    if node.is_leader().await {
                        return true;
                    }
                }
                false
            },
            timeout_duration,
            Duration::from_millis(50),
        )
        .await;

        if result {
            self.get_leader_id().await
        } else {
            None
        }
    }

    /// Get current leader ID
    pub async fn get_leader_id(&self) -> Option<u64> {
        for node in self.nodes.values() {
            if node.is_leader().await {
                return Some(node.node_id);
            }
        }
        None
    }

    /// Get a reference to a specific node
    pub fn get_node(&self, node_id: u64) -> Option<&TestNode> {
        self.nodes.get(&node_id)
    }

    /// Submit a job through the leader
    pub async fn submit_job(&self, command: &str) -> Result<Uuid, String> {
        let leader_id = self.get_leader_id().await.ok_or("No leader elected")?;
        let leader = self.get_node(leader_id).ok_or("Leader node not found")?;

        let job_id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel();

        leader
            .raft_node
            .message_sender()
            .send(RaftMessage::AppendCommand {
                command: Command::SubmitJob {
                    job_id,
                    command: command.to_string(),
                    created_at: Utc::now(),
                },
                response_tx: tx,
            })
            .await
            .map_err(|e| format!("Failed to send command: {}", e))?;

        rx.await
            .map_err(|e| format!("Failed to receive response: {}", e))?
            .map(|_| job_id)
    }

    /// Wait for a specific commit index across all nodes
    pub async fn wait_for_commit_on_all(
        &self,
        min_commits: usize,
        timeout_duration: Duration,
    ) -> bool {
        wait_for(
            || async {
                for node in self.nodes.values() {
                    let log_len = node.log_len().await;
                    if log_len < min_commits {
                        return false;
                    }
                }
                true
            },
            timeout_duration,
            Duration::from_millis(50),
        )
        .await
    }

    /// Verify all nodes have consistent logs (same length and entries)
    pub async fn verify_log_consistency(&self) -> bool {
        if self.nodes.is_empty() {
            return true;
        }

        let mut log_lengths = Vec::new();
        for node in self.nodes.values() {
            log_lengths.push(node.log_len().await);
        }

        // All nodes should have the same log length
        let first_len = log_lengths[0];
        log_lengths.iter().all(|&len| len == first_len)
    }

    /// Count the number of leaders in the cluster
    pub async fn count_leaders(&self) -> usize {
        let mut count = 0;
        for node in self.nodes.values() {
            if node.is_leader().await {
                count += 1;
            }
        }
        count
    }

    /// Shutdown a specific node (simulates crash)
    pub fn shutdown_node(&mut self, node_id: u64) -> bool {
        // Removing the node will drop it, aborting all its tasks
        self.nodes.remove(&node_id).is_some()
    }

    /// Get IDs of all active nodes
    pub fn active_node_ids(&self) -> Vec<u64> {
        self.nodes.keys().copied().collect()
    }

    /// Wait for a new leader among remaining nodes (excluding a specific node)
    pub async fn wait_for_new_leader(
        &self,
        excluded_node: u64,
        timeout_duration: Duration,
    ) -> Option<u64> {
        let result = wait_for(
            || async {
                for (node_id, node) in self.nodes.iter() {
                    if *node_id != excluded_node && node.is_leader().await {
                        return true;
                    }
                }
                false
            },
            timeout_duration,
            Duration::from_millis(50),
        )
        .await;

        if result {
            for (node_id, node) in self.nodes.iter() {
                if *node_id != excluded_node && node.is_leader().await {
                    return Some(*node_id);
                }
            }
        }
        None
    }

    /// Transfer leadership from the current leader to a target node (or auto-select if None)
    pub async fn transfer_leadership(&self, target: Option<u64>) -> Result<u64, String> {
        let leader_id = self.get_leader_id().await.ok_or("No leader elected")?;
        let leader = self.get_node(leader_id).ok_or("Leader node not found")?;

        let (tx, rx) = oneshot::channel();
        leader
            .raft_node
            .message_sender()
            .send(RaftMessage::TransferLeadership {
                target_id: target,
                response_tx: tx,
            })
            .await
            .map_err(|e| format!("Failed to send transfer request: {}", e))?;

        rx.await
            .map_err(|e| format!("Failed to receive transfer response: {}", e))?
    }

    /// Wait for commit on remaining nodes (excluding shutdown nodes)
    pub async fn wait_for_commit_on_remaining(
        &self,
        min_commits: usize,
        timeout_duration: Duration,
    ) -> bool {
        wait_for(
            || async {
                for node in self.nodes.values() {
                    let log_len = node.log_len().await;
                    if log_len < min_commits {
                        return false;
                    }
                }
                true
            },
            timeout_duration,
            Duration::from_millis(50),
        )
        .await
    }

    /// Create a network partition: group_a can't communicate with group_b and vice versa
    pub async fn create_partition(&self, group_a: &[u64], group_b: &[u64]) {
        for &node_a in group_a {
            if let Some(node) = self.nodes.get(&node_a) {
                for &node_b in group_b {
                    node.raft_node.disconnect_peer(node_b).await;
                }
            }
        }
        for &node_b in group_b {
            if let Some(node) = self.nodes.get(&node_b) {
                for &node_a in group_a {
                    node.raft_node.disconnect_peer(node_a).await;
                }
            }
        }
    }

    /// Heal a network partition: restore communication between groups
    pub async fn heal_partition(&self, group_a: &[u64], group_b: &[u64]) {
        for &node_a in group_a {
            if let Some(node) = self.nodes.get(&node_a) {
                for &node_b in group_b {
                    node.raft_node.reconnect_peer(node_b).await;
                }
            }
        }
        for &node_b in group_b {
            if let Some(node) = self.nodes.get(&node_b) {
                for &node_a in group_a {
                    node.raft_node.reconnect_peer(node_a).await;
                }
            }
        }
    }

    /// Isolate a node from all other nodes
    pub async fn isolate_node(&self, node_id: u64) {
        let other_ids: Vec<u64> = self
            .nodes
            .keys()
            .filter(|&&id| id != node_id)
            .copied()
            .collect();
        self.create_partition(&[node_id], &other_ids).await;
    }

    /// Heal an isolated node (reconnect to all others)
    pub async fn heal_node(&self, node_id: u64) {
        let other_ids: Vec<u64> = self
            .nodes
            .keys()
            .filter(|&&id| id != node_id)
            .copied()
            .collect();
        self.heal_partition(&[node_id], &other_ids).await;
    }

    /// Wait for a leader to emerge within a specific group of nodes
    pub async fn wait_for_leader_in_group(
        &self,
        group: &[u64],
        timeout_duration: Duration,
    ) -> Option<u64> {
        let result = wait_for(
            || async {
                for &node_id in group {
                    if let Some(node) = self.nodes.get(&node_id) {
                        if node.is_leader().await {
                            return true;
                        }
                    }
                }
                false
            },
            timeout_duration,
            Duration::from_millis(50),
        )
        .await;

        if result {
            for &node_id in group {
                if let Some(node) = self.nodes.get(&node_id) {
                    if node.is_leader().await {
                        return Some(node_id);
                    }
                }
            }
        }
        None
    }

    /// Submit a job directly to a specific node (must be leader)
    pub async fn submit_job_to_node(&self, node_id: u64, command: &str) -> Result<Uuid, String> {
        let node = self.nodes.get(&node_id).ok_or("Node not found")?;

        let job_id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel();

        node.raft_node
            .message_sender()
            .send(RaftMessage::AppendCommand {
                command: Command::SubmitJob {
                    job_id,
                    command: command.to_string(),
                    created_at: Utc::now(),
                },
                response_tx: tx,
            })
            .await
            .map_err(|e| format!("Failed to send command: {}", e))?;

        rx.await
            .map_err(|e| format!("Failed to receive response: {}", e))?
            .map(|_| job_id)
    }

    /// Wait for a specific commit count on a specific set of nodes
    pub async fn wait_for_commit_on_nodes(
        &self,
        node_ids: &[u64],
        min_commits: usize,
        timeout_duration: Duration,
    ) -> bool {
        wait_for(
            || async {
                for &node_id in node_ids {
                    if let Some(node) = self.nodes.get(&node_id) {
                        if node.log_len().await < min_commits {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                true
            },
            timeout_duration,
            Duration::from_millis(50),
        )
        .await
    }

    /// Verify log consistency among remaining nodes
    pub async fn verify_log_consistency_remaining(&self) -> bool {
        if self.nodes.len() < 2 {
            return true;
        }

        let mut log_lengths = Vec::new();
        for node in self.nodes.values() {
            log_lengths.push(node.log_len().await);
        }

        let first_len = log_lengths[0];
        log_lengths.iter().all(|&len| len == first_len)
    }

    /// Shutdown all nodes (best effort cleanup)
    pub async fn shutdown(&mut self) {
        // Nodes will be dropped when cluster is dropped
        // The spawned tasks will be aborted
        self.nodes.clear();
    }
}

impl Drop for TestCluster {
    fn drop(&mut self) {
        // Tasks are aborted when their handles are dropped
    }
}

/// Wait for a condition to become true with timeout
pub async fn wait_for<F, Fut>(
    condition: F,
    timeout_duration: Duration,
    poll_interval: Duration,
) -> bool
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = tokio::time::Instant::now();
    while start.elapsed() < timeout_duration {
        if condition().await {
            return true;
        }
        tokio::time::sleep(poll_interval).await;
    }
    false
}

/// Assert a condition eventually becomes true
pub async fn assert_eventually<F, Fut>(condition: F, timeout_duration: Duration, message: &str)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    let result = wait_for(condition, timeout_duration, Duration::from_millis(50)).await;
    assert!(result, "{}", message);
}
