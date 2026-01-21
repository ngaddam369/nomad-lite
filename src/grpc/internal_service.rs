use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::proto::internal_service_server::InternalService;
use crate::proto::{GetJobOutputRequest, GetJobOutputResponse};
use crate::scheduler::JobQueue;

/// Internal gRPC service for node-to-node communication.
/// Used to fetch job output from the node that executed the job.
pub struct InternalServiceImpl {
    job_queue: Arc<RwLock<JobQueue>>,
    node_id: u64,
}

impl InternalServiceImpl {
    pub fn new(job_queue: Arc<RwLock<JobQueue>>, node_id: u64) -> Self {
        Self { job_queue, node_id }
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
}
