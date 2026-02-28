use std::process::Stdio;
use std::time::Duration;
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

    /// Execute a job command in a sandboxed Docker container.
    ///
    /// `image` overrides the server-default image for this specific job.
    /// If `SandboxConfig::timeout_secs` is set, the container is force-removed
    /// and the job is marked `Failed` with a "Timed out" error when the limit
    /// is exceeded.
    pub async fn execute(
        &self,
        job_id: Uuid,
        command: &str,
        image: Option<&str>,
    ) -> ExecutionResult {
        let image = image.unwrap_or(&self.config.image);
        tracing::info!(job_id = %job_id, command, image, "Executing job");

        // Unique container name lets us force-remove it on timeout.
        let container_name = format!("nomad-{}", job_id.as_simple());

        let mut args = vec!["run".to_string(), "--rm".to_string()];

        // Named container for cleanup on timeout
        args.push("--name".to_string());
        args.push(container_name.clone());

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
        args.push(image.to_string());
        args.push("sh".to_string());
        args.push("-c".to_string());
        args.push(command.to_string());

        let child = Command::new("docker")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();

        let child = match child {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(job_id = %job_id, error = %e, "Failed to spawn docker");
                return ExecutionResult {
                    job_id,
                    status: JobStatus::Failed,
                    exit_code: None,
                    output: None,
                    error: Some(e.to_string()),
                };
            }
        };

        let timeout_secs = self.config.timeout_secs;
        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
        {
            Ok(result) => Self::process_output(job_id, result),
            Err(_elapsed) => {
                tracing::warn!(
                    job_id = %job_id,
                    timeout_secs,
                    "Job timed out, force-removing container"
                );
                // kill_on_drop fires when child is dropped above; also
                // force-remove the named container to handle the case where
                // the docker CLI detaches from a still-running container.
                let _ = Command::new("docker")
                    .args(["rm", "-f", &container_name])
                    .output()
                    .await;
                ExecutionResult {
                    job_id,
                    status: JobStatus::Failed,
                    exit_code: None,
                    output: None,
                    error: Some(format!("Timed out after {}s", timeout_secs)),
                }
            }
        }
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
