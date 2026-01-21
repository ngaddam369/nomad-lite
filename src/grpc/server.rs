use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::transport::Server;

use crate::config::NodeConfig;
use crate::grpc::client_service::ClientService;
use crate::grpc::cluster_service::ClusterService;
use crate::grpc::internal_service::InternalServiceImpl;
use crate::proto::internal_service_server::InternalServiceServer;
use crate::proto::raft_service_server::RaftServiceServer;
use crate::proto::scheduler_service_server::SchedulerServiceServer;
use crate::raft::RaftNode;
use crate::scheduler::JobQueue;

pub struct GrpcServer {
    addr: SocketAddr,
    config: NodeConfig,
    raft_node: Arc<RaftNode>,
    job_queue: Arc<RwLock<JobQueue>>,
}

impl GrpcServer {
    pub fn new(
        addr: SocketAddr,
        config: NodeConfig,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
    ) -> Self {
        Self {
            addr,
            config,
            raft_node,
            job_queue,
        }
    }

    pub async fn run(self) -> Result<(), tonic::transport::Error> {
        let cluster_service = ClusterService::new(self.raft_node.clone());
        let client_service = ClientService::new(
            self.config.clone(),
            self.raft_node.clone(),
            self.job_queue.clone(),
        );
        let internal_service =
            InternalServiceImpl::new(self.job_queue.clone(), self.config.node_id);

        tracing::info!(addr = %self.addr, "Starting gRPC server");

        Server::builder()
            .add_service(RaftServiceServer::new(cluster_service))
            .add_service(SchedulerServiceServer::new(client_service))
            .add_service(InternalServiceServer::new(internal_service))
            .serve(self.addr)
            .await
    }
}
