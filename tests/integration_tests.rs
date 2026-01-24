//! Integration tests for multi-node Raft cluster operations.
//!
//! These tests verify cluster behavior including leader election,
//! log replication, and consistency across multiple nodes.

mod test_harness;

use chrono::Utc;
use std::time::Duration;
use test_harness::{assert_eventually, TestCluster};

/// Test 1: Three-node cluster elects exactly one leader
#[tokio::test]
async fn test_three_node_cluster_elects_leader() {
    let mut cluster = TestCluster::new(3, 50100).await;

    // Wait for leader election (max 5 seconds)
    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("A leader should be elected within 5 seconds");

    // Verify exactly one leader exists
    let leader_count = cluster.count_leaders().await;
    assert_eq!(leader_count, 1, "Exactly one leader should exist");

    // Verify all nodes agree on the leader (eventually)
    assert_eventually(
        || async {
            for node in cluster.nodes.values() {
                let known_leader = node.leader_id().await;
                if known_leader != Some(leader_id) {
                    return false;
                }
            }
            true
        },
        Duration::from_secs(2),
        "All nodes should agree on leader",
    )
    .await;

    cluster.shutdown().await;
}

/// Test 2: Job submission through leader succeeds
#[tokio::test]
async fn test_job_submission_through_leader() {
    let mut cluster = TestCluster::new(3, 50110).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit job through leader
    let job_id = cluster
        .submit_job("echo hello")
        .await
        .expect("Job submission should succeed");

    // Verify job exists in leader's queue (eventually committed)
    let leader = cluster.get_node(leader_id).unwrap();
    assert_eventually(
        || async {
            let queue = leader.job_queue.read().await;
            queue.get_job(&job_id).is_some()
        },
        Duration::from_secs(2),
        "Job should exist in leader's queue",
    )
    .await;

    cluster.shutdown().await;
}

/// Test 3: Log replication to all followers
#[tokio::test]
async fn test_log_replication_to_followers() {
    let mut cluster = TestCluster::new(3, 50120).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit multiple jobs
    for i in 0..5 {
        cluster
            .submit_job(&format!("echo job_{}", i))
            .await
            .expect(&format!("Job {} submission should succeed", i));
    }

    // Wait for replication to all nodes
    assert!(
        cluster
            .wait_for_commit_on_all(5, Duration::from_secs(3))
            .await,
        "All nodes should have 5 log entries"
    );

    // Verify log consistency
    assert!(
        cluster.verify_log_consistency().await,
        "Logs should be consistent across all nodes"
    );

    cluster.shutdown().await;
}

/// Test 4: Read from any node (including followers)
#[tokio::test]
async fn test_read_from_any_node() {
    let mut cluster = TestCluster::new(3, 50130).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit job through leader
    let job_id = cluster
        .submit_job("echo test_read")
        .await
        .expect("Job submission should succeed");

    // Wait for replication
    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(2))
            .await,
        "Job should be replicated to all nodes"
    );

    // Read from all nodes (including followers)
    for (node_id, node) in cluster.nodes.iter() {
        let queue = node.job_queue.read().await;
        let job = queue.get_job(&job_id);
        assert!(
            job.is_some(),
            "Job should be readable from node {}",
            node_id
        );
        assert_eq!(
            job.unwrap().command,
            "echo test_read",
            "Job command should match on node {}",
            node_id
        );
    }

    cluster.shutdown().await;
}

/// Test 5: Cluster status reporting shows correct state
#[tokio::test]
async fn test_cluster_status_reporting() {
    let mut cluster = TestCluster::new(3, 50140).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let leader = cluster.get_node(leader_id).unwrap();

    // Wait for heartbeats to establish peer status
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify leader state
    {
        let state = leader.raft_node.state.read().await;
        assert_eq!(state.role, nomad_lite::raft::RaftRole::Leader);
        assert!(state.current_term >= 1, "Term should be at least 1");
    }

    // All peers should eventually be marked as alive (leader has communicated with them)
    let raft_node = leader.raft_node.clone();
    assert_eventually(
        || async {
            let status = raft_node.get_peers_status().await;
            status.values().all(|&is_alive| is_alive)
        },
        Duration::from_secs(3),
        "All peers should be marked as alive",
    )
    .await;

    cluster.shutdown().await;
}

/// Test 6: Follower rejects write operations
#[tokio::test]
async fn test_follower_rejects_writes() {
    let mut cluster = TestCluster::new(3, 50150).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Find a follower
    let follower_id = cluster
        .nodes
        .keys()
        .find(|&&id| id != leader_id)
        .copied()
        .expect("Should have at least one follower");

    let follower = cluster.get_node(follower_id).unwrap();

    // Try to submit job directly to follower (should fail)
    let job_id = uuid::Uuid::new_v4();
    let (tx, rx) = tokio::sync::oneshot::channel();

    follower
        .raft_node
        .message_sender()
        .send(nomad_lite::raft::node::RaftMessage::AppendCommand {
            command: nomad_lite::raft::Command::SubmitJob {
                job_id,
                command: "echo should_fail".to_string(),
                created_at: Utc::now(),
            },
            response_tx: tx,
        })
        .await
        .expect("Should be able to send message");

    let result = rx.await.expect("Should receive response");
    assert!(result.is_err(), "Follower should reject write operations");

    cluster.shutdown().await;
}

/// Test 7: Concurrent job submissions all succeed
#[tokio::test]
async fn test_concurrent_job_submissions() {
    let mut cluster = TestCluster::new(3, 50160).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit 10 jobs concurrently
    let mut handles = Vec::new();
    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();

    for i in 0..10 {
        let job_id = uuid::Uuid::new_v4();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sender = leader.raft_node.message_sender();
        let command = format!("echo concurrent_{}", i);

        let handle = tokio::spawn(async move {
            sender
                .send(nomad_lite::raft::node::RaftMessage::AppendCommand {
                    command: nomad_lite::raft::Command::SubmitJob {
                        job_id,
                        command,
                        created_at: Utc::now(),
                    },
                    response_tx: tx,
                })
                .await
                .expect("Send should succeed");

            rx.await.expect("Should receive response")
        });

        handles.push(handle);
    }

    // Wait for all submissions
    let results: Vec<Result<Result<u64, String>, _>> = futures::future::join_all(handles).await;

    // All should succeed
    let successful = results
        .iter()
        .filter(|r| r.as_ref().map(|inner| inner.is_ok()).unwrap_or(false))
        .count();
    assert_eq!(
        successful, 10,
        "All 10 concurrent submissions should succeed"
    );

    // Wait for replication and verify consistency
    assert!(
        cluster
            .wait_for_commit_on_all(10, Duration::from_secs(3))
            .await,
        "All 10 jobs should be replicated"
    );

    assert!(
        cluster.verify_log_consistency().await,
        "Logs should be consistent after concurrent submissions"
    );

    cluster.shutdown().await;
}

/// Test 8: Large batch submission
#[tokio::test]
async fn test_large_batch_submission() {
    let mut cluster = TestCluster::new(3, 50170).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit 50 jobs sequentially
    for i in 0..50 {
        cluster
            .submit_job(&format!("echo batch_{}", i))
            .await
            .expect(&format!("Job {} should be submitted", i));
    }

    // Wait for all to be committed on all nodes
    assert!(
        cluster
            .wait_for_commit_on_all(50, Duration::from_secs(10))
            .await,
        "All 50 jobs should be replicated to all nodes"
    );

    // Verify log consistency
    assert!(
        cluster.verify_log_consistency().await,
        "Logs should be consistent after large batch"
    );

    // Verify log length on each node
    for (node_id, node) in cluster.nodes.iter() {
        let log_len = node.log_len().await;
        assert_eq!(
            log_len, 50,
            "Node {} should have exactly 50 log entries",
            node_id
        );
    }

    cluster.shutdown().await;
}
