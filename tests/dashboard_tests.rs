use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::ServiceExt;

use nomad_lite::config::NodeConfig;
use nomad_lite::dashboard::DashboardState;
use nomad_lite::raft::RaftNode;
use nomad_lite::scheduler::{Job, JobQueue, JobStatus};

/// Create a test app with the dashboard routes
fn create_test_app(state: DashboardState) -> Router {
    Router::new()
        .route("/api/cluster", get(cluster_status_handler))
        .route("/api/jobs", get(list_jobs_handler))
        .with_state(state)
}

// Re-implement handlers for testing (since they're private in the module)
async fn cluster_status_handler(
    axum::extract::State(state): axum::extract::State<DashboardState>,
) -> axum::Json<Value> {
    let raft_state = state.raft_node.state.read().await;

    axum::Json(json!({
        "node_id": state.raft_node.id,
        "role": raft_state.role.to_string(),
        "current_term": raft_state.current_term,
        "leader_id": if raft_state.role == nomad_lite::raft::RaftRole::Leader {
            Some(state.raft_node.id)
        } else {
            raft_state.leader_id
        },
        "commit_index": raft_state.commit_index,
        "last_applied": raft_state.last_applied,
        "log_length": raft_state.log.len(),
    }))
}

async fn list_jobs_handler(
    axum::extract::State(state): axum::extract::State<DashboardState>,
) -> axum::Json<Value> {
    let queue = state.job_queue.read().await;
    let jobs: Vec<Value> = queue
        .all_jobs()
        .into_iter()
        .map(|job| {
            json!({
                "id": job.id.to_string(),
                "command": job.command.clone(),
                "status": job.status.to_string(),
                "assigned_worker": job.assigned_worker,
                "output": job.output.clone(),
                "error": job.error.clone(),
            })
        })
        .collect();

    axum::Json(json!(jobs))
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
