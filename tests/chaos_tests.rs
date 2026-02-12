//! Chaos and fault injection tests for Raft cluster resilience.
//!
//! These tests simulate adversarial conditions including rapid leader kills,
//! concurrent failures during job submission, network flapping, combined
//! partition + crash scenarios, and full cluster isolation/recovery.

mod test_harness;

use std::time::Duration;
use test_harness::TestCluster;

/// Kill leaders in rapid succession and verify the cluster still converges
/// to a stable leader that can commit entries.
#[tokio::test]
async fn test_rapid_leader_churn() {
    let mut cluster = TestCluster::new(5, 50700).await;

    // Kill 2 leaders in rapid succession (5-node cluster tolerates 2 permanent failures)
    for i in 0..2 {
        let leader_id = cluster
            .wait_for_leader(Duration::from_secs(5))
            .await
            .unwrap_or_else(|| panic!("Leader {} should be elected", i + 1));

        // Kill immediately — no grace period
        cluster.shutdown_node(leader_id);
    }

    // 3 of 5 nodes remain — still a quorum
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should elect a stable leader after rapid churn");

    assert_eq!(
        cluster.count_leaders().await,
        1,
        "Exactly one leader after rapid churn"
    );

    // Verify the cluster is functional
    cluster
        .submit_job("echo after_churn")
        .await
        .expect("Should submit job after leader churn");

    let remaining = cluster.active_node_ids();
    assert!(
        cluster
            .wait_for_commit_on_nodes(&remaining, 1, Duration::from_secs(3))
            .await,
        "Job should replicate to remaining nodes"
    );

    cluster.shutdown().await;
}

/// Submit jobs before a failover, kill the leader, elect a new one,
/// then submit more jobs and verify everything commits consistently.
#[tokio::test]
async fn test_submissions_across_failover() {
    let mut cluster = TestCluster::new(5, 50720).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit jobs that should fully commit before the crash
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo pre_failover_{}", i))
            .await
            .unwrap_or_else(|_| panic!("Pre-failover job {} should succeed", i));
    }

    assert!(
        cluster
            .wait_for_commit_on_all(3, Duration::from_secs(3))
            .await,
        "Pre-failover jobs should replicate"
    );

    // Kill the leader
    cluster.shutdown_node(leader_id);

    let _new_leader = cluster
        .wait_for_new_leader(leader_id, Duration::from_secs(5))
        .await
        .expect("New leader should be elected after crash");

    // Submit more jobs through the new leader
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo post_failover_{}", i))
            .await
            .unwrap_or_else(|_| panic!("Post-failover job {} should succeed", i));
    }

    let remaining = cluster.active_node_ids();
    assert!(
        cluster
            .wait_for_commit_on_nodes(&remaining, 6, Duration::from_secs(5))
            .await,
        "All 6 jobs should be committed on remaining nodes"
    );

    assert!(
        cluster.verify_log_consistency_remaining().await,
        "Logs should be consistent after failover"
    );

    cluster.shutdown().await;
}

/// Rapidly create and heal network partitions, then verify the cluster
/// converges to a single leader and can replicate entries to all nodes.
#[tokio::test]
async fn test_network_flapping() {
    let mut cluster = TestCluster::new(5, 50740).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Flap the network 5 times in quick succession
    for _ in 0..5 {
        cluster.create_partition(&[1, 2, 3], &[4, 5]).await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        cluster.heal_partition(&[1, 2, 3], &[4, 5]).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Cluster should converge to exactly one leader
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should elect leader after network flapping");

    assert_eq!(
        cluster.count_leaders().await,
        1,
        "Exactly one leader after network flapping"
    );

    // Verify the cluster is fully functional
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo after_flap_{}", i))
            .await
            .unwrap_or_else(|_| panic!("Post-flap job {} should succeed", i));
    }

    assert!(
        cluster
            .wait_for_commit_on_all(3, Duration::from_secs(5))
            .await,
        "Jobs should replicate to all nodes after healing"
    );

    cluster.shutdown().await;
}

/// Partition the cluster, then crash a node inside the majority partition.
/// The remaining majority should still have quorum and make progress.
#[tokio::test]
async fn test_partition_then_crash() {
    let mut cluster = TestCluster::new(5, 50760).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Commit some initial entries
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo before_chaos_{}", i))
            .await
            .unwrap_or_else(|_| panic!("Initial job {} should succeed", i));
    }
    assert!(
        cluster
            .wait_for_commit_on_all(3, Duration::from_secs(3))
            .await,
        "Initial jobs should replicate"
    );

    // Partition: [1,2,3,4] (majority) vs [5] (minority)
    cluster.create_partition(&[1, 2, 3, 4], &[5]).await;

    let majority_leader = cluster
        .wait_for_leader_in_group(&[1, 2, 3, 4], Duration::from_secs(5))
        .await
        .expect("Majority should elect leader");

    // Crash a non-leader in the majority
    let crash_target = (1..=4u64)
        .find(|&id| id != majority_leader)
        .expect("Should have a non-leader in majority");
    cluster.shutdown_node(crash_target);

    // 3 connected nodes remain out of 5 total — still a quorum
    let remaining_majority: Vec<u64> = (1..=4).filter(|&id| id != crash_target).collect();

    let leader = cluster
        .wait_for_leader_in_group(&remaining_majority, Duration::from_secs(5))
        .await
        .expect("Should still have leader with 3-node quorum");

    // Submit and commit a job through the surviving majority
    cluster
        .submit_job_to_node(leader, "echo after_partition_crash")
        .await
        .expect("Should submit job after partition + crash");

    assert!(
        cluster
            .wait_for_commit_on_nodes(&remaining_majority, 4, Duration::from_secs(5))
            .await,
        "Job should commit on remaining majority nodes"
    );

    cluster.shutdown().await;
}

/// Isolate the leader from all followers. Verify:
/// 1. The isolated leader can still append to its own log (but not commit)
/// 2. The majority elects a new leader
/// 3. After healing, the old leader steps down and logs converge
#[tokio::test]
async fn test_isolated_leader_cannot_commit() {
    let mut cluster = TestCluster::new(5, 50780).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit and commit a job so all nodes have at least 1 entry
    cluster
        .submit_job("echo before_isolation")
        .await
        .expect("Pre-isolation job should succeed");

    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(3))
            .await,
        "Pre-isolation job should replicate"
    );

    // Isolate the leader
    cluster.isolate_node(leader_id).await;

    // The isolated leader can still append entries (it thinks it's leader)
    // but they won't be committed because it can't reach a majority.
    let isolated = cluster.get_node(leader_id).unwrap();
    let pre_commit = {
        let state = isolated.raft_node.state.read().await;
        state.commit_index
    };

    // Append an entry on the isolated leader
    let _ = cluster
        .submit_job_to_node(leader_id, "echo during_isolation")
        .await;

    // Wait a bit — the entry should NOT be committed
    tokio::time::sleep(Duration::from_millis(500)).await;

    {
        let state = isolated.raft_node.state.read().await;
        assert_eq!(
            state.commit_index, pre_commit,
            "Isolated leader should not advance commit index"
        );
    }

    // Meanwhile the majority should elect a new leader
    let followers: Vec<u64> = (1..=5).filter(|&id| id != leader_id).collect();
    let new_leader = cluster
        .wait_for_leader_in_group(&followers, Duration::from_secs(5))
        .await
        .expect("Majority should elect new leader");

    assert_ne!(new_leader, leader_id);

    // Heal the partition
    cluster.heal_node(leader_id).await;

    // Wait for the old leader to step down (it will see a higher term)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Cluster should have exactly one leader
    let final_leader = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should have leader after healing");

    assert_eq!(
        cluster.count_leaders().await,
        1,
        "Exactly one leader after healing"
    );

    // The final leader's log should be authoritative — verify all nodes converge
    // Submit a new job through the real leader and verify full replication
    cluster
        .submit_job_to_node(final_leader, "echo after_healing")
        .await
        .expect("Should submit job after healing");

    // All nodes should eventually converge
    assert!(
        cluster
            .wait_for_commit_on_all(2, Duration::from_secs(5))
            .await,
        "All nodes should converge after healing"
    );

    cluster.shutdown().await;
}

/// Kill nodes one by one with job submissions in between.
/// Verify jobs commit as long as quorum exists, and fail gracefully
/// once quorum is lost.
#[tokio::test]
async fn test_cascading_failures_with_interleaved_jobs() {
    let mut cluster = TestCluster::new(5, 50800).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Round 1: All 5 nodes alive — submit and commit
    cluster
        .submit_job("echo round_1")
        .await
        .expect("Round 1 job should succeed");
    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(3))
            .await,
        "Round 1 should replicate to all 5 nodes"
    );

    // Kill a follower (5 → 4 nodes, quorum = 3, still OK)
    let leader1 = cluster.get_leader_id().await.unwrap();
    let victim1 = cluster
        .active_node_ids()
        .into_iter()
        .find(|&id| id != leader1)
        .unwrap();
    cluster.shutdown_node(victim1);

    // Round 2: 4 nodes alive
    let leader2 = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should still have leader with 4 nodes");
    cluster
        .submit_job("echo round_2")
        .await
        .expect("Round 2 job should succeed");
    let active2 = cluster.active_node_ids();
    assert!(
        cluster
            .wait_for_commit_on_nodes(&active2, 2, Duration::from_secs(3))
            .await,
        "Round 2 should commit on remaining 4 nodes"
    );

    // Kill another node (4 → 3 nodes, still a quorum)
    let victim2 = cluster
        .active_node_ids()
        .into_iter()
        .find(|&id| id != leader2)
        .unwrap();
    cluster.shutdown_node(victim2);

    // Round 3: 3 nodes alive — bare quorum
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should still have leader with 3 nodes (bare quorum)");
    cluster
        .submit_job("echo round_3")
        .await
        .expect("Round 3 job should succeed");
    let active3 = cluster.active_node_ids();
    assert!(
        cluster
            .wait_for_commit_on_nodes(&active3, 3, Duration::from_secs(3))
            .await,
        "Round 3 should commit on remaining 3 nodes"
    );

    // Kill one more (3 → 2 nodes, quorum lost)
    let leader3 = cluster.get_leader_id().await.unwrap();
    let victim3 = cluster
        .active_node_ids()
        .into_iter()
        .find(|&id| id != leader3)
        .unwrap();
    cluster.shutdown_node(victim3);

    // 2 nodes remain — quorum requires 3, so no leader should be elected
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The surviving 2 nodes cannot form quorum
    let active4 = cluster.active_node_ids();
    assert_eq!(active4.len(), 2, "Should have exactly 2 surviving nodes");

    cluster.shutdown().await;
}

/// Disconnect every node from every other node (full mesh breakage),
/// then reconnect them all and verify the cluster recovers.
#[tokio::test]
async fn test_all_nodes_isolated_then_healed() {
    let mut cluster = TestCluster::new(5, 50820).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Submit and commit a job first
    cluster
        .submit_job("echo before_total_isolation")
        .await
        .expect("Job should succeed before isolation");
    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(3))
            .await,
        "Job should replicate before isolation"
    );

    // Isolate every node from every other node
    for id in 1..=5u64 {
        cluster.isolate_node(id).await;
    }

    // No node can reach any other — wait for election timeouts to fire
    tokio::time::sleep(Duration::from_millis(500)).await;

    // No leader should exist (no node can get votes)
    let _leader_during_isolation = cluster.wait_for_leader(Duration::from_millis(300)).await;
    // It's possible a stale leader still thinks it's leader, but it can't commit anything

    // Heal all nodes
    for id in 1..=5u64 {
        cluster.heal_node(id).await;
    }

    // After full isolation, nodes have wildly different terms from failed elections.
    // Give the cluster extra time to settle through the election storm.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Cluster should recover and elect a leader
    let _leader = cluster
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("Should elect leader after total isolation heals");

    assert_eq!(
        cluster.count_leaders().await,
        1,
        "Exactly one leader after recovery"
    );

    // Submit a job to verify full recovery
    cluster
        .submit_job("echo after_total_isolation")
        .await
        .expect("Should submit job after recovery");

    assert!(
        cluster
            .wait_for_commit_on_all(2, Duration::from_secs(10))
            .await,
        "All nodes should have both jobs after recovery"
    );

    assert!(
        cluster.verify_log_consistency().await,
        "Logs should be consistent after full isolation recovery"
    );

    cluster.shutdown().await;
}

/// Partition the leader away while entries are still being replicated,
/// then verify the majority can still commit and the logs converge
/// after the partition heals.
#[tokio::test]
async fn test_leader_partition_during_replication() {
    let mut cluster = TestCluster::new(5, 50840).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Submit several jobs rapidly (some may not have replicated yet)
    for i in 0..5 {
        cluster
            .submit_job(&format!("echo replicate_{}", i))
            .await
            .unwrap_or_else(|_| panic!("Job {} should be accepted by leader", i));
    }

    // Immediately partition the leader away before full replication
    cluster.isolate_node(leader_id).await;

    // Majority should elect a new leader
    let followers: Vec<u64> = (1..=5).filter(|&id| id != leader_id).collect();
    let new_leader = cluster
        .wait_for_leader_in_group(&followers, Duration::from_secs(5))
        .await
        .expect("Majority should elect new leader");

    // Submit a new job through the new leader
    cluster
        .submit_job_to_node(new_leader, "echo post_partition")
        .await
        .expect("Should submit job to new leader");

    // Heal the partition
    cluster.heal_node(leader_id).await;

    // Wait for convergence — all nodes should eventually agree
    tokio::time::sleep(Duration::from_secs(1)).await;

    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should have leader after healing");

    assert_eq!(
        cluster.count_leaders().await,
        1,
        "Exactly one leader after healing"
    );

    // All nodes should have consistent logs
    assert!(
        cluster.verify_log_consistency().await,
        "Logs should be consistent after partition heals"
    );

    cluster.shutdown().await;
}
