use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use uuid::Uuid;

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
    raft_node: Arc<RaftNode>,
    job_queue: Arc<RwLock<JobQueue>>,
}

impl ClientService {
    pub fn new(raft_node: Arc<RaftNode>, job_queue: Arc<RwLock<JobQueue>>) -> Self {
        Self {
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

        // Check if we're the leader
        if !self.raft_node.is_leader().await {
            let leader = self.raft_node.get_leader_id().await;
            return Ok(Response::new(SubmitJobResponse {
                job_id: String::new(),
                accepted: false,
                error: format!(
                    "Not the leader. Current leader: {}",
                    leader.map(|l| l.to_string()).unwrap_or_else(|| "unknown".to_string())
                ),
            }));
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
            return Ok(Response::new(SubmitJobResponse {
                job_id: String::new(),
                accepted: false,
                error: "Failed to send command to Raft".to_string(),
            }));
        }

        match rx.await {
            Ok(Ok(_index)) => {
                // Add job to queue
                self.job_queue.write().await.add_job(job);

                tracing::info!(job_id = %job_id, "Job submitted");
                Ok(Response::new(SubmitJobResponse {
                    job_id: job_id.to_string(),
                    accepted: true,
                    error: String::new(),
                }))
            }
            Ok(Err(e)) => Ok(Response::new(SubmitJobResponse {
                job_id: String::new(),
                accepted: false,
                error: e,
            })),
            Err(_) => Ok(Response::new(SubmitJobResponse {
                job_id: String::new(),
                accepted: false,
                error: "Failed to receive Raft response".to_string(),
            })),
        }
    }

    async fn get_job_status(
        &self,
        request: Request<GetJobStatusRequest>,
    ) -> Result<Response<GetJobStatusResponse>, Status> {
        let req = request.into_inner();

        let job_id = Uuid::parse_str(&req.job_id)
            .map_err(|_| Status::invalid_argument("Invalid job ID"))?;

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
        _request: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let queue = self.job_queue.read().await;
        let jobs: Vec<JobInfo> = queue
            .all_jobs()
            .into_iter()
            .map(|job| JobInfo {
                job_id: job.id.to_string(),
                command: job.command.clone(),
                status: status_to_proto(&job.status) as i32,
                assigned_worker: job.assigned_worker.unwrap_or(0),
            })
            .collect();

        Ok(Response::new(ListJobsResponse { jobs }))
    }

    async fn get_cluster_status(
        &self,
        _request: Request<GetClusterStatusRequest>,
    ) -> Result<Response<GetClusterStatusResponse>, Status> {
        let state = self.raft_node.state.read().await;

        Ok(Response::new(GetClusterStatusResponse {
            node_id: self.raft_node.id,
            role: state.role.to_string(),
            current_term: state.current_term,
            leader_id: state.leader_id,
            nodes: vec![NodeInfo {
                node_id: self.raft_node.id,
                address: String::new(), // Would need config access
                is_alive: true,
            }],
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
