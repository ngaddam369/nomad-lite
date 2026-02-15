use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use tonic::{Request, Response, Status};

use crate::proto::raft_service_server::RaftService;
use crate::proto::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    TimeoutNowRequest, TimeoutNowResponse, VoteRequest, VoteResponse,
};
use crate::raft::RaftNode;

/// gRPC service for internal Raft communication
pub struct ClusterService {
    raft_node: Arc<RaftNode>,
}

impl ClusterService {
    pub fn new(raft_node: Arc<RaftNode>) -> Self {
        Self { raft_node }
    }
}

#[tonic::async_trait]
impl RaftService for ClusterService {
    async fn request_vote(
        &self,
        request: Request<VoteRequest>,
    ) -> Result<Response<VoteResponse>, Status> {
        let req = request.into_inner();
        tracing::debug!(
            candidate = req.candidate_id,
            term = req.term,
            "Received RequestVote"
        );

        let node = self.raft_node.clone();
        let result = AssertUnwindSafe(async { node.handle_vote_request(req).await })
            .catch_unwind()
            .await;

        match result {
            Ok(Ok(response)) => Ok(Response::new(response)),
            Ok(Err(e)) => Err(Status::internal(format!("RequestVote handler error: {e}"))),
            Err(_) => {
                tracing::error!("Panic in RequestVote handler");
                Err(Status::internal("Internal error in RequestVote handler"))
            }
        }
    }

    async fn append_entries(
        &self,
        request: Request<AppendEntriesRequest>,
    ) -> Result<Response<AppendEntriesResponse>, Status> {
        let req = request.into_inner();
        let is_heartbeat = req.entries.is_empty();
        tracing::trace!(
            leader = req.leader_id,
            term = req.term,
            entries = req.entries.len(),
            is_heartbeat,
            "Received AppendEntries"
        );

        let node = self.raft_node.clone();
        let result = AssertUnwindSafe(async { node.handle_append_entries(req).await })
            .catch_unwind()
            .await;

        match result {
            Ok(Ok(response)) => Ok(Response::new(response)),
            Ok(Err(e)) => Err(Status::internal(format!(
                "AppendEntries handler error: {e}"
            ))),
            Err(_) => {
                tracing::error!("Panic in AppendEntries handler");
                Err(Status::internal("Internal error in AppendEntries handler"))
            }
        }
    }

    async fn timeout_now(
        &self,
        request: Request<TimeoutNowRequest>,
    ) -> Result<Response<TimeoutNowResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(
            leader_id = req.leader_id,
            term = req.term,
            "Received TimeoutNow"
        );

        let node = self.raft_node.clone();
        let result = AssertUnwindSafe(async { node.handle_timeout_now(req).await })
            .catch_unwind()
            .await;

        match result {
            Ok(Ok(response)) => Ok(Response::new(response)),
            Ok(Err(e)) => Err(Status::internal(format!("TimeoutNow handler error: {e}"))),
            Err(_) => {
                tracing::error!("Panic in TimeoutNow handler");
                Err(Status::internal("Internal error in TimeoutNow handler"))
            }
        }
    }

    async fn install_snapshot(
        &self,
        request: Request<InstallSnapshotRequest>,
    ) -> Result<Response<InstallSnapshotResponse>, Status> {
        let req = request.into_inner();
        tracing::info!(
            leader_id = req.leader_id,
            term = req.term,
            last_included_index = req.last_included_index,
            "Received InstallSnapshot"
        );

        let node = self.raft_node.clone();
        let result = AssertUnwindSafe(async { node.handle_install_snapshot(req).await })
            .catch_unwind()
            .await;

        match result {
            Ok(Ok(response)) => Ok(Response::new(response)),
            Ok(Err(e)) => Err(Status::internal(format!(
                "InstallSnapshot handler error: {e}"
            ))),
            Err(_) => {
                tracing::error!("Panic in InstallSnapshot handler");
                Err(Status::internal(
                    "Internal error in InstallSnapshot handler",
                ))
            }
        }
    }
}
