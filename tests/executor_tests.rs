use nomad_lite::config::SandboxConfig;
use nomad_lite::scheduler::JobStatus;
use nomad_lite::worker::JobExecutor;
use uuid::Uuid;

/// Create a test executor with default sandbox config
fn test_executor() -> JobExecutor {
    JobExecutor::new(SandboxConfig::default())
}

#[tokio::test]
async fn test_execute_simple_command() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    let result = executor.execute(job_id, "echo hello").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Completed);
    assert_eq!(result.output, Some("hello\n".to_string()));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn test_execute_empty_output() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    // Command that produces no output
    let result = executor.execute(job_id, "true").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Completed);
    assert!(result.output.is_none()); // Empty output should be None
    assert!(result.error.is_none());
}

#[tokio::test]
async fn test_execute_large_output() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    // Generate large output (1000 lines)
    let result = executor.execute(job_id, "seq 1 1000").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Completed);
    assert!(result.output.is_some());

    let output = result.output.unwrap();
    let line_count = output.lines().count();
    assert_eq!(line_count, 1000);
}

#[tokio::test]
async fn test_execute_command_failure() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    // Command that exits with non-zero status
    let result = executor.execute(job_id, "exit 1").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Failed);
    assert!(result.error.is_some());
}

#[tokio::test]
async fn test_execute_command_with_stderr() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    // Command that writes to stderr and fails
    let result = executor
        .execute(job_id, "echo 'error message' >&2 && exit 1")
        .await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Failed);
    assert!(result.error.is_some());
    assert!(result.error.unwrap().contains("error message"));
}

#[tokio::test]
async fn test_execute_invalid_command() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    // Command that doesn't exist
    let result = executor.execute(job_id, "nonexistent_command_12345").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Failed);
    assert!(result.error.is_some());
}

#[tokio::test]
async fn test_execute_multiline_output() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    let result = executor
        .execute(job_id, "echo -e 'line1\\nline2\\nline3'")
        .await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Completed);
    assert!(result.output.is_some());

    let output = result.output.unwrap();
    assert_eq!(output.lines().count(), 3);
}

#[tokio::test]
async fn test_execute_with_special_characters() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    // Command with special characters
    let result = executor.execute(job_id, "echo 'hello $USER'").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Completed);
    // Single quotes prevent variable expansion
    assert_eq!(result.output, Some("hello $USER\n".to_string()));
}

#[tokio::test]
async fn test_execute_piped_commands() {
    let executor = test_executor();
    let job_id = Uuid::new_v4();

    let result = executor.execute(job_id, "echo 'hello world' | wc -w").await;

    assert_eq!(result.job_id, job_id);
    assert_eq!(result.status, JobStatus::Completed);
    assert!(result.output.is_some());
    // Output should be "2" (word count)
    assert!(result.output.unwrap().trim() == "2");
}
