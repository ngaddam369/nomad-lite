use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{delete, get, post},
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceExt;

use nomad_lite::config::NodeConfig;
use nomad_lite::dashboard::{
    cancel_job_handler, cluster_status_handler, index_handler, list_jobs_handler, live_handler,
    ready_handler, submit_job_handler, DashboardState,
};
use nomad_lite::raft::node::RaftMessage;
use nomad_lite::raft::state::RaftRole;
use nomad_lite::raft::RaftNode;
use nomad_lite::scheduler::{Job, JobQueue, JobStatus};

/// Create a test app wired to the real dashboard handlers
fn create_test_app(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/api/cluster", get(cluster_status_handler))
        .route("/api/jobs", get(list_jobs_handler))
        .route("/api/jobs", post(submit_job_handler))
        .route("/api/jobs/:id", delete(cancel_job_handler))
        .route("/health/live", get(live_handler))
        .route("/health/ready", get(ready_handler))
        .with_state(state)
}

/// Helper to create test state
fn create_test_state() -> (
    DashboardState,
    tokio::sync::mpsc::Receiver<nomad_lite::raft::node::RaftMessage>,
) {
    let config = NodeConfig::default();
    let (raft_node, raft_rx) = RaftNode::new(config, None);

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue: Arc::new(RwLock::new(JobQueue::new())),
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };

    (state, raft_rx)
}

#[tokio::test]
async fn test_index_returns_html() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(content_type.contains("text/html"));
}

#[tokio::test]
async fn test_cluster_status_endpoint() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/cluster")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["node_id"], 1);
    assert_eq!(json["role"], "follower");
    assert_eq!(json["current_term"], 0);
    assert_eq!(json["commit_index"], 0);
    assert_eq!(json["last_applied"], 0);
    assert_eq!(json["log_length"], 0);
}

#[tokio::test]
async fn test_list_jobs_empty() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_list_jobs_with_jobs() {
    let (state, _rx) = create_test_state();

    // Add some jobs to the queue
    {
        let mut queue = state.job_queue.write().await;
        queue.add_job(Job::new("echo hello".to_string()));
        queue.add_job(Job::new("echo world".to_string()));
    }

    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let jobs = json.as_array().unwrap();
    assert_eq!(jobs.len(), 2);
}

#[tokio::test]
async fn test_list_jobs_shows_job_details() {
    let (state, _rx) = create_test_state();

    // Add a job and update its status
    let job = Job::new("echo test".to_string());
    let job_id = job.id;
    {
        let mut queue = state.job_queue.write().await;
        queue.add_job(job);
        queue.update_status(
            &job_id,
            JobStatus::Completed,
            Some("test output".to_string()),
            None,
        );
    }

    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let jobs = json.as_array().unwrap();
    assert_eq!(jobs.len(), 1);

    let job = &jobs[0];
    assert_eq!(job["command"], "echo test");
    assert_eq!(job["status"], "completed");
    assert_eq!(job["output"], "test output");
}

#[tokio::test]
async fn test_cluster_status_returns_json() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/cluster")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Check content type is JSON
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    assert!(content_type.contains("application/json"));
}

#[tokio::test]
async fn test_submit_job_rejected_when_draining() {
    let (state, _rx) = create_test_state();
    state.draining.store(true, Ordering::Relaxed);
    let app = create_test_app(state);

    let body = serde_json::to_string(&json!({ "command": "echo hi" })).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("draining"));
}

/// Verify queue-full path returns 503 with "capacity" in the error message.
/// Requires forcing the node into leader state and providing a fake Raft responder
/// so the handler proceeds past the is_leader() check to the add_job() call.
#[tokio::test]
async fn test_submit_job_queue_at_capacity() {
    let config = NodeConfig::default();
    let (raft_node, mut raft_rx) = RaftNode::new(config, None);

    // Small queue already at capacity
    let job_queue = Arc::new(RwLock::new(JobQueue::with_capacity(1)));
    job_queue
        .write()
        .await
        .add_job(Job::new("existing".to_string()));

    // Force node to think it is the leader
    {
        let mut state = raft_node.state.write().await;
        state.role = RaftRole::Leader;
        state.leader_id = Some(1);
    }

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue,
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };

    // Fake Raft responder: immediately reply Ok(1) so the handler advances past Raft
    tokio::spawn(async move {
        while let Some(msg) = raft_rx.recv().await {
            if let RaftMessage::AppendCommand { response_tx, .. } = msg {
                let _ = response_tx.send(Ok(1));
            }
        }
    });

    let app = create_test_app(state);
    let body = serde_json::to_string(&json!({ "command": "echo hi" })).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("capacity"));
}

/// Verify that the /api/cluster response always includes a "role" field.
#[tokio::test]
async fn test_cluster_status_role_field() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/cluster")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert!(
        json.get("role").is_some(),
        "role field must be present in cluster status"
    );
    assert_eq!(
        json["role"], "follower",
        "a fresh node starts as a follower"
    );
}

#[tokio::test]
async fn test_submit_job_rejected_when_not_leader() {
    let (state, _rx) = create_test_state();
    // A freshly-created node starts as a follower, so is_leader() == false
    let app = create_test_app(state);

    let body = serde_json::to_string(&json!({ "command": "echo hi" })).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("Not the leader"));
}

// ── cancel_job_handler tests ─────────────────────────────────────────────────

#[tokio::test]
async fn test_cancel_job_invalid_uuid() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/jobs/not-a-valid-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("invalid job id"));
}

#[tokio::test]
async fn test_cancel_job_rejected_when_not_leader() {
    let (state, _rx) = create_test_state();
    // Fresh node is a follower
    let job_id = uuid::Uuid::new_v4();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/jobs/{}", job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("not the leader"));
}

#[tokio::test]
async fn test_cancel_job_not_found() {
    let config = NodeConfig::default();
    let (raft_node, _raft_rx) = RaftNode::new(config, None);

    // Force leader so we reach the job-lookup
    {
        let mut state = raft_node.state.write().await;
        state.role = RaftRole::Leader;
        state.leader_id = Some(1);
    }

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue: Arc::new(RwLock::new(JobQueue::new())),
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };
    let job_id = uuid::Uuid::new_v4();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/jobs/{}", job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"].as_str().unwrap().contains("job not found"));
}

#[tokio::test]
async fn test_cancel_job_already_terminal() {
    let config = NodeConfig::default();
    let (raft_node, _raft_rx) = RaftNode::new(config, None);

    {
        let mut state = raft_node.state.write().await;
        state.role = RaftRole::Leader;
        state.leader_id = Some(1);
    }

    let job = Job::new("echo hello".to_string());
    let job_id = job.id;
    let job_queue = Arc::new(RwLock::new(JobQueue::new()));
    {
        let mut queue = job_queue.write().await;
        queue.add_job(job);
        queue.update_status(&job_id, JobStatus::Completed, None, None);
    }

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue,
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/jobs/{}", job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"]
        .as_str()
        .unwrap()
        .contains("job is already completed"));
}

#[tokio::test]
async fn test_cancel_pending_job_success() {
    let config = NodeConfig::default();
    let (raft_node, mut raft_rx) = RaftNode::new(config, None);

    {
        let mut state = raft_node.state.write().await;
        state.role = RaftRole::Leader;
        state.leader_id = Some(1);
    }

    let job = Job::new("echo hello".to_string());
    let job_id = job.id;
    let job_queue = Arc::new(RwLock::new(JobQueue::new()));
    job_queue.write().await.add_job(job);

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue,
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };

    // Fake Raft responder: immediately reply Ok(1)
    tokio::spawn(async move {
        while let Some(msg) = raft_rx.recv().await {
            if let RaftMessage::AppendCommand { response_tx, .. } = msg {
                let _ = response_tx.send(Ok(1));
            }
        }
    });

    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/jobs/{}", job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], true);
    assert!(json["error"].is_null());
}

#[tokio::test]
async fn test_cancel_job_raft_commit_error() {
    let config = NodeConfig::default();
    let (raft_node, mut raft_rx) = RaftNode::new(config, None);

    {
        let mut state = raft_node.state.write().await;
        state.role = RaftRole::Leader;
        state.leader_id = Some(1);
    }

    let job = Job::new("echo hello".to_string());
    let job_id = job.id;
    let job_queue = Arc::new(RwLock::new(JobQueue::new()));
    job_queue.write().await.add_job(job);

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue,
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };

    // Fake Raft responder: reply with an error
    tokio::spawn(async move {
        while let Some(msg) = raft_rx.recv().await {
            if let RaftMessage::AppendCommand { response_tx, .. } = msg {
                let _ = response_tx.send(Err("simulated Raft error".to_string()));
            }
        }
    });

    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/jobs/{}", job_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"]
        .as_str()
        .unwrap()
        .contains("simulated Raft error"));
}

// ── cluster status nodes field tests ─────────────────────────────────────────

#[tokio::test]
async fn test_cluster_status_includes_nodes_field() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/cluster")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    assert!(json["nodes"].is_array(), "nodes must be a non-null array");
}

#[tokio::test]
async fn test_cluster_status_self_node_is_alive() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/cluster")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();

    let nodes = json["nodes"].as_array().unwrap();
    assert_eq!(
        nodes.len(),
        1,
        "single-node config should have exactly one node entry"
    );
    assert_eq!(nodes[0]["node_id"], 1);
    assert_eq!(nodes[0]["is_alive"], true);
}

// ── input validation tests ────────────────────────────────────────────────────

#[tokio::test]
async fn test_submit_job_empty_command() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let body = serde_json::to_string(&json!({ "command": "   " })).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    assert!(json["error"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("empty"));
}

#[tokio::test]
async fn test_submit_job_command_too_long() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let long_command = "a".repeat(nomad_lite::scheduler::MAX_COMMAND_LEN + 1);
    let body = serde_json::to_string(&json!({ "command": long_command })).unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["success"], false);
    let err = json["error"].as_str().unwrap().to_lowercase();
    assert!(
        err.contains("length") || err.contains("exceeds"),
        "error should mention length/exceeds, got: {}",
        err
    );
}

// ── health check tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_live_returns_200() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_live_returns_status_ok() {
    let (state, _rx) = create_test_state();
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn test_ready_returns_503_when_no_leader() {
    let (state, _rx) = create_test_state();
    // A fresh node has no leader (leader_id = None)
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "no_leader");
    assert!(json["leader_id"].is_null());
}

#[tokio::test]
async fn test_ready_returns_200_when_leader_known() {
    let config = NodeConfig::default();
    let (raft_node, _raft_rx) = RaftNode::new(config, None);

    // Force node into leader state so get_leader_id() returns Some(1)
    {
        let mut raft_state = raft_node.state.write().await;
        raft_state.role = RaftRole::Leader;
        raft_state.leader_id = Some(1);
    }

    let state = DashboardState {
        raft_node: Arc::new(raft_node),
        job_queue: Arc::new(RwLock::new(JobQueue::new())),
        draining: Arc::new(AtomicBool::new(false)),
        tls_identity: None,
    };
    let app = create_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["leader_id"], 1);
}
