//! Integration tests for job cancellation across a multi-node Raft cluster.

mod test_harness;

use std::time::Duration;
use test_harness::{assert_eventually, TestCluster};
use tokio::sync::oneshot;
use uuid::Uuid;

use nomad_lite::raft::node::RaftMessage;
use nomad_lite::raft::state::Command;
use nomad_lite::scheduler::JobStatus;

/// Helper: send a CancelJob command to the cluster leader and wait for commit.
async fn cancel_job(cluster: &TestCluster, job_id: Uuid) -> Result<(), String> {
    let leader_id = cluster.get_leader_id().await.ok_or("No leader elected")?;
    let leader = cluster.get_node(leader_id).ok_or("Leader node not found")?;

    let (tx, rx) = oneshot::channel();
    leader
        .raft_node
        .message_sender()
        .send(RaftMessage::AppendCommand {
            command: Command::CancelJob { job_id },
            response_tx: tx,
        })
        .await
        .map_err(|e| format!("Failed to send command: {}", e))?;

    rx.await
        .map_err(|e| format!("Failed to receive response: {}", e))?
        .map(|_| ())
}

/// Cancel a pending job and verify it is replicated as Cancelled on all nodes.
#[tokio::test]
async fn test_cancel_pending_job_replicated_to_all_nodes() {
    let mut cluster = TestCluster::new(3, 50900).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let job_id = cluster
        .submit_job("echo cancellation-test")
        .await
        .expect("Job submission should succeed");

    // Wait for the job to appear on all nodes
    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let queue = node.job_queue.read().await;
                if queue.get_job(&job_id).is_none() {
                    return false;
                }
            }
            true
        },
        Duration::from_secs(3),
        "Job should be replicated to all nodes",
    )
    .await;

    // Cancel the job
    cancel_job(&cluster, job_id)
        .await
        .expect("CancelJob should be committed");

    // All nodes should eventually reflect Cancelled status
    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let queue = node.job_queue.read().await;
                match queue.get_job(&job_id) {
                    Some(j) if j.status == JobStatus::Cancelled => {}
                    _ => return false,
                }
            }
            true
        },
        Duration::from_secs(3),
        "All nodes should show the job as Cancelled",
    )
    .await;

    cluster.shutdown().await;
}

/// Cancelling the same job twice: the second Raft commit succeeds (the command
/// is idempotent at the Raft level — it commits but cancel_job returns false),
/// and the status remains Cancelled everywhere.
#[tokio::test]
async fn test_cancel_already_cancelled_job_is_idempotent() {
    let mut cluster = TestCluster::new(3, 50910).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let job_id = cluster
        .submit_job("echo idempotent")
        .await
        .expect("Job submission should succeed");

    // Wait for the job to appear on all nodes
    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let queue = node.job_queue.read().await;
                if queue.get_job(&job_id).is_none() {
                    return false;
                }
            }
            true
        },
        Duration::from_secs(3),
        "Job should appear on all nodes",
    )
    .await;

    // First cancel
    cancel_job(&cluster, job_id)
        .await
        .expect("First CancelJob should commit");

    // Wait for Cancelled status
    assert_eventually(
        || async {
            let leader_id = cluster.get_leader_id().await.unwrap_or(1);
            let node = cluster.get_node(leader_id).unwrap();
            node.job_queue
                .read()
                .await
                .get_job(&job_id)
                .map(|j| j.status)
                == Some(JobStatus::Cancelled)
        },
        Duration::from_secs(3),
        "Job should be Cancelled after first cancel",
    )
    .await;

    // Second cancel — should still commit to Raft (no error), status stays Cancelled
    cancel_job(&cluster, job_id)
        .await
        .expect("Second CancelJob should still commit to Raft");

    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let queue = node.job_queue.read().await;
                match queue.get_job(&job_id) {
                    Some(j) if j.status == JobStatus::Cancelled => {}
                    _ => return false,
                }
            }
            true
        },
        Duration::from_secs(3),
        "Job should remain Cancelled after second cancel",
    )
    .await;

    cluster.shutdown().await;
}

/// A non-existent job ID is accepted by Raft (no pre-check at consensus layer),
/// and the scheduler loop simply no-ops it. The queue is unchanged.
#[tokio::test]
async fn test_cancel_nonexistent_job_is_a_noop() {
    let mut cluster = TestCluster::new(3, 50920).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let phantom_id = Uuid::new_v4();
    // Should commit without error — the scheduler just finds nothing to do
    cancel_job(&cluster, phantom_id)
        .await
        .expect("CancelJob for unknown ID should still commit");

    // Queue remains empty on all nodes
    for node in cluster.nodes.values() {
        let queue = node.job_queue.read().await;
        assert!(
            queue.get_job(&phantom_id).is_none(),
            "Non-existent job must not be created by a cancel command"
        );
    }

    cluster.shutdown().await;
}

/// Submit a job, wait for it to be applied by the scheduler loop, manually mark it
/// Completed on all nodes, then verify that a subsequent CancelJob command leaves
/// the status unchanged (cancel is a no-op on terminal jobs).
#[tokio::test]
async fn test_cancel_completed_job_leaves_status_unchanged() {
    let mut cluster = TestCluster::new(3, 50930).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let job_id = cluster
        .submit_job("echo done")
        .await
        .expect("Job submission should succeed");

    // Wait for the SubmitJob entry to be applied on every node so that the job
    // exists in each queue before we manually mark it as Completed.
    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let queue = node.job_queue.read().await;
                if queue.get_job(&job_id).is_none() {
                    return false;
                }
            }
            true
        },
        Duration::from_secs(3),
        "Job should appear on all nodes before we mark it Completed",
    )
    .await;

    // Directly mark the job as Completed on all nodes (simulates UpdateJobStatus
    // being applied; we don't need Raft for the test assertion itself).
    for node in cluster.nodes.values() {
        let mut queue = node.job_queue.write().await;
        queue.update_status(
            &job_id,
            JobStatus::Completed,
            Some("done\n".to_string()),
            None,
        );
    }

    // Now attempt to cancel the completed job
    cancel_job(&cluster, job_id)
        .await
        .expect("CancelJob command should still commit to Raft");

    // Give the scheduler loop time to apply the CancelJob entry, then verify
    // that the Completed status was not overwritten.
    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let queue = node.job_queue.read().await;
                match queue.get_job(&job_id) {
                    Some(j) if j.status == JobStatus::Completed => {}
                    _ => return false,
                }
            }
            true
        },
        Duration::from_secs(3),
        "Completed job should stay Completed after a cancel command",
    )
    .await;

    cluster.shutdown().await;
}
