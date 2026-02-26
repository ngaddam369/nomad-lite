//! Integration tests for Raft log compaction.
//!
//! Verifies that:
//! - Log compaction triggers when log exceeds threshold
//! - Slow followers catch up via snapshot
//! - State is consistent after compaction

mod test_harness;

use std::time::Duration;
use test_harness::{assert_eventually, TestCluster};

/// Test that log compaction triggers when many entries are submitted.
/// After submitting more than 1000 jobs, the in-memory log should be shorter
/// than the total number of entries (because prefix was compacted).
#[tokio::test]
async fn test_log_compaction_triggered() {
    let mut cluster = TestCluster::new(3, 51100).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit enough jobs to trigger compaction (threshold is 1000)
    let total_jobs = 1050;
    for i in 0..total_jobs {
        cluster
            .submit_job(&format!("echo compaction_test_{}", i))
            .await
            .unwrap_or_else(|e| panic!("Job {} submission failed: {}", i, e));
    }

    // Wait for all entries to be replicated
    assert!(
        cluster
            .wait_for_commit_on_all(total_jobs, Duration::from_secs(60))
            .await,
        "All jobs should be replicated to all nodes"
    );

    // Give time for compaction to trigger (it runs after applying committed entries)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify compaction occurred on the leader
    let leader_id = cluster.get_leader_id().await.unwrap();
    let leader = cluster.get_node(leader_id).unwrap();
    let state = leader.raft_node.state.read().await;

    assert!(
        state.log_offset > 0,
        "Log offset should be > 0 after compaction (got {})",
        state.log_offset
    );
    assert!(
        state.log.len() < total_jobs,
        "In-memory log should be shorter than total entries ({} < {})",
        state.log.len(),
        total_jobs
    );
    assert_eq!(
        state.last_log_index(),
        total_jobs as u64,
        "last_log_index should still reflect total entries"
    );
    assert!(
        state.snapshot.is_some(),
        "Snapshot should exist after compaction"
    );

    drop(state);
    cluster.shutdown().await;
}

/// Test that a slow follower catches up via snapshot after being isolated
/// during a compaction event.
#[tokio::test]
async fn test_snapshot_sent_to_slow_follower() {
    let mut cluster = TestCluster::new(3, 51200).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit a few initial jobs so all nodes have some state
    for i in 0..10 {
        cluster
            .submit_job(&format!("echo pre_isolation_{}", i))
            .await
            .unwrap();
    }
    assert!(
        cluster
            .wait_for_commit_on_all(10, Duration::from_secs(5))
            .await,
        "Initial jobs should be replicated"
    );

    // Find a follower and isolate it
    let follower_id = cluster
        .nodes
        .keys()
        .find(|&&id| id != leader_id)
        .copied()
        .unwrap();

    cluster.isolate_node(follower_id).await;

    // Submit enough jobs to trigger compaction on the majority
    // The other nodes in the majority need to be identified
    let majority_nodes: Vec<u64> = cluster
        .nodes
        .keys()
        .filter(|&&id| id != follower_id)
        .copied()
        .collect();

    let total_new_jobs = 1050;
    for i in 0..total_new_jobs {
        // Submit to leader (which is in the majority)
        cluster
            .submit_job_to_node(leader_id, &format!("echo isolated_{}", i))
            .await
            .unwrap_or_else(|e| panic!("Job {} failed: {}", i, e));
    }

    // Wait for commits on the majority nodes (follower is isolated)
    let expected_total = 10 + total_new_jobs;
    assert!(
        cluster
            .wait_for_commit_on_nodes(&majority_nodes, expected_total, Duration::from_secs(60))
            .await,
        "Majority nodes should have all entries"
    );

    // Wait for compaction to trigger
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify compaction happened on leader
    {
        let leader = cluster.get_node(leader_id).unwrap();
        let state = leader.raft_node.state.read().await;
        assert!(state.log_offset > 0, "Leader should have compacted its log");
    }

    // Heal the partition — follower should catch up via snapshot
    cluster.heal_node(follower_id).await;

    // Wait for the follower to catch up
    let follower_raft = cluster.get_node(follower_id).unwrap().raft_node.clone();

    assert_eventually(
        || async {
            let state = follower_raft.state.read().await;
            state.last_log_index() >= expected_total as u64
        },
        Duration::from_secs(15),
        "Isolated follower should catch up via snapshot",
    )
    .await;

    // Verify the follower has a snapshot installed
    {
        let follower = cluster.get_node(follower_id).unwrap();
        let state = follower.raft_node.state.read().await;
        assert!(
            state.log_offset > 0,
            "Follower should have received snapshot (log_offset = {})",
            state.log_offset
        );
    }

    cluster.shutdown().await;
}

/// Test that two successive compaction rounds both trigger and the cluster stays healthy.
#[tokio::test]
async fn test_multiple_compaction_rounds() {
    let mut cluster = TestCluster::new(3, 51400).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Round 1: exceed the 1 000-entry compaction threshold
    for i in 0..1100 {
        cluster
            .submit_job(&format!("echo round1_{}", i))
            .await
            .unwrap_or_else(|e| panic!("Round-1 job {} failed: {}", i, e));
    }
    assert!(
        cluster
            .wait_for_commit_on_all(1100, Duration::from_secs(120))
            .await,
        "Round-1 jobs should replicate to all nodes"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;

    let leader_id = cluster.get_leader_id().await.unwrap();
    let first_offset = {
        let leader = cluster.get_node(leader_id).unwrap();
        let state = leader.raft_node.state.read().await;
        assert!(
            state.log_offset > 0,
            "First compaction should have occurred"
        );
        state.log_offset
    };

    // Round 2: another 1 100 entries — triggers a second compaction
    for i in 0..1100 {
        cluster
            .submit_job(&format!("echo round2_{}", i))
            .await
            .unwrap_or_else(|e| panic!("Round-2 job {} failed: {}", i, e));
    }
    assert!(
        cluster
            .wait_for_commit_on_all(2200, Duration::from_secs(120))
            .await,
        "Round-2 jobs should replicate to all nodes"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let leader = cluster.get_node(leader_id).unwrap();
        let state = leader.raft_node.state.read().await;
        assert!(
            state.log_offset > first_offset,
            "Second compaction should have advanced log_offset (was {}, now {})",
            first_offset,
            state.log_offset
        );
    }

    // Cluster must still accept new work after two compaction rounds
    cluster
        .submit_job("echo post_compaction")
        .await
        .expect("Post-compaction job submission should succeed");
    assert!(
        cluster
            .wait_for_commit_on_all(2201, Duration::from_secs(30))
            .await,
        "Post-compaction job should replicate"
    );

    cluster.shutdown().await;
}

/// When an isolated follower misses entries that are still below the compaction threshold,
/// it catches up via AppendEntries (no snapshot is available) once the partition heals.
#[tokio::test]
async fn test_snapshot_replication_falls_back_to_entries() {
    let mut cluster = TestCluster::new(3, 51500).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Find a follower and immediately isolate it before any compaction occurs
    let follower_id = cluster
        .nodes
        .keys()
        .find(|&&id| id != leader_id)
        .copied()
        .unwrap();
    cluster.isolate_node(follower_id).await;

    let majority_nodes: Vec<u64> = cluster
        .nodes
        .keys()
        .filter(|&&id| id != follower_id)
        .copied()
        .collect();

    // Accumulate 50 entries — well below the 1 000-entry compaction threshold
    for i in 0..50 {
        cluster
            .submit_job_to_node(leader_id, &format!("echo entry_{}", i))
            .await
            .unwrap_or_else(|e| panic!("Job {} failed: {}", i, e));
    }

    assert!(
        cluster
            .wait_for_commit_on_nodes(&majority_nodes, 50, Duration::from_secs(30))
            .await,
        "Majority should commit 50 entries"
    );

    // Verify no compaction has happened (only 50 entries < 1 000 threshold)
    {
        let leader = cluster.get_node(leader_id).unwrap();
        let state = leader.raft_node.state.read().await;
        assert_eq!(
            state.log_offset, 0,
            "No compaction should have occurred with only 50 entries"
        );
    }

    // Heal the partition — follower must catch up via AppendEntries (no snapshot available)
    cluster.heal_node(follower_id).await;

    let follower_raft = cluster.get_node(follower_id).unwrap().raft_node.clone();
    assert_eventually(
        || async {
            let state = follower_raft.state.read().await;
            state.last_log_index() >= 50
        },
        Duration::from_secs(15),
        "Follower should catch up via AppendEntries",
    )
    .await;

    // The follower must NOT have received a snapshot (log_offset stays 0)
    {
        let follower = cluster.get_node(follower_id).unwrap();
        let state = follower.raft_node.state.read().await;
        assert_eq!(
            state.log_offset, 0,
            "Follower should have caught up via AppendEntries, not a snapshot"
        );
        assert_eq!(state.last_log_index(), 50);
    }

    cluster.shutdown().await;
}

/// Test that job queue state is consistent across all nodes after compaction.
#[tokio::test]
async fn test_state_consistency_after_compaction() {
    let mut cluster = TestCluster::new(3, 51300).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit enough to trigger compaction
    let total_jobs = 1050;
    for i in 0..total_jobs {
        cluster
            .submit_job(&format!("echo consistency_test_{}", i))
            .await
            .unwrap_or_else(|e| panic!("Job {} failed: {}", i, e));
    }

    // Wait for all replicated
    assert!(
        cluster
            .wait_for_commit_on_all(total_jobs, Duration::from_secs(60))
            .await,
        "All jobs should be replicated"
    );

    // Wait for compaction
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify all nodes have the same job queue state
    let mut job_counts: Vec<(u64, usize)> = Vec::new();
    for (node_id, node) in cluster.nodes.iter() {
        let queue = node.job_queue.read().await;
        job_counts.push((*node_id, queue.len()));
    }

    let first_count = job_counts[0].1;
    for (node_id, count) in &job_counts {
        assert_eq!(
            *count, first_count,
            "Node {} has {} jobs, expected {} (same as first node)",
            node_id, count, first_count
        );
    }

    // Verify all nodes agree on last_log_index
    let mut log_indices: Vec<(u64, u64)> = Vec::new();
    for (node_id, node) in cluster.nodes.iter() {
        let state = node.raft_node.state.read().await;
        log_indices.push((*node_id, state.last_log_index()));
    }

    let first_index = log_indices[0].1;
    for (node_id, index) in &log_indices {
        assert_eq!(
            *index, first_index,
            "Node {} has last_log_index={}, expected {}",
            node_id, index, first_index
        );
    }

    cluster.shutdown().await;
}
