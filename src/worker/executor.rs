use std::process::Stdio;
use tokio::process::Command;
use uuid::Uuid;

use crate::config::SandboxConfig;
use crate::scheduler::JobStatus;

/// Result of job execution
#[derive(Debug)]
pub struct ExecutionResult {
    pub job_id: Uuid,
    pub status: JobStatus,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Executes jobs in Docker containers with security isolation.
///
/// All jobs run in sandboxed Docker containers with:
/// - Network isolation (disabled by default)
/// - Dropped capabilities
/// - Read-only root filesystem
/// - Memory and CPU limits
#[derive(Debug, Clone)]
pub struct JobExecutor {
    config: SandboxConfig,
}

impl JobExecutor {
    pub fn new(config: SandboxConfig) -> Self {
        Self { config }
    }

    /// Execute a job command in a sandboxed Docker container
    pub async fn execute(&self, job_id: Uuid, command: &str) -> ExecutionResult {
        tracing::info!(job_id = %job_id, command, image = %self.config.image, "Executing job");

        let mut args = vec!["run".to_string(), "--rm".to_string()];

        // Network isolation
        if self.config.network_disabled {
            args.push("--network=none".to_string());
        }

        // Memory limit
        if let Some(ref limit) = self.config.memory_limit {
            args.push(format!("--memory={}", limit));
        }

        // CPU limit
        if let Some(ref limit) = self.config.cpu_limit {
            args.push(format!("--cpus={}", limit));
        }

        // Security: drop all capabilities, no new privileges
        args.push("--cap-drop=ALL".to_string());
        args.push("--security-opt=no-new-privileges".to_string());

        // Read-only root filesystem
        args.push("--read-only".to_string());

        // Add image and command
        args.push(self.config.image.clone());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        let result = Command::new("docker")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        Self::process_output(job_id, result)
    }

    fn process_output(
        job_id: Uuid,
        result: Result<std::process::Output, std::io::Error>,
    ) -> ExecutionResult {
        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code();

                let (status, error) = if output.status.success() {
                    (JobStatus::Completed, None)
                } else {
                    (
                        JobStatus::Failed,
                        Some(if stderr.is_empty() {
                            format!("Exit code: {:?}", exit_code)
                        } else {
                            stderr.clone()
                        }),
                    )
                };

                tracing::info!(
                    job_id = %job_id,
                    status = %status,
                    exit_code = ?exit_code,
                    "Job completed"
                );

                ExecutionResult {
                    job_id,
                    status,
                    exit_code,
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
                    exit_code: None,
                    output: None,
                    error: Some(e.to_string()),
                }
            }
        }
    }
}
