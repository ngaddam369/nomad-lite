use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::config::NodeConfig;
use crate::proto::internal_service_client::InternalServiceClient;
use crate::proto::scheduler_service_client::SchedulerServiceClient;
use crate::proto::scheduler_service_server::SchedulerService;
use crate::proto::{
    GetClusterStatusRequest, GetClusterStatusResponse, GetJobOutputRequest, GetJobStatusRequest,
    GetJobStatusResponse, GetRaftLogEntriesRequest, GetRaftLogEntriesResponse, JobInfo,
    JobStatus as ProtoJobStatus, ListJobsRequest, ListJobsResponse, NodeInfo, RaftLogEntryInfo,
    StreamJobsRequest, SubmitJobRequest, SubmitJobResponse,
};
use crate::raft::rpc::log_entry_to_proto;
use crate::raft::{Command, RaftNode};
use crate::scheduler::{Job, JobQueue, JobStatus};
use crate::tls::TlsIdentity;

/// gRPC service for client-facing API
pub struct ClientService {
    config: NodeConfig,
    raft_node: Arc<RaftNode>,
    job_queue: Arc<RwLock<JobQueue>>,
    /// Connection pool for forwarding requests to other nodes
    client_pool: Arc<Mutex<HashMap<u64, SchedulerServiceClient<Channel>>>>,
    /// Connection pool for internal service requests (fetching job output)
    internal_pool: Arc<Mutex<HashMap<u64, InternalServiceClient<Channel>>>>,
    /// TLS identity for secure connections to other nodes
    tls_identity: Option<TlsIdentity>,
}

impl ClientService {
    pub fn new(
        config: NodeConfig,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
        tls_identity: Option<TlsIdentity>,
    ) -> Self {
        Self {
            config,
            raft_node,
            job_queue,
            client_pool: Arc::new(Mutex::new(HashMap::new())),
            internal_pool: Arc::new(Mutex::new(HashMap::new())),
            tls_identity,
        }
    }

    /// Create a channel to a peer with TLS if configured
    async fn create_channel(&self, peer_addr: &str) -> Result<Channel, Status> {
        let uri = if self.tls_identity.is_some() {
            format!("https://{}", peer_addr)
        } else {
            format!("http://{}", peer_addr)
        };

        let endpoint = Endpoint::from_shared(uri.clone())
            .map_err(|e| Status::internal(format!("Invalid endpoint: {}", e)))?;

        let channel = if let Some(ref tls_identity) = self.tls_identity {
            let tls_config = tls_identity.client_tls_config();
            endpoint
                .tls_config(tls_config)
                .map_err(|e| Status::internal(format!("TLS config error: {}", e)))?
                .connect()
                .await
        } else {
            endpoint.connect().await
        };

        channel
            .map_err(|e| Status::unavailable(format!("Failed to connect to {}: {}", peer_addr, e)))
    }

    /// Get or create a cached connection to a peer node
    async fn get_client(&self, node_id: u64) -> Result<SchedulerServiceClient<Channel>, Status> {
        let mut pool = self.client_pool.lock().await;

        // Return cached client if available
        if let Some(client) = pool.get(&node_id) {
            return Ok(client.clone());
        }

        // Find peer address
        let peer_addr = self
            .config
            .peers
            .iter()
            .find(|p| p.node_id == node_id)
            .map(|p| p.addr.clone())
            .ok_or_else(|| Status::not_found(format!("Unknown node {}", node_id)))?;

        // Create new connection with TLS if configured
        let channel = self.create_channel(&peer_addr).await?;
        let client = SchedulerServiceClient::new(channel);

        pool.insert(node_id, client.clone());
        Ok(client)
    }

    /// Get or create a cached internal service connection to a peer node
    async fn get_internal_client(
        &self,
        node_id: u64,
    ) -> Result<InternalServiceClient<Channel>, Status> {
        let mut pool = self.internal_pool.lock().await;

        if let Some(client) = pool.get(&node_id) {
            return Ok(client.clone());
        }

        let peer_addr = self
            .config
            .peers
            .iter()
            .find(|p| p.node_id == node_id)
            .map(|p| p.addr.clone())
            .ok_or_else(|| Status::not_found(format!("Unknown node {}", node_id)))?;

        // Create new connection with TLS if configured
        let channel = self.create_channel(&peer_addr).await?;
        let client = InternalServiceClient::new(channel);

        pool.insert(node_id, client.clone());
        Ok(client)
    }

    /// Fetch job output from the node that executed it
    async fn fetch_output_from_node(
        &self,
        node_id: u64,
        job_id: &Uuid,
    ) -> Result<(String, String), Status> {
        let mut client = self.get_internal_client(node_id).await?;

        let response = client
            .get_job_output(GetJobOutputRequest {
                job_id: job_id.to_string(),
            })
            .await
            .map_err(|e| Status::unavailable(format!("Failed to fetch output: {}", e)))?;

        let resp = response.into_inner();
        if resp.found {
            Ok((resp.output, resp.error))
        } else {
            Err(Status::not_found("Output not found on executing node"))
        }
    }
}

type JobStream = Pin<Box<dyn tokio_stream::Stream<Item = Result<JobInfo, Status>> + Send>>;

#[tonic::async_trait]
impl SchedulerService for ClientService {
    type StreamJobsStream = JobStream;

    async fn submit_job(
        &self,
        request: Request<SubmitJobRequest>,
    ) -> Result<Response<SubmitJobResponse>, Status> {
        let req = request.into_inner();

        // Validate command is not empty
        if req.command.trim().is_empty() {
            return Err(Status::invalid_argument("Command cannot be empty"));
        }

        // Check if we're the leader
        if !self.raft_node.is_leader().await {
            let leader = self.raft_node.get_leader_id().await;
            let message = match leader {
                Some(id) => format!("Not the leader. Redirect to node {}", id),
                None => "Not the leader. Leader unknown, retry later".to_string(),
            };
            return Err(Status::failed_precondition(message));
        }

        // Create a new job
        let job = Job::new(req.command.clone());
        let job_id = job.id;

        // Append to Raft log
        let command = Command::SubmitJob {
            job_id,
            command: req.command,
            created_at: job.created_at,
        };

        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .raft_node
            .message_sender()
            .send(crate::raft::node::RaftMessage::AppendCommand {
                command,
                response_tx: tx,
            })
            .await
            .is_err()
        {
            return Err(Status::internal("Failed to send command to Raft"));
        }

        match rx.await {
            Ok(Ok(_index)) => {
                let created_at_ms = job.created_at.timestamp_millis();
                // Add job to queue
                if !self.job_queue.write().await.add_job(job) {
                    return Err(Status::resource_exhausted("Job queue is at capacity"));
                }

                tracing::info!(job_id = %job_id, created_at_ms, "Job submitted");
                Ok(Response::new(SubmitJobResponse {
                    job_id: job_id.to_string(),
                    created_at_ms,
                }))
            }
            Ok(Err(e)) => Err(Status::internal(format!("Raft error: {}", e))),
            Err(_) => Err(Status::internal("Failed to receive Raft response")),
        }
    }

    async fn get_job_status(
        &self,
        request: Request<GetJobStatusRequest>,
    ) -> Result<Response<GetJobStatusResponse>, Status> {
        let req = request.into_inner();

        let job_id =
            Uuid::parse_str(&req.job_id).map_err(|_| Status::invalid_argument("Invalid job ID"))?;

        let queue = self.job_queue.read().await;
        let job = queue
            .get_job(&job_id)
            .ok_or_else(|| Status::not_found("Job not found"))?;

        // Build base response with metadata (available on all nodes via Raft)
        let mut response = GetJobStatusResponse {
            job_id: job.id.to_string(),
            status: status_to_proto(&job.status) as i32,
            output: String::new(),
            error: String::new(),
            assigned_worker: job.assigned_worker.unwrap_or(0),
            executed_by: job.executed_by.unwrap_or(0),
            exit_code: job.exit_code,
            created_at_ms: job.created_at.timestamp_millis(),
            completed_at_ms: job.completed_at.map(|dt| dt.timestamp_millis()),
        };

        // If this node executed the job, we have the output locally
        if job.executed_by == Some(self.config.node_id) {
            response.output = job.output.clone().unwrap_or_default();
            response.error = job.error.clone().unwrap_or_default();
            return Ok(Response::new(response));
        }

        // If another node executed it and job is completed/failed, fetch output
        if let Some(executed_by) = job.executed_by {
            if job.status == JobStatus::Completed || job.status == JobStatus::Failed {
                drop(queue); // Release lock before making network call

                if let Ok((output, error)) = self.fetch_output_from_node(executed_by, &job_id).await
                {
                    response.output = output;
                    response.error = error;
                }
                // If fetch fails, return response with empty output (best effort)
            }
        }

        Ok(Response::new(response))
    }

    async fn list_jobs(
        &self,
        request: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let req = request.into_inner();

        // Parse pagination parameters
        let page_size = if req.page_size == 0 {
            100 // Default page size
        } else {
            req.page_size.min(1000) // Max 1000
        } as usize;

        // Parse page token (offset-based: token is the starting index)
        let offset: usize = if req.page_token.is_empty() {
            0
        } else {
            req.page_token
                .parse()
                .map_err(|_| Status::invalid_argument("Invalid page token"))?
        };

        let queue = self.job_queue.read().await;
        let all_jobs = queue.all_jobs();
        let total_count = all_jobs.len() as u32;

        // Apply pagination
        let jobs: Vec<JobInfo> = all_jobs
            .into_iter()
            .skip(offset)
            .take(page_size)
            .map(|job| JobInfo {
                job_id: job.id.to_string(),
                command: job.command.clone(),
                status: status_to_proto(&job.status) as i32,
                assigned_worker: job.assigned_worker.unwrap_or(0),
                created_at_ms: job.created_at.timestamp_millis(),
                completed_at_ms: job.completed_at.map(|dt| dt.timestamp_millis()),
            })
            .collect();

        // Calculate next page token
        let next_offset = offset + jobs.len();
        let next_page_token = if next_offset < total_count as usize {
            next_offset.to_string()
        } else {
            String::new()
        };

        Ok(Response::new(ListJobsResponse {
            jobs,
            next_page_token,
            total_count,
        }))
    }

    async fn get_cluster_status(
        &self,
        _request: Request<GetClusterStatusRequest>,
    ) -> Result<Response<GetClusterStatusResponse>, Status> {
        // If we're not the leader, forward request to the leader
        if !self.raft_node.is_leader().await {
            return self.forward_cluster_status_to_leader().await;
        }

        // We're the leader - return authoritative cluster status
        let state = self.raft_node.state.read().await;
        let peers_status = self.raft_node.get_peers_status().await;

        // Build node list: current node + all peers
        let mut nodes = Vec::with_capacity(self.config.peers.len() + 1);

        // Add current node (leader is always alive)
        nodes.push(NodeInfo {
            node_id: self.config.node_id,
            address: self.config.listen_addr.to_string(),
            is_alive: true,
        });

        // Add all peer nodes with their health status from heartbeat responses
        for peer in &self.config.peers {
            let is_alive = peers_status.get(&peer.node_id).copied().unwrap_or(false);
            nodes.push(NodeInfo {
                node_id: peer.node_id,
                address: peer.addr.clone(),
                is_alive,
            });
        }

        // Sort by node_id for consistent ordering
        nodes.sort_by_key(|n| n.node_id);

        Ok(Response::new(GetClusterStatusResponse {
            current_term: state.current_term,
            leader_id: self.raft_node.id,
            nodes,
        }))
    }

    async fn stream_jobs(
        &self,
        request: Request<StreamJobsRequest>,
    ) -> Result<Response<Self::StreamJobsStream>, Status> {
        let req = request.into_inner();
        let status_filter = ProtoJobStatus::try_from(req.status_filter).ok();

        // Get all jobs from the queue
        let queue = self.job_queue.read().await;
        let jobs: Vec<Job> = queue.all_jobs().into_iter().cloned().collect();
        drop(queue); // Release lock before streaming

        // Create a channel for streaming
        let (tx, rx) = tokio::sync::mpsc::channel(32);

        // Spawn a task to send jobs through the channel
        tokio::spawn(async move {
            for job in jobs {
                let job_status = status_to_proto(&job.status);

                // Apply status filter if specified
                if let Some(filter) = status_filter {
                    if filter != ProtoJobStatus::Unspecified && job_status != filter {
                        continue;
                    }
                }

                let job_info = JobInfo {
                    job_id: job.id.to_string(),
                    command: job.command.clone(),
                    status: job_status as i32,
                    assigned_worker: job.assigned_worker.unwrap_or(0),
                    created_at_ms: job.created_at.timestamp_millis(),
                    completed_at_ms: job.completed_at.map(|dt| dt.timestamp_millis()),
                };

                if tx.send(Ok(job_info)).await.is_err() {
                    // Client disconnected
                    break;
                }
            }
        });

        let stream = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream) as Self::StreamJobsStream))
    }

    async fn get_raft_log_entries(
        &self,
        request: Request<GetRaftLogEntriesRequest>,
    ) -> Result<Response<GetRaftLogEntriesResponse>, Status> {
        // Forward to leader if not the leader
        if !self.raft_node.is_leader().await {
            return self.forward_raft_log_entries_to_leader(request).await;
        }

        let req = request.into_inner();

        // Parse pagination parameters
        let limit = if req.limit == 0 {
            100 // Default limit
        } else {
            req.limit.min(1000) // Max 1000
        } as usize;

        let state = self.raft_node.state.read().await;
        let commit_index = state.commit_index;
        let last_log_index = state.last_log_index();

        // Determine start index (1-indexed, 0 means from beginning)
        let start_index = if req.start_index == 0 {
            1
        } else {
            req.start_index
        };

        // Convert 1-indexed start_index to 0-indexed array position
        // Log entries are 1-indexed, so entry at index N is at position N-1
        let start_pos = if start_index == 0 {
            0
        } else {
            (start_index - 1) as usize
        };

        // Efficiently slice the log directly instead of filtering O(n)
        let entries: Vec<RaftLogEntryInfo> = if start_pos >= state.log.len() {
            Vec::new()
        } else {
            let end_pos = (start_pos + limit).min(state.log.len());
            state.log[start_pos..end_pos]
                .iter()
                .map(|entry| {
                    let proto_entry = log_entry_to_proto(entry);
                    RaftLogEntryInfo {
                        index: entry.index,
                        term: entry.term,
                        command: proto_entry.command,
                        is_committed: entry.index <= commit_index,
                    }
                })
                .collect()
        };

        Ok(Response::new(GetRaftLogEntriesResponse {
            entries,
            commit_index,
            last_log_index,
        }))
    }
}

impl ClientService {
    /// Forward cluster status request to the current leader
    async fn forward_cluster_status_to_leader(
        &self,
    ) -> Result<Response<GetClusterStatusResponse>, Status> {
        let leader_id = self
            .raft_node
            .get_leader_id()
            .await
            .ok_or_else(|| Status::unavailable("Leader unknown, please retry later"))?;

        let mut client = self.get_client(leader_id).await?;

        let response = client
            .get_cluster_status(GetClusterStatusRequest {})
            .await
            .map_err(|e| Status::unavailable(format!("Leader request failed: {}", e)))?;

        Ok(response)
    }

    /// Forward raft log entries request to the current leader
    async fn forward_raft_log_entries_to_leader(
        &self,
        request: Request<GetRaftLogEntriesRequest>,
    ) -> Result<Response<GetRaftLogEntriesResponse>, Status> {
        let leader_id = self
            .raft_node
            .get_leader_id()
            .await
            .ok_or_else(|| Status::unavailable("Leader unknown, please retry later"))?;

        let mut client = self.get_client(leader_id).await?;

        let response = client
            .get_raft_log_entries(request.into_inner())
            .await
            .map_err(|e| Status::unavailable(format!("Leader request failed: {}", e)))?;

        Ok(response)
    }
}

fn status_to_proto(status: &JobStatus) -> ProtoJobStatus {
    match status {
        JobStatus::Pending => ProtoJobStatus::Pending,
        JobStatus::Running => ProtoJobStatus::Running,
        JobStatus::Completed => ProtoJobStatus::Completed,
        JobStatus::Failed => ProtoJobStatus::Failed,
    }
}
