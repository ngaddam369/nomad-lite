use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::transport::Server;

use crate::grpc::client_service::ClientService;
use crate::grpc::cluster_service::ClusterService;
use crate::proto::raft_service_server::RaftServiceServer;
use crate::proto::scheduler_service_server::SchedulerServiceServer;
use crate::raft::RaftNode;
use crate::scheduler::JobQueue;

pub struct GrpcServer {
    addr: SocketAddr,
    raft_node: Arc<RaftNode>,
    job_queue: Arc<RwLock<JobQueue>>,
}

impl GrpcServer {
    pub fn new(
        addr: SocketAddr,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
    ) -> Self {
        Self {
            addr,
            raft_node,
            job_queue,
        }
    }

    pub async fn run(self) -> Result<(), tonic::transport::Error> {
        let cluster_service = ClusterService::new(self.raft_node.clone());
        let client_service = ClientService::new(self.raft_node.clone(), self.job_queue.clone());

        tracing::info!(addr = %self.addr, "Starting gRPC server");

        Server::builder()
            .add_service(RaftServiceServer::new(cluster_service))
            .add_service(SchedulerServiceServer::new(client_service))
            .serve(self.addr)
            .await
    }
}
