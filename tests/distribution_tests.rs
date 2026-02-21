//! Tests for distributed job scheduling across multiple nodes.
//!
//! Verifies that:
//! - Jobs are spread across nodes, not all executed on the leader
//! - The WorkerHeartbeat RPC correctly updates the leader's assigner
//! - Dead workers (stopped heartbeats) are not assigned new jobs

mod test_harness;

use std::time::Duration;
use test_harness::{assert_eventually, TestCluster};

/// Submit several jobs to a 3-node cluster and verify that they end up
/// assigned to different workers.
///
/// The test checks `assigned_worker` (set when AssignJob commits), which is
/// visible in the leader's job queue after the Raft entries are applied.
#[tokio::test]
async fn test_jobs_distributed_across_nodes() {
    let base_port = 19300u16;
    let mut cluster = TestCluster::new(3, base_port).await;

    // Wait for a leader to be elected
    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("No leader elected within timeout");

    // Register all nodes as workers via Raft so the leader's assigner knows about them.
    // All commands must go through the leader — followers reject AppendCommand.
    let leader_node = cluster.get_node(leader_id).unwrap().raft_node.clone();
    for node_id in cluster.active_node_ids() {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let _ = leader_node
            .message_sender()
            .send(nomad_lite::raft::node::RaftMessage::AppendCommand {
                command: nomad_lite::raft::state::Command::RegisterWorker { worker_id: node_id },
                response_tx: tx,
            })
            .await;
    }

    // Give registrations time to commit
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Submit 9 jobs (ideally 3 per node)
    const NUM_JOBS: usize = 9;
    for i in 0..NUM_JOBS {
        cluster
            .submit_job(&format!("echo job-{}", i))
            .await
            .expect("Failed to submit job");
    }

    // Wait until all NUM_JOBS jobs have been assigned (semantically what matters).
    // We avoid relying on a specific Raft entry count because one registration
    // command may occasionally be lost if the leader briefly steps down before
    // processing it; the scheduler still assigns all jobs to however many workers
    // registered successfully.
    assert_eventually(
        || {
            let cluster_ref = &cluster;
            async move {
                if let Some(leader) = cluster_ref.get_node(leader_id) {
                    let queue = leader.job_queue.read().await;
                    let all_jobs = queue.all_jobs();
                    all_jobs.len() == NUM_JOBS
                        && all_jobs.iter().all(|j| j.assigned_worker.is_some())
                } else {
                    false
                }
            }
        },
        Duration::from_secs(10),
        "All jobs should be assigned to workers within timeout",
    )
    .await;

    // Check assigned workers in the leader's job queue
    let assigned_workers: std::collections::HashSet<u64> = {
        let leader = cluster.get_node(leader_id).unwrap();
        let queue = leader.job_queue.read().await;
        queue
            .all_jobs()
            .iter()
            .filter_map(|j| j.assigned_worker)
            .collect()
    };

    assert!(
        assigned_workers.len() > 1,
        "Expected jobs distributed across multiple workers, but all went to: {:?}",
        assigned_workers
    );

    cluster.shutdown().await;
}

/// Call WorkerHeartbeat on a live node and verify the node's assigner
/// reflects the updated liveness.
#[tokio::test]
async fn test_worker_heartbeat_rpc() {
    use nomad_lite::proto::internal_service_client::InternalServiceClient;
    use nomad_lite::proto::WorkerHeartbeatRequest;

    let base_port = 19310u16;
    let mut cluster = TestCluster::new(3, base_port).await;

    // Wait for a leader
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("No leader elected");

    // Send a WorkerHeartbeat to node 1's InternalService
    let node1_addr = format!("http://127.0.0.1:{}", base_port);
    let mut client = InternalServiceClient::connect(node1_addr)
        .await
        .expect("Failed to connect to node 1 internal service");

    let resp = client
        .worker_heartbeat(WorkerHeartbeatRequest { node_id: 42 })
        .await;

    assert!(resp.is_ok(), "WorkerHeartbeat RPC failed: {:?}", resp.err());

    // Verify that node 1's assigner now knows about worker 42
    let alive_workers: Vec<u64> = {
        let node1 = cluster.get_node(1).unwrap();
        let assigner = node1.job_assigner.read().await;
        assigner.available_workers()
    };

    assert!(
        alive_workers.contains(&42),
        "Worker 42 should be alive in node 1's assigner after heartbeat, got: {:?}",
        alive_workers
    );

    cluster.shutdown().await;
}

/// Verify that a worker whose heartbeats have stopped is not assigned new jobs.
///
/// Strategy: register workers 100, 200, 300 in the leader's assigner directly
/// (bypassing Raft), then let the 5 s liveness window expire, then send one
/// heartbeat for worker 100, submit a job, and assert it goes to worker 100.
#[tokio::test]
async fn test_dead_worker_not_assigned() {
    use nomad_lite::proto::internal_service_client::InternalServiceClient;
    use nomad_lite::proto::WorkerHeartbeatRequest;

    let base_port = 19320u16;
    let mut cluster = TestCluster::new(3, base_port).await;

    // Wait for a leader
    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("No leader elected");

    // Manually register workers 100, 200, 300 in the leader's assigner
    {
        let leader = cluster.get_node(leader_id).unwrap();
        let mut assigner = leader.job_assigner.write().await;
        assigner.register_worker(100);
        assigner.register_worker(200);
        assigner.register_worker(300);
    }

    // Wait for the 5 s liveness window to expire (no heartbeats sent)
    tokio::time::sleep(Duration::from_secs(6)).await;

    // Revive only worker 100 via a heartbeat to the leader's InternalService
    let leader_port = base_port + (leader_id - 1) as u16;
    let leader_addr = format!("http://127.0.0.1:{}", leader_port);
    let mut client = InternalServiceClient::connect(leader_addr)
        .await
        .expect("Failed to connect to leader");
    client
        .worker_heartbeat(WorkerHeartbeatRequest { node_id: 100 })
        .await
        .expect("Heartbeat failed");

    // Submit a job — it should only go to worker 100 (the only live one)
    let job_id = cluster
        .submit_job("echo assigned-to-live")
        .await
        .expect("Failed to submit job");

    // Wait for the assignment to commit on the leader
    assert_eventually(
        || {
            let cluster_ref = &cluster;
            async move {
                let leader = cluster_ref.get_node(leader_id).unwrap();
                let queue = leader.job_queue.read().await;
                queue
                    .get_job(&job_id)
                    .map(|j| j.assigned_worker == Some(100))
                    .unwrap_or(false)
            }
        },
        Duration::from_secs(5),
        "Job should have been assigned to live worker 100",
    )
    .await;

    cluster.shutdown().await;
}
