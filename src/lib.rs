pub mod config;
pub mod dashboard;
pub mod error;
pub mod grpc;
pub mod node;
pub mod raft;
pub mod scheduler;
pub mod worker;

// Re-export generated protobuf types
pub mod proto {
    tonic::include_proto!("scheduler");
}
