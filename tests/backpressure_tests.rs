//! Integration tests for proposal backpressure.
//!
//! These tests validate that:
//! - When the Raft proposal channel is full, SubmitJob immediately returns
//!   RESOURCE_EXHAUSTED rather than blocking indefinitely.
//! - When the Raft loop stalls (no quorum), SubmitJob returns DEADLINE_EXCEEDED
//!   after 5 seconds rather than hanging forever.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tonic::{Code, Request};

use nomad_lite::{
    config::NodeConfig,
    grpc::client_service::ClientService,
    proto::{scheduler_service_server::SchedulerService, SubmitJobRequest},
    raft::{
        node::{RaftMessage, RaftNode},
        state::RaftRole,
        Command,
    },
    scheduler::JobQueue,
};

/// Helper: create a stopped RaftNode (receiver never consumed) set to leader role.
async fn make_idle_leader() -> (Arc<RaftNode>, tokio::sync::mpsc::Receiver<RaftMessage>) {
    let config = NodeConfig::new(1, "127.0.0.1:0".parse().unwrap());
    let (node, rx) = RaftNode::new(config, None);
    {
        let mut state = node.state.write().await;
        state.role = RaftRole::Leader;
    }
    (Arc::new(node), rx)
}

/// Fill the proposal channel to capacity; return the number of slots filled.
fn fill_channel(node: &RaftNode) -> usize {
    let sender = node.message_sender();
    let mut count = 0usize;
    loop {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        match sender.try_send(RaftMessage::AppendCommand {
            command: Command::SubmitJob {
                job_id: uuid::Uuid::new_v4(),
                command: "fill".to_string(),
                created_at: chrono::Utc::now(),
            },
            response_tx: tx,
        }) {
            Ok(()) => count += 1,
            Err(_) => break,
        }
    }
    count
}

fn make_service(config: NodeConfig, raft_node: Arc<RaftNode>) -> ClientService {
    let job_queue = Arc::new(RwLock::new(JobQueue::new()));
    let draining = Arc::new(AtomicBool::new(false));
    ClientService::new(config, raft_node, job_queue, None, draining)
}

// ---------------------------------------------------------------------------
// Test 1: full channel → RESOURCE_EXHAUSTED (no blocking)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_resource_exhausted_when_channel_full() {
    let (raft_node, _rx) = make_idle_leader().await;

    let filled = fill_channel(&raft_node);
    assert_eq!(filled, 256, "channel capacity should be exactly 256");

    let config = NodeConfig::new(1, "127.0.0.1:0".parse().unwrap());
    let service = make_service(config, raft_node);

    let start = std::time::Instant::now();
    let result = service
        .submit_job(Request::new(SubmitJobRequest {
            command: "echo overload".to_string(),
        }))
        .await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "should have been rejected");
    assert_eq!(
        result.unwrap_err().code(),
        Code::ResourceExhausted,
        "should be RESOURCE_EXHAUSTED"
    );
    // Must complete nearly instantly — not block until the channel drains
    assert!(
        elapsed < Duration::from_millis(200),
        "try_send must be non-blocking (took {:?})",
        elapsed
    );
}

// ---------------------------------------------------------------------------
// Test 2: Raft loop stalled (no quorum) → DEADLINE_EXCEEDED after 5 s
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_deadline_exceeded_when_raft_stalled() {
    // Create the node; keep _rx alive so the channel stays open (Closed variant
    // would surface immediately as UNAVAILABLE, which is not what we test here).
    let (raft_node, _rx) = make_idle_leader().await;

    let config = NodeConfig::new(1, "127.0.0.1:0".parse().unwrap());
    let service = make_service(config, raft_node);

    let start = std::time::Instant::now();
    let result = service
        .submit_job(Request::new(SubmitJobRequest {
            command: "echo stalled".to_string(),
        }))
        .await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "should have timed out");
    assert_eq!(
        result.unwrap_err().code(),
        Code::DeadlineExceeded,
        "should be DEADLINE_EXCEEDED"
    );
    // Should time out in ~5 s, not hang forever
    assert!(
        elapsed >= Duration::from_secs(4),
        "should have waited ~5 s (took {:?})",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "should not wait much longer than 5 s (took {:?})",
        elapsed
    );
}
