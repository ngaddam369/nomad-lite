mod test_harness;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use nomad_lite::proto::scheduler_service_client::SchedulerServiceClient;
use nomad_lite::proto::{DrainNodeRequest, SubmitJobRequest};
use test_harness::TestCluster;
use tonic::transport::Channel;

/// Helper: create a gRPC client to a specific test node port
async fn connect_client(port: u16) -> SchedulerServiceClient<Channel> {
    let addr = format!("http://127.0.0.1:{}", port);
    let channel = Channel::from_shared(addr).unwrap().connect().await.unwrap();
    SchedulerServiceClient::new(channel)
}

/// Test that a drained node rejects new job submissions.
#[tokio::test]
async fn test_drain_rejects_new_jobs() {
    let cluster = TestCluster::new(3, 18400).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let leader = cluster.get_node(leader_id).unwrap();
    let leader_port = leader.port;

    // Set draining on the leader node directly (simulating DrainNode RPC effect)
    let draining = Arc::new(AtomicBool::new(true));
    draining.store(true, Ordering::Relaxed);

    // Since we can't directly set draining on the test node's ClientService
    // (it's internal to GrpcServer), call the DrainNode RPC via gRPC
    let mut client = connect_client(leader_port).await;
    let drain_result = client.drain_node(DrainNodeRequest {}).await;
    assert!(drain_result.is_ok(), "Drain should succeed");

    // Now try to submit a job — should be rejected
    let submit_result = client
        .submit_job(SubmitJobRequest {
            command: "echo test".to_string(),
        })
        .await;

    assert!(submit_result.is_err(), "Submit should be rejected");
    let status = submit_result.unwrap_err();
    assert_eq!(
        status.code(),
        tonic::Code::Unavailable,
        "Should return UNAVAILABLE"
    );
    assert!(
        status.message().contains("draining"),
        "Error should mention draining: {}",
        status.message()
    );
}

/// Test that draining the leader causes leadership to transfer.
#[tokio::test]
async fn test_drain_transfers_leadership() {
    let cluster = TestCluster::new(3, 18500).await;

    let leader_id = cluster
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("Leader should be elected");

    let leader = cluster.get_node(leader_id).unwrap();
    let leader_port = leader.port;

    // Drain the leader
    let mut client = connect_client(leader_port).await;
    let response = client
        .drain_node(DrainNodeRequest {})
        .await
        .expect("Drain should succeed")
        .into_inner();
    assert!(response.success, "Drain should report success");

    // Wait for a new leader election
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Check that a new leader was elected (might be any of the remaining nodes)
    let new_leader = cluster.get_leader_id().await;
    assert!(new_leader.is_some(), "A new leader should be elected");
    assert_ne!(
        new_leader.unwrap(),
        leader_id,
        "New leader should be different from drained node"
    );
}
