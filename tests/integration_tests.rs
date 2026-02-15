//! Integration tests for multi-node Raft cluster operations.
//!
//! These tests verify cluster behavior including leader election,
//! log replication, and consistency across multiple nodes.

mod test_harness;

use chrono::Utc;
use nomad_lite::raft::rpc::log_entry_to_proto;
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

/// Test 9: View Raft log entries returns entries in order
#[tokio::test]
async fn test_view_raft_log_entries() {
    let mut cluster = TestCluster::new(3, 50180).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit 5 jobs
    for i in 0..5 {
        cluster
            .submit_job(&format!("echo log_test_{}", i))
            .await
            .expect(&format!("Job {} submission should succeed", i));
    }

    // Wait for replication
    assert!(
        cluster
            .wait_for_commit_on_all(5, Duration::from_secs(3))
            .await,
        "All nodes should have 5 log entries"
    );

    // Get leader and verify log entries
    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();

    let state = leader.raft_node.state.read().await;

    // Verify log entries are in order
    assert_eq!(state.last_log_index(), 5, "Should have 5 log entries");

    let all_entries = state.get_entries_from(1);
    for (i, entry) in all_entries.iter().enumerate() {
        assert_eq!(
            entry.index,
            (i + 1) as u64,
            "Entry index should be sequential"
        );
        assert!(
            entry.index <= state.commit_index,
            "Entry should be committed"
        );
    }

    // Verify entries can be converted to proto format
    for entry in all_entries.iter() {
        let proto = log_entry_to_proto(entry);
        assert_eq!(proto.index, entry.index);
        assert_eq!(proto.term, entry.term);
    }

    drop(state);
    cluster.shutdown().await;
}

/// Test 10: Raft log entries show correct commit status
#[tokio::test]
async fn test_raft_log_commit_status() {
    let mut cluster = TestCluster::new(3, 50190).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit a job
    cluster
        .submit_job("echo commit_test")
        .await
        .expect("Job submission should succeed");

    // Wait for commit
    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(2))
            .await,
        "Job should be committed on all nodes"
    );

    // Verify commit_index on leader
    let leader = cluster.get_node(leader_id).unwrap();
    let state = leader.raft_node.state.read().await;

    assert!(state.commit_index >= 1, "Commit index should be at least 1");
    assert_eq!(state.last_log_index(), 1, "Should have 1 log entry");

    let entry = state.get_entry(1).expect("Entry 1 should exist");
    assert!(
        entry.index <= state.commit_index,
        "Entry should be committed"
    );

    drop(state);
    cluster.shutdown().await;
}

/// Test 11: Empty log returns no entries
#[tokio::test]
async fn test_empty_raft_log() {
    let mut cluster = TestCluster::new(3, 50200).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Don't submit any jobs - log should be empty
    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();

    let state = leader.raft_node.state.read().await;
    assert_eq!(state.last_log_index(), 0, "Log should be empty");
    assert_eq!(state.commit_index, 0, "Commit index should be 0");

    drop(state);
    cluster.shutdown().await;
}

/// Test 12: Log entries contain correct command types
#[tokio::test]
async fn test_raft_log_command_types() {
    let mut cluster = TestCluster::new(3, 50210).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit a job
    let job_id = cluster
        .submit_job("echo command_type_test")
        .await
        .expect("Job submission should succeed");

    // Wait for commit
    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(2))
            .await,
        "Job should be committed"
    );

    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();

    let state = leader.raft_node.state.read().await;
    assert_eq!(state.last_log_index(), 1, "Should have 1 log entry");

    // Verify the command is a SubmitJob
    let first_entry = state.get_entry(1).expect("Entry 1 should exist");
    match &first_entry.command {
        nomad_lite::raft::Command::SubmitJob {
            job_id: entry_job_id,
            command,
            ..
        } => {
            assert_eq!(*entry_job_id, job_id, "Job ID should match");
            assert_eq!(command, "echo command_type_test", "Command should match");
        }
        _ => panic!("Expected SubmitJob command"),
    }

    drop(state);
    cluster.shutdown().await;
}

/// Test 13: Log pagination with start_index beyond log length returns empty
#[tokio::test]
async fn test_raft_log_pagination_beyond_length() {
    let mut cluster = TestCluster::new(3, 50220).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit 3 jobs
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo pagination_test_{}", i))
            .await
            .expect("Job submission should succeed");
    }

    // Wait for commit
    assert!(
        cluster
            .wait_for_commit_on_all(3, Duration::from_secs(2))
            .await,
        "Jobs should be committed"
    );

    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();

    let state = leader.raft_node.state.read().await;

    // Simulate pagination beyond log length (start_index = 100 when log has 3 entries)
    let entries_from_beyond = state.get_entries_from(100);
    assert!(
        entries_from_beyond.is_empty(),
        "Should return empty when start_index is beyond log length"
    );

    drop(state);
    cluster.shutdown().await;
}

/// Test 14: Log pagination with limit larger than available entries
#[tokio::test]
async fn test_raft_log_pagination_limit_exceeds_entries() {
    let mut cluster = TestCluster::new(3, 50230).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit 5 jobs
    for i in 0..5 {
        cluster
            .submit_job(&format!("echo limit_test_{}", i))
            .await
            .expect("Job submission should succeed");
    }

    // Wait for commit
    assert!(
        cluster
            .wait_for_commit_on_all(5, Duration::from_secs(2))
            .await,
        "Jobs should be committed"
    );

    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();

    let state = leader.raft_node.state.read().await;

    // Request all entries (limit larger than available)
    let entries = state.get_entries_from(1);
    assert_eq!(entries.len(), 5, "Should return all 5 entries");

    drop(state);
    cluster.shutdown().await;
}

/// Test 15: Log entries accessible from follower (forwarded to leader)
#[tokio::test]
async fn test_raft_log_accessible_from_follower() {
    let mut cluster = TestCluster::new(3, 50240).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit jobs through leader
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo follower_access_test_{}", i))
            .await
            .expect("Job submission should succeed");
    }

    // Wait for replication
    assert!(
        cluster
            .wait_for_commit_on_all(3, Duration::from_secs(2))
            .await,
        "Jobs should be replicated"
    );

    // Find a follower
    let follower_id = cluster
        .nodes
        .keys()
        .find(|&&id| id != leader_id)
        .copied()
        .expect("Should have a follower");

    // Verify follower has the same log entries (replication works)
    let follower = cluster.get_node(follower_id).unwrap();
    let follower_state = follower.raft_node.state.read().await;

    assert_eq!(
        follower_state.last_log_index(),
        3,
        "Follower should have 3 log entries"
    );

    // Verify entries match leader's entries
    let leader = cluster.get_node(leader_id).unwrap();
    let leader_state = leader.raft_node.state.read().await;

    for idx in 1..=3u64 {
        let follower_entry = follower_state.get_entry(idx).expect("Entry should exist");
        let leader_entry = leader_state.get_entry(idx).expect("Entry should exist");
        assert_eq!(
            follower_entry.index, leader_entry.index,
            "Entry {} index should match",
            idx
        );
        assert_eq!(
            follower_entry.term, leader_entry.term,
            "Entry {} term should match",
            idx
        );
    }

    drop(follower_state);
    drop(leader_state);
    cluster.shutdown().await;
}
