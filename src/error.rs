use thiserror::Error;

#[derive(Error, Debug)]
pub enum NomadError {
    #[error("Not the leader, current leader is node {0:?}")]
    NotLeader(Option<u64>),

    #[error("Job not found: {0}")]
    JobNotFound(String),

    #[error("Worker not found: {0}")]
    WorkerNotFound(u64),

    #[error("No workers available")]
    NoWorkersAvailable,

    #[error("Raft error: {0}")]
    RaftError(String),

    #[error("gRPC error: {0}")]
    GrpcError(#[from] tonic::Status),

    #[error("Transport error: {0}")]
    TransportError(#[from] tonic::transport::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, NomadError>;
