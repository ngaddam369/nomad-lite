//! Network partition tests for Raft cluster behavior.
//!
//! These tests verify correct behavior when the network is partitioned:
//! majority elects leader, minority cannot, logs converge after healing.

mod test_harness;

use std::time::Duration;
use test_harness::TestCluster;

/// Test 1: Majority partition elects a leader
#[tokio::test]
async fn test_majority_partition_elects_leader() {
    let mut cluster = TestCluster::new(5, 50300).await;

    // Wait for initial leader
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Create partition: [1,2,3] vs [4,5]
    let majority = vec![1, 2, 3];
    let minority = vec![4, 5];
    cluster.create_partition(&majority, &minority).await;

    // Wait for majority side to elect a leader
    let leader = cluster
        .wait_for_leader_in_group(&majority, Duration::from_secs(5))
        .await;

    assert!(leader.is_some(), "Majority partition should elect a leader");
    assert!(
        majority.contains(&leader.unwrap()),
        "Leader should be in the majority partition"
    );

    cluster.shutdown().await;
}

/// Test 2: Minority partition cannot elect a leader
#[tokio::test]
async fn test_minority_partition_cannot_elect_leader() {
    let mut cluster = TestCluster::new(5, 50310).await;

    // Wait for initial leader
    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Build partition groups dynamically so the leader is always in the majority.
    // This prevents a stale leader in the minority from passing is_leader() checks.
    let mut majority: Vec<u64> = vec![leader_id];
    let mut minority: Vec<u64> = Vec::new();
    for id in 1..=5u64 {
        if id == leader_id {
            continue;
        }
        if majority.len() < 3 {
            majority.push(id);
        } else {
            minority.push(id);
        }
    }

    cluster.create_partition(&majority, &minority).await;

    // Wait for elections to settle
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify no leader in minority partition
    // Minority nodes may start elections but can't get majority (need 3 of 5)
    let minority_leader = cluster
        .wait_for_leader_in_group(&minority, Duration::from_millis(500))
        .await;

    assert!(
        minority_leader.is_none(),
        "Minority partition should not be able to elect a leader"
    );

    cluster.shutdown().await;
}

/// Test 3: Leader isolated triggers new election in remaining nodes
#[tokio::test]
async fn test_leader_isolated_new_election() {
    let mut cluster = TestCluster::new(5, 50320).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Isolate the leader
    cluster.isolate_node(leader_id).await;

    // Wait for remaining nodes to elect a new leader
    let remaining: Vec<u64> = (1..=5).filter(|&id| id != leader_id).collect();
    let new_leader = cluster
        .wait_for_leader_in_group(&remaining, Duration::from_secs(5))
        .await;

    assert!(
        new_leader.is_some(),
        "Remaining nodes should elect a new leader"
    );
    assert_ne!(
        new_leader.unwrap(),
        leader_id,
        "New leader should be different from isolated leader"
    );

    cluster.shutdown().await;
}

/// Test 4: Logs converge after partition healing
#[tokio::test]
async fn test_partition_healing_logs_converge() {
    let mut cluster = TestCluster::new(5, 50330).await;

    // Wait for initial leader
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Create partition: [1,2,3] vs [4,5]
    let majority = vec![1, 2, 3];
    let minority = vec![4, 5];
    cluster.create_partition(&majority, &minority).await;

    // Wait for majority to elect a leader
    let leader_id = cluster
        .wait_for_leader_in_group(&majority, Duration::from_secs(5))
        .await
        .expect("Majority should elect a leader");

    // Submit jobs through the majority leader
    for i in 0..3 {
        cluster
            .submit_job_to_node(leader_id, &format!("echo partition_{}", i))
            .await
            .expect(&format!("Job {} should be submitted", i));
    }

    // Wait for jobs to commit on majority nodes
    assert!(
        cluster
            .wait_for_commit_on_nodes(&majority, 3, Duration::from_secs(3))
            .await,
        "Jobs should be committed on majority nodes"
    );

    // Heal the partition
    cluster.heal_partition(&majority, &minority).await;

    // Wait for logs to converge on all nodes
    assert!(
        cluster
            .wait_for_commit_on_nodes(&[1, 2, 3, 4, 5], 3, Duration::from_secs(5))
            .await,
        "All nodes should have 3 log entries after healing"
    );

    cluster.shutdown().await;
}

/// Test 5: Jobs replicate to formerly partitioned nodes after healing
#[tokio::test]
async fn test_jobs_replicate_after_partition_healing() {
    let mut cluster = TestCluster::new(5, 50340).await;

    // Wait for initial leader and submit some initial jobs
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Create partition: [1,2,3] vs [4,5]
    let majority = vec![1, 2, 3];
    let minority = vec![4, 5];
    cluster.create_partition(&majority, &minority).await;

    // Wait for majority to stabilize with a leader
    let leader_id = cluster
        .wait_for_leader_in_group(&majority, Duration::from_secs(5))
        .await
        .expect("Majority should elect a leader");

    // Submit jobs during partition
    for i in 0..5 {
        cluster
            .submit_job_to_node(leader_id, &format!("echo replicate_{}", i))
            .await
            .expect(&format!("Job {} should be submitted", i));
    }

    // Wait for majority to commit
    assert!(
        cluster
            .wait_for_commit_on_nodes(&majority, 5, Duration::from_secs(3))
            .await,
        "Majority nodes should have 5 entries"
    );

    // Verify minority has no entries yet
    for &node_id in &minority {
        let node = cluster.get_node(node_id).unwrap();
        let log_len = node.log_len().await;
        assert_eq!(
            log_len, 0,
            "Minority node {} should have 0 entries before healing",
            node_id
        );
    }

    // Heal the partition
    cluster.heal_partition(&majority, &minority).await;

    // Wait for replication to minority nodes
    assert!(
        cluster
            .wait_for_commit_on_nodes(&minority, 5, Duration::from_secs(5))
            .await,
        "Minority nodes should receive all 5 entries after healing"
    );

    cluster.shutdown().await;
}

/// Test 6: Split brain prevention - exactly one leader after partition heals
#[tokio::test]
async fn test_split_brain_prevention() {
    let mut cluster = TestCluster::new(5, 50350).await;

    // Wait for initial leader
    cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Initial leader should be elected");

    // Create partition: [1,2,3] vs [4,5]
    let majority = vec![1, 2, 3];
    let minority = vec![4, 5];
    cluster.create_partition(&majority, &minority).await;

    // Wait for majority to elect leader
    cluster
        .wait_for_leader_in_group(&majority, Duration::from_secs(5))
        .await
        .expect("Majority should elect a leader");

    // Let minority nodes attempt elections (they'll fail but increment terms)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Heal partition
    cluster.heal_partition(&majority, &minority).await;

    // Wait for cluster to stabilize after healing
    // Minority nodes may have higher terms, triggering re-election
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Wait for a single leader to emerge
    let leader = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Should have a leader after healing");

    // Verify exactly one leader across all nodes
    let leader_count = cluster.count_leaders().await;
    assert_eq!(
        leader_count, 1,
        "Exactly one leader should exist after partition heals (found {}, leader={})",
        leader_count, leader
    );

    cluster.shutdown().await;
}

/// Test 7: Isolated node rejoins cluster and catches up
#[tokio::test]
async fn test_isolated_node_rejoins_cluster() {
    let mut cluster = TestCluster::new(3, 50360).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Find a follower to isolate
    let isolated_node = (1..=3u64)
        .find(|&id| id != leader_id)
        .expect("Should have a follower");

    // Submit initial job before isolation
    cluster
        .submit_job("echo before_isolation")
        .await
        .expect("Initial job should be submitted");

    // Wait for replication to all nodes
    assert!(
        cluster
            .wait_for_commit_on_all(1, Duration::from_secs(2))
            .await,
        "Initial job should replicate to all"
    );

    // Isolate the follower
    cluster.isolate_node(isolated_node).await;

    // Submit more jobs while node is isolated
    for i in 0..3 {
        cluster
            .submit_job(&format!("echo during_isolation_{}", i))
            .await
            .expect(&format!("Job {} should be submitted", i));
    }

    // Wait for remaining nodes to commit
    let remaining: Vec<u64> = (1..=3).filter(|&id| id != isolated_node).collect();
    assert!(
        cluster
            .wait_for_commit_on_nodes(&remaining, 4, Duration::from_secs(3))
            .await,
        "Remaining nodes should have 4 entries"
    );

    // Verify isolated node still has only 1 entry
    {
        let node = cluster.get_node(isolated_node).unwrap();
        assert_eq!(
            node.log_len().await,
            1,
            "Isolated node should still have only 1 entry"
        );
    }

    // Heal the isolated node
    cluster.heal_node(isolated_node).await;

    // Wait for isolated node to catch up
    assert!(
        cluster
            .wait_for_commit_on_nodes(&[isolated_node], 4, Duration::from_secs(5))
            .await,
        "Isolated node should catch up to 4 entries after rejoining"
    );

    // Verify log consistency across all nodes
    assert!(
        cluster.verify_log_consistency().await,
        "All nodes should have consistent logs"
    );

    cluster.shutdown().await;
}
