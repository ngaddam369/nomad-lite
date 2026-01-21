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
    pub output: Option<String>,
    pub error: Option<String>,
}

/// Executes jobs by running shell commands
#[derive(Debug, Clone, Default)]
pub struct JobExecutor {
    sandbox: SandboxConfig,
}

impl JobExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_sandbox(sandbox: SandboxConfig) -> Self {
        Self { sandbox }
    }

    /// Execute a job command and return the result
    pub async fn execute(&self, job_id: Uuid, command: &str) -> ExecutionResult {
        if self.sandbox.enabled {
            self.execute_sandboxed(job_id, command).await
        } else {
            self.execute_direct(job_id, command).await
        }
    }

    /// Execute command directly on the host (unsafe, for backwards compatibility)
    async fn execute_direct(&self, job_id: Uuid, command: &str) -> ExecutionResult {
        tracing::info!(job_id = %job_id, command, "Executing job (direct)");

        let result = Command::new("sh")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        Self::process_output(job_id, result)
    }

    /// Execute command in a Docker container (sandboxed)
    async fn execute_sandboxed(&self, job_id: Uuid, command: &str) -> ExecutionResult {
        tracing::info!(job_id = %job_id, command, image = %self.sandbox.image, "Executing job (sandboxed)");

        let mut args = vec!["run".to_string(), "--rm".to_string()];

        // Network isolation
        if self.sandbox.network_disabled {
            args.push("--network=none".to_string());
        }

        // Memory limit
        if let Some(ref limit) = self.sandbox.memory_limit {
            args.push(format!("--memory={}", limit));
        }

        // CPU limit
        if let Some(ref limit) = self.sandbox.cpu_limit {
            args.push(format!("--cpus={}", limit));
        }

        // Security: drop all capabilities, no new privileges
        args.push("--cap-drop=ALL".to_string());
        args.push("--security-opt=no-new-privileges".to_string());

        // Read-only root filesystem
        args.push("--read-only".to_string());

        // Add image and command
        args.push(self.sandbox.image.clone());
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
