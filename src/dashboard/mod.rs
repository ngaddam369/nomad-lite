use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_http::cors::{Any, CorsLayer};

use crate::raft::node::RaftMessage;
use crate::raft::{Command, RaftNode};
use crate::scheduler::{Job, JobQueue};

#[derive(Clone)]
pub struct DashboardState {
    pub raft_node: Arc<RaftNode>,
    pub job_queue: Arc<RwLock<JobQueue>>,
}

#[derive(Serialize)]
struct ClusterStatusResponse {
    node_id: u64,
    role: String,
    current_term: u64,
    leader_id: Option<u64>,
    commit_index: u64,
    last_applied: u64,
    log_length: usize,
}

#[derive(Serialize)]
struct JobResponse {
    id: String,
    command: String,
    status: String,
    assigned_worker: Option<u64>,
    output: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct SubmitJobRequest {
    command: String,
}

#[derive(Serialize)]
struct SubmitJobResponse {
    success: bool,
    job_id: Option<String>,
    error: Option<String>,
}

pub async fn run_dashboard(addr: SocketAddr, state: DashboardState) {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/cluster", get(cluster_status_handler))
        .route("/api/jobs", get(list_jobs_handler))
        .route("/api/jobs", post(submit_job_handler))
        .layer(cors)
        .with_state(state);

    tracing::info!(addr = %addr, "Starting dashboard server");

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(addr = %addr, error = %e, "Failed to bind dashboard server");
            return;
        }
    };

    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!(error = %e, "Dashboard server failed");
    }
}

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

async fn cluster_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let raft_state = state.raft_node.state.read().await;

    Json(ClusterStatusResponse {
        node_id: state.raft_node.id,
        role: raft_state.role.to_string(),
        current_term: raft_state.current_term,
        leader_id: if raft_state.role == crate::raft::RaftRole::Leader {
            Some(state.raft_node.id)
        } else {
            raft_state.leader_id
        },
        commit_index: raft_state.commit_index,
        last_applied: raft_state.last_applied,
        log_length: raft_state.log.len(),
    })
}

async fn list_jobs_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let queue = state.job_queue.read().await;
    let jobs: Vec<JobResponse> = queue
        .all_jobs()
        .into_iter()
        .map(|job| JobResponse {
            id: job.id.to_string(),
            command: job.command.clone(),
            status: job.status.to_string(),
            assigned_worker: job.assigned_worker,
            output: job.output.clone(),
            error: job.error.clone(),
        })
        .collect();

    Json(jobs)
}

async fn submit_job_handler(
    State(state): State<DashboardState>,
    Json(payload): Json<SubmitJobRequest>,
) -> impl IntoResponse {
    if !state.raft_node.is_leader().await {
        return (
            StatusCode::BAD_REQUEST,
            Json(SubmitJobResponse {
                success: false,
                job_id: None,
                error: Some("Not the leader".to_string()),
            }),
        );
    }

    let job = Job::new(payload.command.clone());
    let job_id = job.id;

    let command = Command::SubmitJob {
        job_id,
        command: payload.command,
    };

    let (tx, rx) = tokio::sync::oneshot::channel();
    if state
        .raft_node
        .message_sender()
        .send(RaftMessage::AppendCommand {
            command,
            response_tx: tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SubmitJobResponse {
                success: false,
                job_id: None,
                error: Some("Failed to send to Raft".to_string()),
            }),
        );
    }

    match rx.await {
        Ok(Ok(_)) => {
            if !state.job_queue.write().await.add_job(job) {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(SubmitJobResponse {
                        success: false,
                        job_id: None,
                        error: Some("Job queue is at capacity".to_string()),
                    }),
                );
            }
            (
                StatusCode::OK,
                Json(SubmitJobResponse {
                    success: true,
                    job_id: Some(job_id.to_string()),
                    error: None,
                }),
            )
        }
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(SubmitJobResponse {
                success: false,
                job_id: None,
                error: Some(e),
            }),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SubmitJobResponse {
                success: false,
                job_id: None,
                error: Some("Raft response failed".to_string()),
            }),
        ),
    }
}
