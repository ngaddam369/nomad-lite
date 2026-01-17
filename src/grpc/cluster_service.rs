use std::sync::Arc;
use tonic::{Request, Response, Status};

use crate::proto::raft_service_server::RaftService;
use crate::proto::{AppendEntriesRequest, AppendEntriesResponse, VoteRequest, VoteResponse};
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

        let response = self.raft_node.handle_vote_request(req).await;
        Ok(Response::new(response))
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

        let response = self.raft_node.handle_append_entries(req).await;
        Ok(Response::new(response))
    }
}
