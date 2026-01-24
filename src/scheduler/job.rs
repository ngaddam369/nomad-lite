use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JobStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Pending => write!(f, "pending"),
            JobStatus::Running => write!(f, "running"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub command: String,
    pub status: JobStatus,
    pub assigned_worker: Option<u64>,
    pub executed_by: Option<u64>,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl Job {
    pub fn new(command: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            command,
            status: JobStatus::Pending,
            assigned_worker: None,
            executed_by: None,
            exit_code: None,
            output: None,
            error: None,
            created_at: Utc::now(),
            completed_at: None,
        }
    }

    pub fn with_id(id: Uuid, command: String, created_at: DateTime<Utc>) -> Self {
        Self {
            id,
            command,
            status: JobStatus::Pending,
            assigned_worker: None,
            executed_by: None,
            exit_code: None,
            output: None,
            error: None,
            created_at,
            completed_at: None,
        }
    }
}
