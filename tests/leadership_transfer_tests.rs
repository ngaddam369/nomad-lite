mod test_harness;

use std::time::Duration;
use test_harness::TestCluster;

/// Test transferring leadership to a specific node.
#[tokio::test]
async fn test_leadership_transfer_to_specific_node() {
    let cluster = TestCluster::new(3, 18100).await;

    // Wait for initial leader
    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Pick a follower as target
    let target_id = if leader_id == 1 { 2 } else { 1 };

    // Transfer leadership
    let new_leader = cluster
        .transfer_leadership(Some(target_id))
        .await
        .expect("Transfer should succeed");
    assert_eq!(new_leader, target_id);

    // Wait for the target to actually become leader
    tokio::time::sleep(Duration::from_millis(300)).await;

    let current_leader = cluster.get_leader_id().await;
    assert_eq!(
        current_leader,
        Some(target_id),
        "Target node should be the new leader"
    );
}

/// Test auto-selecting a transfer target when no target is specified.
#[tokio::test]
async fn test_leadership_transfer_auto_select() {
    let cluster = TestCluster::new(3, 18200).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Auto-select target
    let new_leader = cluster
        .transfer_leadership(None)
        .await
        .expect("Auto-select transfer should succeed");

    assert_ne!(new_leader, leader_id, "New leader should be different");

    // Wait for election to settle
    tokio::time::sleep(Duration::from_millis(300)).await;

    let current_leader = cluster.get_leader_id().await;
    assert!(current_leader.is_some(), "A new leader should exist");
    assert_ne!(
        current_leader.unwrap(),
        leader_id,
        "New leader should differ from old"
    );
}

/// Test that transferring from a non-leader returns an error.
#[tokio::test]
async fn test_transfer_rejects_on_non_leader() {
    let cluster = TestCluster::new(3, 18300).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    // Pick a follower
    let follower_id = if leader_id == 1 { 2 } else { 1 };
    let follower = cluster.get_node(follower_id).unwrap();

    // Try to transfer from follower — should fail
    let (tx, rx) = tokio::sync::oneshot::channel();
    follower
        .raft_node
        .message_sender()
        .send(nomad_lite::raft::node::RaftMessage::TransferLeadership {
            target_id: None,
            response_tx: tx,
        })
        .await
        .expect("Should send message");

    let result = rx.await.expect("Should receive response");
    assert!(result.is_err(), "Transfer from follower should fail");
    assert!(
        result.unwrap_err().contains("Not the leader"),
        "Error should mention not being leader"
    );
}
