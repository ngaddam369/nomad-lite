use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
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
use crate::tls::TlsIdentity;

pub struct GrpcServer {
    addr: SocketAddr,
    config: NodeConfig,
    raft_node: Arc<RaftNode>,
    job_queue: Arc<RwLock<JobQueue>>,
    tls_identity: Option<TlsIdentity>,
    draining: Arc<AtomicBool>,
}

impl GrpcServer {
    pub fn new(
        addr: SocketAddr,
        config: NodeConfig,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
        tls_identity: Option<TlsIdentity>,
        draining: Arc<AtomicBool>,
    ) -> Self {
        Self {
            addr,
            config,
            raft_node,
            job_queue,
            tls_identity,
            draining,
        }
    }

    pub async fn run(
        self,
        shutdown_token: CancellationToken,
    ) -> Result<(), tonic::transport::Error> {
        let cluster_service = ClusterService::new(self.raft_node.clone());
        let client_service = ClientService::new(
            self.config.clone(),
            self.raft_node.clone(),
            self.job_queue.clone(),
            self.tls_identity.clone(),
            self.draining.clone(),
        );
        let internal_service =
            InternalServiceImpl::new(self.job_queue.clone(), self.config.node_id);

        // Build server with optional TLS
        let mut builder = Server::builder();

        if let Some(ref tls_identity) = self.tls_identity {
            let tls_config = tls_identity.server_tls_config();
            builder = builder.tls_config(tls_config)?;
            tracing::info!(addr = %self.addr, "Starting gRPC server with mTLS");
        } else {
            tracing::warn!(
                addr = %self.addr,
                "Starting gRPC server WITHOUT TLS - not recommended for production"
            );
        }

        builder
            .add_service(RaftServiceServer::new(cluster_service))
            .add_service(SchedulerServiceServer::new(client_service))
            .add_service(InternalServiceServer::new(internal_service))
            .serve_with_shutdown(self.addr, shutdown_token.cancelled())
            .await
    }
}
