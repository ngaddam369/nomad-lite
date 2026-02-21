use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
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
    cluster_status_handler, index_handler, list_jobs_handler, submit_job_handler, DashboardState,
};
use nomad_lite::raft::RaftNode;
use nomad_lite::scheduler::{Job, JobQueue, JobStatus};

/// Create a test app wired to the real dashboard handlers
fn create_test_app(state: DashboardState) -> Router {
    Router::new()
        .route("/", get(index_handler))
        .route("/api/cluster", get(cluster_status_handler))
        .route("/api/jobs", get(list_jobs_handler))
        .route("/api/jobs", post(submit_job_handler))
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
