use std::process::Stdio;
use tokio::process::Command;
use uuid::Uuid;

use crate::scheduler::JobStatus;

/// Result of job execution
#[derive(Debug)]
pub struct ExecutionResult {
    pub job_id: Uuid,
    pub status: JobStatus,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Executes jobs by running shell commands
#[derive(Debug, Default)]
pub struct JobExecutor;

impl JobExecutor {
    pub fn new() -> Self {
        Self
    }

    /// Execute a job command and return the result
    pub async fn execute(&self, job_id: Uuid, command: &str) -> ExecutionResult {
        tracing::info!(job_id = %job_id, command, "Executing job");

        let result = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                let (status, error) = if output.status.success() {
                    (JobStatus::Completed, None)
                } else {
                    (
                        JobStatus::Failed,
                        Some(if stderr.is_empty() {
                            format!("Exit code: {:?}", output.status.code())
                        } else {
                            stderr.clone()
                        }),
                    )
                };

                tracing::info!(
                    job_id = %job_id,
                    status = %status,
                    exit_code = ?output.status.code(),
                    "Job completed"
                );

                ExecutionResult {
                    job_id,
                    status,
                    output: if stdout.is_empty() {
                        None
                    } else {
                        Some(stdout)
                    },
                    error,
                }
            }
            Err(e) => {
                tracing::error!(job_id = %job_id, error = %e, "Job execution failed");
                ExecutionResult {
                    job_id,
                    status: JobStatus::Failed,
                    output: None,
                    error: Some(e.to_string()),
                }
            }
        }
    }
}
