use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};

use crate::raft::node::RaftMessage;
use crate::raft::{Command, RaftNode};
use crate::scheduler::{Job, JobQueue, JobStatus};
use uuid::Uuid;

#[derive(Clone)]
pub struct DashboardState {
    pub raft_node: Arc<RaftNode>,
    pub job_queue: Arc<RwLock<JobQueue>>,
    pub draining: Arc<AtomicBool>,
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
    executed_by: Option<u64>,
    output: Option<String>,
    error: Option<String>,
    created_at: String,
    completed_at: Option<String>,
}

#[derive(Deserialize)]
pub struct SubmitJobRequest {
    command: String,
}

#[derive(Serialize)]
struct SubmitJobResponse {
    success: bool,
    job_id: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct CancelJobResponse {
    success: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct LiveResponse {
    status: &'static str,
}

#[derive(Serialize)]
struct ReadyResponse {
    status: &'static str,
    leader_id: Option<u64>,
}

pub async fn live_handler() -> impl IntoResponse {
    Json(LiveResponse { status: "ok" })
}

pub async fn ready_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let leader_id = state.raft_node.get_leader_id().await;
    if leader_id.is_some() {
        (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ok",
                leader_id,
            }),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "no_leader",
                leader_id: None,
            }),
        )
    }
}

pub async fn run_dashboard(
    addr: SocketAddr,
    state: DashboardState,
    shutdown_token: CancellationToken,
) {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/cluster", get(cluster_status_handler))
        .route("/api/jobs", get(list_jobs_handler))
        .route("/api/jobs", post(submit_job_handler))
        .route("/api/jobs/:id", delete(cancel_job_handler))
        .route("/health/live", get(live_handler))
        .route("/health/ready", get(ready_handler))
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

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_token.cancelled().await;
        })
        .await
    {
        tracing::error!(error = %e, "Dashboard server failed");
    }
}

pub async fn cancel_job_handler(
    Path(id): Path<String>,
    State(state): State<DashboardState>,
) -> impl IntoResponse {
    let job_id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(CancelJobResponse {
                    success: false,
                    error: Some("invalid job id".to_string()),
                }),
            );
        }
    };

    if !state.raft_node.is_leader().await {
        return (
            StatusCode::BAD_REQUEST,
            Json(CancelJobResponse {
                success: false,
                error: Some("not the leader".to_string()),
            }),
        );
    }

    {
        let queue = state.job_queue.read().await;
        match queue.get_job(&job_id) {
            None => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(CancelJobResponse {
                        success: false,
                        error: Some("job not found".to_string()),
                    }),
                );
            }
            Some(job) if !matches!(job.status, JobStatus::Pending | JobStatus::Running) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(CancelJobResponse {
                        success: false,
                        error: Some(format!("job is already {}", job.status)),
                    }),
                );
            }
            _ => {}
        }
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    if state
        .raft_node
        .message_sender()
        .send(RaftMessage::AppendCommand {
            command: Command::CancelJob { job_id },
            response_tx: tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CancelJobResponse {
                success: false,
                error: Some("failed to send to Raft".to_string()),
            }),
        );
    }

    match rx.await {
        Ok(Ok(_)) => (
            StatusCode::OK,
            Json(CancelJobResponse {
                success: true,
                error: None,
            }),
        ),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(CancelJobResponse {
                success: false,
                error: Some(e),
            }),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CancelJobResponse {
                success: false,
                error: Some("Raft response failed".to_string()),
            }),
        ),
    }
}

pub async fn index_handler() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

pub async fn cluster_status_handler(State(state): State<DashboardState>) -> impl IntoResponse {
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
        log_length: raft_state.last_log_index() as usize,
    })
}

pub async fn list_jobs_handler(State(state): State<DashboardState>) -> impl IntoResponse {
    let queue = state.job_queue.read().await;
    let jobs: Vec<JobResponse> = queue
        .all_jobs()
        .into_iter()
        .map(|job| JobResponse {
            id: job.id.to_string(),
            command: job.command.clone(),
            status: job.status.to_string(),
            executed_by: job.executed_by,
            output: job.output.clone(),
            error: job.error.clone(),
            created_at: job.created_at.to_rfc3339(),
            completed_at: job.completed_at.map(|dt| dt.to_rfc3339()),
        })
        .collect();

    Json(jobs)
}

pub async fn submit_job_handler(
    State(state): State<DashboardState>,
    Json(payload): Json<SubmitJobRequest>,
) -> impl IntoResponse {
    if state.draining.load(Ordering::Relaxed) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SubmitJobResponse {
                success: false,
                job_id: None,
                error: Some("Node is draining and not accepting new jobs".to_string()),
            }),
        );
    }

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

    // Reject before touching Raft — avoids orphaned committed log entries
    // when the queue is already at capacity.
    if state.job_queue.read().await.is_full() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(SubmitJobResponse {
                success: false,
                job_id: None,
                error: Some("Job queue is at capacity".to_string()),
            }),
        );
    }

    let job = Job::new(payload.command.clone());
    let job_id = job.id;

    let command = Command::SubmitJob {
        job_id,
        command: payload.command,
        created_at: job.created_at,
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
