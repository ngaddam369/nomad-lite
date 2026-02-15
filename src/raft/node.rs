use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex, RwLock};
use tokio::time::{timeout, Duration, Instant};
use tonic::transport::{Channel, Endpoint};

use tokio_util::sync::CancellationToken;

use crate::config::NodeConfig;
use crate::proto::raft_service_client::RaftServiceClient;
use crate::proto::{AppendEntriesRequest, InstallSnapshotRequest, TimeoutNowRequest, VoteRequest};
use crate::raft::rpc::{
    handle_append_entries, handle_install_snapshot, handle_request_vote, log_entry_to_proto,
    snapshot_job_to_proto,
};
use crate::raft::state::{Command, RaftRole, RaftState, Snapshot};
use crate::raft::timer::random_election_timeout;
use crate::tls::TlsIdentity;

/// Message types for the Raft node event loop
#[derive(Debug)]
pub enum RaftMessage {
    /// Request to append a command to the log
    AppendCommand {
        command: Command,
        response_tx: tokio::sync::oneshot::Sender<Result<u64, String>>,
    },
    /// Heartbeat received from leader (resets election timeout)
    HeartbeatReceived,
    /// Trigger election
    TriggerElection,
    /// Transfer leadership to a target node (None = auto-select)
    TransferLeadership {
        target_id: Option<u64>,
        response_tx: tokio::sync::oneshot::Sender<Result<u64, String>>,
    },
}

/// Timeout for considering a peer as dead (3 seconds)
const PEER_TIMEOUT_MS: u64 = 3000;

/// The main Raft node that coordinates consensus
pub struct RaftNode {
    pub id: u64,
    pub state: Arc<RwLock<RaftState>>,
    config: NodeConfig,
    peers: Arc<Mutex<HashMap<u64, RaftServiceClient<Channel>>>>,
    /// Peers temporarily disconnected to simulate network partitions (for testing)
    disconnected_peers: Arc<Mutex<HashMap<u64, RaftServiceClient<Channel>>>>,
    message_tx: mpsc::Sender<RaftMessage>,
    last_heartbeat: Arc<RwLock<Instant>>,
    commit_notify_tx: watch::Sender<u64>,
    commit_notify_rx: watch::Receiver<u64>,
    /// Tracks last successful response time from each peer
    peer_last_seen: Arc<RwLock<HashMap<u64, Instant>>>,
    /// TLS identity for secure peer connections
    tls_identity: Option<TlsIdentity>,
    /// Pending snapshot installed by a leader — the scheduler loop must rebuild
    /// state machines from this snapshot. Uses a Mutex<Option<Snapshot>> as a
    /// simple flag/channel.
    pending_snapshot: Arc<tokio::sync::Mutex<Option<Snapshot>>>,
}

impl RaftNode {
    pub fn new(
        config: NodeConfig,
        tls_identity: Option<TlsIdentity>,
    ) -> (Self, mpsc::Receiver<RaftMessage>) {
        let (message_tx, message_rx) = mpsc::channel(100);
        let (commit_notify_tx, commit_notify_rx) = watch::channel(0);

        let node = Self {
            id: config.node_id,
            state: Arc::new(RwLock::new(RaftState::new())),
            config,
            peers: Arc::new(Mutex::new(HashMap::new())),
            disconnected_peers: Arc::new(Mutex::new(HashMap::new())),
            message_tx,
            last_heartbeat: Arc::new(RwLock::new(Instant::now())),
            commit_notify_tx,
            commit_notify_rx,
            peer_last_seen: Arc::new(RwLock::new(HashMap::new())),
            tls_identity,
            pending_snapshot: Arc::new(tokio::sync::Mutex::new(None)),
        };

        (node, message_rx)
    }

    /// Get the message sender for external communication
    pub fn message_sender(&self) -> mpsc::Sender<RaftMessage> {
        self.message_tx.clone()
    }

    /// Subscribe to commit index updates. Returns a receiver that gets notified
    /// when new entries are committed.
    pub fn subscribe_commits(&self) -> watch::Receiver<u64> {
        self.commit_notify_rx.clone()
    }

    /// Get the status of all configured peers based on recent communication.
    /// Only meaningful when called on the leader (who communicates with all peers).
    pub async fn get_peers_status(&self) -> HashMap<u64, bool> {
        let last_seen = self.peer_last_seen.read().await;
        self.config
            .peers
            .iter()
            .map(|p| {
                let is_alive = last_seen
                    .get(&p.node_id)
                    .map(|instant| instant.elapsed().as_millis() < PEER_TIMEOUT_MS as u128)
                    .unwrap_or(false);
                (p.node_id, is_alive)
            })
            .collect()
    }

    /// Connect to peer nodes
    pub async fn connect_to_peers(&self) {
        let mut peers = self.peers.lock().await;
        for peer_config in &self.config.peers {
            if peers.contains_key(&peer_config.node_id) {
                continue;
            }

            // Build URI with appropriate scheme based on TLS configuration
            let (uri, scheme) = if self.tls_identity.is_some() {
                (format!("https://{}", peer_config.addr), "https")
            } else {
                (format!("http://{}", peer_config.addr), "http")
            };

            // Create endpoint
            let endpoint = match Endpoint::from_shared(uri.clone()) {
                Ok(ep) => ep,
                Err(e) => {
                    tracing::warn!(
                        peer_id = peer_config.node_id,
                        addr = %peer_config.addr,
                        error = %e,
                        "Failed to create endpoint"
                    );
                    continue;
                }
            };

            // Configure TLS if identity is available
            let connect_result = if let Some(ref tls_identity) = self.tls_identity {
                let tls_config = tls_identity.client_tls_config();
                match endpoint.tls_config(tls_config) {
                    Ok(ep) => ep.connect().await,
                    Err(e) => {
                        tracing::warn!(
                            peer_id = peer_config.node_id,
                            addr = %peer_config.addr,
                            error = %e,
                            "Failed to configure TLS"
                        );
                        continue;
                    }
                }
            } else {
                endpoint.connect().await
            };

            match connect_result {
                Ok(channel) => {
                    let client = RaftServiceClient::new(channel);
                    tracing::info!(
                        peer_id = peer_config.node_id,
                        addr = %peer_config.addr,
                        scheme,
                        "Connected to peer"
                    );
                    peers.insert(peer_config.node_id, client);
                }
                Err(e) => {
                    tracing::warn!(
                        peer_id = peer_config.node_id,
                        addr = %peer_config.addr,
                        error = %e,
                        "Failed to connect to peer"
                    );
                }
            }
        }
    }

    /// Check if all configured peers are connected
    pub async fn all_peers_connected(&self) -> bool {
        let peers = self.peers.lock().await;
        self.config
            .peers
            .iter()
            .all(|p| peers.contains_key(&p.node_id))
    }

    /// Simulate disconnecting from a specific peer (moves client to disconnected_peers).
    /// Used for network partition testing.
    pub async fn disconnect_peer(&self, peer_id: u64) {
        let mut peers = self.peers.lock().await;
        let mut disconnected = self.disconnected_peers.lock().await;
        if let Some(client) = peers.remove(&peer_id) {
            disconnected.insert(peer_id, client);
        }
    }

    /// Reconnect a previously disconnected peer.
    /// Used for network partition healing in tests.
    pub async fn reconnect_peer(&self, peer_id: u64) {
        let mut peers = self.peers.lock().await;
        let mut disconnected = self.disconnected_peers.lock().await;
        if let Some(client) = disconnected.remove(&peer_id) {
            peers.insert(peer_id, client);
        }
    }

    /// Disconnect from all peers (full isolation).
    pub async fn disconnect_all_peers(&self) {
        let mut peers = self.peers.lock().await;
        let mut disconnected = self.disconnected_peers.lock().await;
        for (id, client) in peers.drain() {
            disconnected.insert(id, client);
        }
    }

    /// Reconnect all previously disconnected peers.
    pub async fn reconnect_all_peers(&self) {
        let mut peers = self.peers.lock().await;
        let mut disconnected = self.disconnected_peers.lock().await;
        for (id, client) in disconnected.drain() {
            peers.insert(id, client);
        }
    }

    /// Run the Raft node main loop
    pub async fn run(
        &self,
        mut message_rx: mpsc::Receiver<RaftMessage>,
        shutdown_token: CancellationToken,
    ) {
        let mut election_timeout = random_election_timeout(
            self.config.election_timeout_min_ms,
            self.config.election_timeout_max_ms,
        );

        loop {
            let role = self.state.read().await.role;

            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    tracing::info!(node_id = self.id, "Raft node shutting down");
                    break;
                }

                // Handle incoming messages
                Some(msg) = message_rx.recv() => {
                    match msg {
                        RaftMessage::AppendCommand { command, response_tx } => {
                            let result = self.handle_append_command(command).await;
                            let _ = response_tx.send(result);
                        }
                        RaftMessage::HeartbeatReceived => {
                            *self.last_heartbeat.write().await = Instant::now();
                            election_timeout = random_election_timeout(
                                self.config.election_timeout_min_ms,
                                self.config.election_timeout_max_ms,
                            );
                        }
                        RaftMessage::TriggerElection => {
                            self.start_election().await;
                        }
                        RaftMessage::TransferLeadership { target_id, response_tx } => {
                            let result = self.handle_transfer_leadership(target_id).await;
                            let _ = response_tx.send(result);
                        }
                    }
                }

                // Election timeout (for followers and candidates)
                _ = tokio::time::sleep(election_timeout), if role != RaftRole::Leader => {
                    let elapsed = self.last_heartbeat.read().await.elapsed();
                    if elapsed >= election_timeout {
                        tracing::info!(
                            node_id = self.id,
                            elapsed_ms = elapsed.as_millis(),
                            "Election timeout, starting election"
                        );
                        self.start_election().await;
                    }
                    election_timeout = random_election_timeout(
                        self.config.election_timeout_min_ms,
                        self.config.election_timeout_max_ms,
                    );
                }

                // Heartbeat interval (for leaders)
                _ = tokio::time::sleep(Duration::from_millis(self.config.heartbeat_interval_ms)), if role == RaftRole::Leader => {
                    self.send_heartbeats().await;
                }
            }
        }
    }

    /// Start a new election
    async fn start_election(&self) {
        let mut state = self.state.write().await;
        state.become_candidate(self.id);
        let term = state.current_term;
        let last_log_index = state.last_log_index();
        let last_log_term = state.last_log_term();
        let total_nodes = self.config.peers.len() + 1; // peers + self
        let majority = (total_nodes / 2) + 1;
        drop(state);

        tracing::info!(node_id = self.id, term, "Starting election");

        // Request votes from all peers
        let req = VoteRequest {
            term,
            candidate_id: self.id,
            last_log_index,
            last_log_term,
        };

        let peers = self.peers.lock().await;
        let mut vote_count = 1u64; // Vote for self

        for (peer_id, client) in peers.iter() {
            let mut client = client.clone();
            match timeout(Duration::from_millis(100), client.request_vote(req)).await {
                Ok(Ok(response)) => {
                    let resp = response.into_inner();

                    // Record successful response from peer
                    self.peer_last_seen
                        .write()
                        .await
                        .insert(*peer_id, Instant::now());

                    if resp.term > term {
                        // Higher term seen, become follower
                        self.state.write().await.become_follower(resp.term);
                        return;
                    }
                    if resp.vote_granted {
                        vote_count += 1;
                        tracing::debug!(
                            node_id = self.id,
                            peer_id,
                            votes = vote_count,
                            "Received vote"
                        );
                    }
                }
                Ok(Err(e)) => {
                    tracing::warn!(peer_id, error = %e, "Vote request failed");
                }
                Err(_) => {
                    tracing::warn!(peer_id, "Vote request timed out");
                }
            }
        }
        drop(peers);

        // Check if we won
        let mut state = self.state.write().await;
        if state.role == RaftRole::Candidate && state.current_term == term {
            state.votes_received = vote_count;
            if vote_count >= majority as u64 {
                let peer_ids: Vec<u64> = self.config.peers.iter().map(|p| p.node_id).collect();
                state.become_leader(self.id, &peer_ids);
                tracing::info!(node_id = self.id, term, votes = vote_count, "Became leader");
            } else {
                tracing::debug!(
                    node_id = self.id,
                    term,
                    votes = vote_count,
                    needed = majority,
                    "Election failed, not enough votes"
                );
            }
        }
    }

    /// Send heartbeats to all followers (leader only)
    async fn send_heartbeats(&self) {
        let state = self.state.read().await;
        if state.role != RaftRole::Leader {
            return;
        }

        let term = state.current_term;
        let commit_index = state.commit_index;
        let next_index = state.next_index.clone();
        let log_offset = state.log_offset;
        let snapshot = state.snapshot.clone();

        // Only clone log entries that might be needed for replication.
        // Start from (min_next_index - 1) to include the entry needed for prev_log_term.
        let min_next_index = next_index.values().copied().min().unwrap_or(1);
        let entries_start = min_next_index.saturating_sub(1).max(log_offset + 1);
        let log_entries = state.get_entries_from(entries_start);
        drop(state);

        let peers = self.peers.lock().await;

        for (peer_id, client) in peers.iter() {
            let peer_next_index = *next_index.get(peer_id).unwrap_or(&1);

            // If the peer is too far behind (needs compacted entries), send snapshot
            if peer_next_index <= log_offset {
                if let Some(ref snapshot) = snapshot {
                    let req = InstallSnapshotRequest {
                        term,
                        leader_id: self.id,
                        last_included_index: snapshot.last_included_index,
                        last_included_term: snapshot.last_included_term,
                        jobs: snapshot.jobs.iter().map(snapshot_job_to_proto).collect(),
                        workers: snapshot.workers.clone(),
                    };

                    let mut client = client.clone();
                    let peer_id = *peer_id;
                    let state = self.state.clone();
                    let peer_last_seen = self.peer_last_seen.clone();
                    let snapshot_last = snapshot.last_included_index;

                    tokio::spawn(async move {
                        match timeout(Duration::from_millis(500), client.install_snapshot(req))
                            .await
                        {
                            Ok(Ok(response)) => {
                                let resp = response.into_inner();
                                peer_last_seen.write().await.insert(peer_id, Instant::now());

                                let mut state = state.write().await;
                                if resp.term > state.current_term {
                                    state.become_follower(resp.term);
                                    return;
                                }
                                // Advance next_index past the snapshot
                                state.next_index.insert(peer_id, snapshot_last + 1);
                                state.match_index.insert(peer_id, snapshot_last);
                                tracing::info!(
                                    peer_id,
                                    snapshot_last,
                                    "Snapshot sent to slow follower"
                                );
                            }
                            Ok(Err(e)) => {
                                tracing::trace!(peer_id, error = %e, "InstallSnapshot failed");
                            }
                            Err(_) => {
                                tracing::trace!(peer_id, "InstallSnapshot timed out");
                            }
                        }
                    });
                    continue; // Skip normal AppendEntries for this peer
                }
            }

            let prev_log_index = peer_next_index.saturating_sub(1);
            let prev_log_term = if prev_log_index == 0 {
                0
            } else if prev_log_index == log_offset {
                // Term from snapshot
                snapshot.as_ref().map(|s| s.last_included_term).unwrap_or(0)
            } else {
                log_entries
                    .iter()
                    .find(|e| e.index == prev_log_index)
                    .map(|e| e.term)
                    .unwrap_or(0)
            };

            // Get entries to send
            let entries: Vec<_> = log_entries
                .iter()
                .filter(|e| e.index >= peer_next_index)
                .map(log_entry_to_proto)
                .collect();

            let req = AppendEntriesRequest {
                term,
                leader_id: self.id,
                prev_log_index,
                prev_log_term,
                entries,
                leader_commit: commit_index,
            };

            let mut client = client.clone();
            let peer_id = *peer_id;
            let state = self.state.clone();
            let commit_notify = self.commit_notify_tx.clone();
            let peer_last_seen = self.peer_last_seen.clone();

            // Send AppendEntries asynchronously
            tokio::spawn(async move {
                match timeout(Duration::from_millis(100), client.append_entries(req)).await {
                    Ok(Ok(response)) => {
                        let resp = response.into_inner();

                        // Record successful response from peer
                        peer_last_seen.write().await.insert(peer_id, Instant::now());

                        let mut state = state.write().await;

                        if resp.term > state.current_term {
                            state.become_follower(resp.term);
                            return;
                        }

                        if state.role == RaftRole::Leader && resp.success {
                            state.match_index.insert(peer_id, resp.match_index);
                            state.next_index.insert(peer_id, resp.match_index + 1);

                            // Update commit index
                            let mut match_indices: Vec<_> =
                                state.match_index.values().copied().collect();
                            match_indices.push(state.last_log_index()); // Include self
                            match_indices.sort_unstable();

                            let majority_index = match_indices.len() / 2;
                            let new_commit_index = match_indices[majority_index];

                            if new_commit_index > state.commit_index {
                                // Only commit entries from current term
                                if let Some(entry) = state.get_entry(new_commit_index) {
                                    if entry.term == state.current_term {
                                        state.commit_index = new_commit_index;
                                        let _ = commit_notify.send(new_commit_index);
                                        tracing::debug!(
                                            commit_index = new_commit_index,
                                            "Updated commit index"
                                        );
                                    }
                                }
                            }
                        } else if state.role == RaftRole::Leader && !resp.success {
                            // Decrement next_index and retry
                            let current = state.next_index.get(&peer_id).copied().unwrap_or(1);
                            if current > 1 {
                                state.next_index.insert(peer_id, current - 1);
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::trace!(peer_id, error = %e, "AppendEntries failed");
                    }
                    Err(_) => {
                        tracing::trace!(peer_id, "AppendEntries timed out");
                    }
                }
            });
        }
    }

    /// Handle a request to append a command (leader only)
    async fn handle_append_command(&self, command: Command) -> Result<u64, String> {
        let mut state = self.state.write().await;

        if state.role != RaftRole::Leader {
            return Err(format!("Not leader. Current leader: {:?}", state.leader_id));
        }

        let entry = state.append_entry(command);
        let index = entry.index;
        tracing::debug!(index, term = entry.term, "Appended command to log");

        Ok(index)
    }

    /// Handle incoming RequestVote RPC
    pub async fn handle_vote_request(
        &self,
        req: VoteRequest,
    ) -> Result<crate::proto::VoteResponse, String> {
        let mut state = self.state.write().await;
        let response = handle_request_vote(&mut state, &req, self.id)?;

        // Reset election timeout if we granted vote
        if response.vote_granted {
            *self.last_heartbeat.write().await = Instant::now();
        }

        Ok(response)
    }

    /// Handle incoming AppendEntries RPC
    pub async fn handle_append_entries(
        &self,
        req: AppendEntriesRequest,
    ) -> Result<crate::proto::AppendEntriesResponse, String> {
        let mut state = self.state.write().await;
        let old_commit_index = state.commit_index;
        let response = handle_append_entries(&mut state, &req, self.id)?;

        // Notify if commit_index advanced
        if response.success && state.commit_index > old_commit_index {
            let _ = self.commit_notify_tx.send(state.commit_index);
        }

        // Reset election timeout on successful AppendEntries
        if response.success {
            drop(state);
            *self.last_heartbeat.write().await = Instant::now();
        }

        Ok(response)
    }

    /// Handle leadership transfer request
    async fn handle_transfer_leadership(&self, target_id: Option<u64>) -> Result<u64, String> {
        let state = self.state.read().await;
        if state.role != RaftRole::Leader {
            return Err("Not the leader".to_string());
        }

        // Pick target: explicit or auto-select by highest match_index
        let target = match target_id {
            Some(id) => {
                // Verify target is a known peer
                if !self.config.peers.iter().any(|p| p.node_id == id) {
                    return Err(format!("Unknown target node {}", id));
                }
                id
            }
            None => {
                // Auto-select: pick the peer with the highest match_index
                state
                    .match_index
                    .iter()
                    .max_by_key(|(_, &idx)| idx)
                    .map(|(&id, _)| id)
                    .ok_or_else(|| "No peers available for transfer".to_string())?
            }
        };

        let term = state.current_term;
        let leader_last_index = state.last_log_index();
        drop(state);

        tracing::info!(
            node_id = self.id,
            target,
            term,
            "Starting leadership transfer"
        );

        // Wait for target to catch up (up to 2s)
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let state = self.state.read().await;
            let target_match = state.match_index.get(&target).copied().unwrap_or(0);
            if target_match >= leader_last_index {
                break;
            }
            drop(state);

            if Instant::now() >= deadline {
                return Err(format!("Target node {} did not catch up within 2s", target));
            }

            // Send heartbeats to push entries, then wait
            self.send_heartbeats().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Send TimeoutNow to target
        self.send_timeout_now(target, term).await?;

        // Step down to follower
        self.state.write().await.become_follower(term);
        tracing::info!(
            node_id = self.id,
            target,
            "Stepped down after leadership transfer"
        );

        Ok(target)
    }

    /// Send TimeoutNow RPC to a target peer
    async fn send_timeout_now(&self, target: u64, term: u64) -> Result<(), String> {
        let peers = self.peers.lock().await;
        let client = peers
            .get(&target)
            .ok_or_else(|| format!("Not connected to target node {}", target))?;

        let mut client = client.clone();
        drop(peers);

        let req = TimeoutNowRequest {
            term,
            leader_id: self.id,
        };

        match timeout(Duration::from_millis(500), client.timeout_now(req)).await {
            Ok(Ok(response)) => {
                let resp = response.into_inner();
                if resp.success {
                    tracing::info!(target, "TimeoutNow accepted by target");
                    Ok(())
                } else {
                    Err(format!(
                        "TimeoutNow rejected by node {} (term {})",
                        target, resp.term
                    ))
                }
            }
            Ok(Err(e)) => Err(format!("TimeoutNow RPC failed: {}", e)),
            Err(_) => Err(format!("TimeoutNow RPC to node {} timed out", target)),
        }
    }

    /// Handle incoming TimeoutNow RPC — immediately start an election
    pub async fn handle_timeout_now(
        &self,
        req: TimeoutNowRequest,
    ) -> Result<crate::proto::TimeoutNowResponse, String> {
        let state = self.state.read().await;
        let current_term = state.current_term;

        if req.term < current_term {
            return Ok(crate::proto::TimeoutNowResponse {
                term: current_term,
                success: false,
            });
        }
        drop(state);

        tracing::info!(
            node_id = self.id,
            from_leader = req.leader_id,
            term = req.term,
            "Received TimeoutNow, starting immediate election"
        );

        // Immediately start election
        self.start_election().await;

        let new_term = self.state.read().await.current_term;
        Ok(crate::proto::TimeoutNowResponse {
            term: new_term,
            success: true,
        })
    }

    /// Handle incoming InstallSnapshot RPC
    pub async fn handle_install_snapshot(
        &self,
        req: InstallSnapshotRequest,
    ) -> Result<crate::proto::InstallSnapshotResponse, String> {
        let mut state = self.state.write().await;
        let response = handle_install_snapshot(&mut state, &req, self.id)?;

        // If the snapshot was installed, store it for the scheduler loop to rebuild state
        if state.snapshot.is_some() && state.log_offset >= req.last_included_index {
            if let Some(ref snapshot) = state.snapshot {
                let mut pending = self.pending_snapshot.lock().await;
                *pending = Some(snapshot.clone());
            }
            // Notify commit watchers so the scheduler loop wakes up
            let _ = self.commit_notify_tx.send(state.commit_index);
        }

        // Reset election timeout
        drop(state);
        *self.last_heartbeat.write().await = Instant::now();

        Ok(response)
    }

    /// Take the pending snapshot (if any) for the scheduler loop to process.
    pub async fn take_pending_snapshot(&self) -> Option<Snapshot> {
        self.pending_snapshot.lock().await.take()
    }

    /// Check if this node is the leader
    pub async fn is_leader(&self) -> bool {
        self.state.read().await.role == RaftRole::Leader
    }

    /// Get the current leader ID
    pub async fn get_leader_id(&self) -> Option<u64> {
        let state = self.state.read().await;
        if state.role == RaftRole::Leader {
            Some(self.id)
        } else {
            state.leader_id
        }
    }

    /// Get entries that have been committed but not yet applied
    pub async fn get_committed_entries(&self) -> Vec<crate::raft::state::LogEntry> {
        let mut state = self.state.write().await;
        let mut entries = Vec::new();

        while state.last_applied < state.commit_index {
            state.last_applied += 1;
            if let Some(entry) = state.get_entry(state.last_applied) {
                entries.push(entry.clone());
            }
        }

        entries
    }
}
