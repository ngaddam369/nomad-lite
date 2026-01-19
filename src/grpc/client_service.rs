use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::config::NodeConfig;
use crate::proto::scheduler_service_server::SchedulerService;
use crate::proto::{
    GetClusterStatusRequest, GetClusterStatusResponse, GetJobStatusRequest, GetJobStatusResponse,
    JobInfo, JobStatus as ProtoJobStatus, ListJobsRequest, ListJobsResponse, NodeInfo,
    SubmitJobRequest, SubmitJobResponse,
};
use crate::raft::{Command, RaftNode};
use crate::scheduler::{Job, JobQueue, JobStatus};

/// gRPC service for client-facing API
pub struct ClientService {
    config: NodeConfig,
    raft_node: Arc<RaftNode>,
    job_queue: Arc<RwLock<JobQueue>>,
}

impl ClientService {
    pub fn new(
        config: NodeConfig,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
    ) -> Self {
        Self {
            config,
            raft_node,
            job_queue,
        }
    }
}

#[tonic::async_trait]
impl SchedulerService for ClientService {
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
                // Add job to queue
                if !self.job_queue.write().await.add_job(job) {
                    return Err(Status::resource_exhausted("Job queue is at capacity"));
                }

                tracing::info!(job_id = %job_id, "Job submitted");
                Ok(Response::new(SubmitJobResponse {
                    job_id: job_id.to_string(),
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
        match queue.get_job(&job_id) {
            Some(job) => Ok(Response::new(GetJobStatusResponse {
                job_id: job.id.to_string(),
                status: status_to_proto(&job.status) as i32,
                output: job.output.clone().unwrap_or_default(),
                error: job.error.clone().unwrap_or_default(),
                assigned_worker: job.assigned_worker.unwrap_or(0),
            })),
            None => Err(Status::not_found("Job not found")),
        }
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
        let state = self.raft_node.state.read().await;

        // Build node list: current node + all peers
        let mut nodes = Vec::with_capacity(self.config.peers.len() + 1);

        // Add current node
        nodes.push(NodeInfo {
            node_id: self.config.node_id,
            address: self.config.listen_addr.to_string(),
            is_alive: true,
        });

        // Add all peer nodes
        for peer in &self.config.peers {
            nodes.push(NodeInfo {
                node_id: peer.node_id,
                address: peer.addr.clone(),
                is_alive: true, // TODO: Track actual peer health
            });
        }

        // Sort by node_id for consistent ordering
        nodes.sort_by_key(|n| n.node_id);

        Ok(Response::new(GetClusterStatusResponse {
            node_id: self.raft_node.id,
            role: state.role.to_string(),
            current_term: state.current_term,
            leader_id: state.leader_id,
            nodes,
        }))
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
