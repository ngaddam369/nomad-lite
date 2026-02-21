use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::proto::internal_service_server::InternalService;
use crate::proto::{
    ForwardJobStatusRequest, ForwardJobStatusResponse, GetJobOutputRequest, GetJobOutputResponse,
    WorkerHeartbeatRequest, WorkerHeartbeatResponse,
};
use crate::raft::node::RaftMessage;
use crate::raft::state::JobStatusUpdate;
use crate::raft::{Command, RaftNode};
use crate::scheduler::assigner::JobAssigner;
use crate::scheduler::{JobQueue, JobStatus};

/// Internal gRPC service for node-to-node communication.
/// Used to fetch job output from the node that executed the job,
/// and to receive worker heartbeats so the leader's assigner tracks live workers.
pub struct InternalServiceImpl {
    job_queue: Arc<RwLock<JobQueue>>,
    job_assigner: Arc<RwLock<JobAssigner>>,
    node_id: u64,
    /// Raft node for proposing commands. None in unit-test contexts.
    raft_node: Option<Arc<RaftNode>>,
}

impl InternalServiceImpl {
    pub fn new(
        job_queue: Arc<RwLock<JobQueue>>,
        job_assigner: Arc<RwLock<JobAssigner>>,
        node_id: u64,
        raft_node: Option<Arc<RaftNode>>,
    ) -> Self {
        Self {
            job_queue,
            job_assigner,
            node_id,
            raft_node,
        }
    }
}

#[tonic::async_trait]
impl InternalService for InternalServiceImpl {
    async fn get_job_output(
        &self,
        request: Request<GetJobOutputRequest>,
    ) -> Result<Response<GetJobOutputResponse>, Status> {
        let req = request.into_inner();
        let job_id =
            Uuid::parse_str(&req.job_id).map_err(|_| Status::invalid_argument("Invalid job ID"))?;

        let queue = self.job_queue.read().await;
        match queue.get_job(&job_id) {
            Some(job) if job.executed_by == Some(self.node_id) => {
                // This node executed the job - return the output
                Ok(Response::new(GetJobOutputResponse {
                    job_id: job.id.to_string(),
                    output: job.output.clone().unwrap_or_default(),
                    error: job.error.clone().unwrap_or_default(),
                    found: true,
                }))
            }
            Some(_) => {
                // Job exists but wasn't executed by this node
                Ok(Response::new(GetJobOutputResponse {
                    job_id: req.job_id,
                    output: String::new(),
                    error: String::new(),
                    found: false,
                }))
            }
            None => Err(Status::not_found("Job not found")),
        }
    }

    async fn worker_heartbeat(
        &self,
        request: Request<WorkerHeartbeatRequest>,
    ) -> Result<Response<WorkerHeartbeatResponse>, Status> {
        let node_id = request.into_inner().node_id;
        self.job_assigner.write().await.worker_heartbeat(node_id);
        Ok(Response::new(WorkerHeartbeatResponse {}))
    }

    async fn forward_job_status(
        &self,
        request: Request<ForwardJobStatusRequest>,
    ) -> Result<Response<ForwardJobStatusResponse>, Status> {
        let Some(ref raft_node) = self.raft_node else {
            return Err(Status::unavailable("Raft not available"));
        };

        let updates_proto = request.into_inner().updates;
        if updates_proto.is_empty() {
            return Ok(Response::new(ForwardJobStatusResponse {}));
        }

        let updates: Vec<JobStatusUpdate> = updates_proto
            .into_iter()
            .filter_map(|u| {
                let job_id = Uuid::parse_str(&u.job_id).ok()?;
                let status = proto_status_to_internal(
                    crate::proto::JobStatus::try_from(u.status)
                        .unwrap_or(crate::proto::JobStatus::Unspecified),
                );
                let completed_at = u.completed_at_ms.map(|ms| {
                    chrono::TimeZone::timestamp_millis_opt(&chrono::Utc, ms)
                        .single()
                        .unwrap_or_default()
                });
                Some(JobStatusUpdate {
                    job_id,
                    status,
                    executed_by: u.executed_by,
                    exit_code: u.exit_code,
                    completed_at,
                })
            })
            .collect();

        let command = if updates.len() == 1 {
            let u = updates.into_iter().next().unwrap();
            Command::UpdateJobStatus {
                job_id: u.job_id,
                status: u.status,
                executed_by: u.executed_by,
                exit_code: u.exit_code,
                completed_at: u.completed_at,
            }
        } else {
            Command::BatchUpdateJobStatus { updates }
        };

        let (tx, _rx) = tokio::sync::oneshot::channel();
        if let Err(e) = raft_node
            .message_sender()
            .send(RaftMessage::AppendCommand {
                command,
                response_tx: tx,
            })
            .await
        {
            tracing::warn!(error = %e, "Failed to forward job status to Raft");
            return Err(Status::internal("Failed to propose status update"));
        }

        Ok(Response::new(ForwardJobStatusResponse {}))
    }
}

fn proto_status_to_internal(status: crate::proto::JobStatus) -> JobStatus {
    match status {
        crate::proto::JobStatus::Pending | crate::proto::JobStatus::Unspecified => {
            JobStatus::Pending
        }
        crate::proto::JobStatus::Running => JobStatus::Running,
        crate::proto::JobStatus::Completed => JobStatus::Completed,
        crate::proto::JobStatus::Failed => JobStatus::Failed,
    }
}
