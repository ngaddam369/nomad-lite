use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{timeout, Duration, Instant};
use tonic::transport::Channel;

use crate::config::NodeConfig;
use crate::proto::raft_service_client::RaftServiceClient;
use crate::proto::{AppendEntriesRequest, VoteRequest};
use crate::raft::rpc::{handle_append_entries, handle_request_vote, log_entry_to_proto};
use crate::raft::state::{Command, RaftRole, RaftState};
use crate::raft::timer::random_election_timeout;

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
}

/// The main Raft node that coordinates consensus
pub struct RaftNode {
    pub id: u64,
    pub state: Arc<RwLock<RaftState>>,
    config: NodeConfig,
    peers: Arc<Mutex<HashMap<u64, RaftServiceClient<Channel>>>>,
    message_tx: mpsc::Sender<RaftMessage>,
    last_heartbeat: Arc<RwLock<Instant>>,
}

impl RaftNode {
    pub fn new(config: NodeConfig) -> (Self, mpsc::Receiver<RaftMessage>) {
        let (message_tx, message_rx) = mpsc::channel(100);

        let node = Self {
            id: config.node_id,
            state: Arc::new(RwLock::new(RaftState::new())),
            config,
            peers: Arc::new(Mutex::new(HashMap::new())),
            message_tx,
            last_heartbeat: Arc::new(RwLock::new(Instant::now())),
        };

        (node, message_rx)
    }

    /// Get the message sender for external communication
    pub fn message_sender(&self) -> mpsc::Sender<RaftMessage> {
        self.message_tx.clone()
    }

    /// Connect to peer nodes
    pub async fn connect_to_peers(&self) {
        let mut peers = self.peers.lock().await;
        for peer_config in &self.config.peers {
            let addr = format!("http://{}", peer_config.addr);
            match RaftServiceClient::connect(addr.clone()).await {
                Ok(client) => {
                    tracing::info!(peer_id = peer_config.node_id, addr = %addr, "Connected to peer");
                    peers.insert(peer_config.node_id, client);
                }
                Err(e) => {
                    tracing::warn!(
                        peer_id = peer_config.node_id,
                        addr = %addr,
                        error = %e,
                        "Failed to connect to peer"
                    );
                }
            }
        }
    }

    /// Run the Raft node main loop
    pub async fn run(&self, mut message_rx: mpsc::Receiver<RaftMessage>) {
        let mut election_timeout = random_election_timeout(
            self.config.election_timeout_min_ms,
            self.config.election_timeout_max_ms,
        );

        loop {
            let role = self.state.read().await.role;

            tokio::select! {
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
            match timeout(Duration::from_millis(100), client.request_vote(req.clone())).await {
                Ok(Ok(response)) => {
                    let resp = response.into_inner();
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
        let log_snapshot: Vec<_> = state.log.clone();
        drop(state);

        let peers = self.peers.lock().await;

        for (peer_id, client) in peers.iter() {
            let peer_next_index = *next_index.get(peer_id).unwrap_or(&1);
            let prev_log_index = peer_next_index.saturating_sub(1);
            let prev_log_term = if prev_log_index == 0 {
                0
            } else {
                log_snapshot
                    .get((prev_log_index - 1) as usize)
                    .map(|e| e.term)
                    .unwrap_or(0)
            };

            // Get entries to send
            let entries: Vec<_> = log_snapshot
                .iter()
                .filter(|e| e.index >= peer_next_index)
                .map(|e| log_entry_to_proto(e))
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

            // Send AppendEntries asynchronously
            tokio::spawn(async move {
                match timeout(Duration::from_millis(100), client.append_entries(req)).await {
                    Ok(Ok(response)) => {
                        let resp = response.into_inner();
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
    pub async fn handle_vote_request(&self, req: VoteRequest) -> crate::proto::VoteResponse {
        let mut state = self.state.write().await;
        let response = handle_request_vote(&mut state, &req, self.id);

        // Reset election timeout if we granted vote
        if response.vote_granted {
            *self.last_heartbeat.write().await = Instant::now();
        }

        response
    }

    /// Handle incoming AppendEntries RPC
    pub async fn handle_append_entries(
        &self,
        req: AppendEntriesRequest,
    ) -> crate::proto::AppendEntriesResponse {
        let mut state = self.state.write().await;
        let response = handle_append_entries(&mut state, &req, self.id);

        // Reset election timeout on successful AppendEntries
        if response.success {
            drop(state);
            *self.last_heartbeat.write().await = Instant::now();
        }

        response
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
