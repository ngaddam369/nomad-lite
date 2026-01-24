use chrono::Utc;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::Request;
use uuid::Uuid;

use nomad_lite::grpc::internal_service::InternalServiceImpl;
use nomad_lite::proto::internal_service_server::InternalService;
use nomad_lite::proto::GetJobOutputRequest;
use nomad_lite::scheduler::job::{Job, JobStatus};
use nomad_lite::scheduler::queue::JobQueue;

/// Helper to create a test service with a job queue
fn create_test_service(node_id: u64) -> (InternalServiceImpl, Arc<RwLock<JobQueue>>) {
    let queue = Arc::new(RwLock::new(JobQueue::new()));
    let service = InternalServiceImpl::new(queue.clone(), node_id);
    (service, queue)
}

/// Test get_job_output returns output when this node executed the job.
#[tokio::test]
async fn test_get_job_output_executed_by_this_node() {
    let (service, queue) = create_test_service(1);

    // Add a job that was executed by this node
    let job_id = Uuid::new_v4();
    {
        let mut q = queue.write().await;
        q.add_job(Job::with_id(job_id, "echo hello".to_string(), Utc::now()));
        q.update_job_result(
            &job_id,
            JobStatus::Completed,
            1, // executed_by this node
            Some(0),
            Some("hello\n".to_string()),
            None,
            Utc::now(),
        );
    }

    let request = Request::new(GetJobOutputRequest {
        job_id: job_id.to_string(),
    });

    let response = service.get_job_output(request).await.unwrap();
    let resp = response.into_inner();

    assert!(resp.found);
    assert_eq!(resp.job_id, job_id.to_string());
    assert_eq!(resp.output, "hello\n");
    assert_eq!(resp.error, "");
}

/// Test get_job_output returns found=false when job was executed by another node.
#[tokio::test]
async fn test_get_job_output_executed_by_other_node() {
    let (service, queue) = create_test_service(1);

    // Add a job that was executed by a different node
    let job_id = Uuid::new_v4();
    {
        let mut q = queue.write().await;
        q.add_job(Job::with_id(job_id, "echo hello".to_string(), Utc::now()));
        q.update_job_result(
            &job_id,
            JobStatus::Completed,
            2, // executed_by different node
            Some(0),
            Some("hello\n".to_string()),
            None,
            Utc::now(),
        );
    }

    let request = Request::new(GetJobOutputRequest {
        job_id: job_id.to_string(),
    });

    let response = service.get_job_output(request).await.unwrap();
    let resp = response.into_inner();

    // Should return found=false since this node didn't execute it
    assert!(!resp.found);
    assert_eq!(resp.output, "");
    assert_eq!(resp.error, "");
}

/// Test get_job_output returns error when job doesn't exist.
#[tokio::test]
async fn test_get_job_output_job_not_found() {
    let (service, _queue) = create_test_service(1);

    let nonexistent_id = Uuid::new_v4();
    let request = Request::new(GetJobOutputRequest {
        job_id: nonexistent_id.to_string(),
    });

    let result = service.get_job_output(request).await;
    assert!(result.is_err());

    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(status.message(), "Job not found");
}

/// Test get_job_output returns error for invalid job ID format.
#[tokio::test]
async fn test_get_job_output_invalid_job_id() {
    let (service, _queue) = create_test_service(1);

    let request = Request::new(GetJobOutputRequest {
        job_id: "not-a-valid-uuid".to_string(),
    });

    let result = service.get_job_output(request).await;
    assert!(result.is_err());

    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(status.message(), "Invalid job ID");
}

/// Test get_job_output returns both output and error fields.
#[tokio::test]
async fn test_get_job_output_with_output_and_error() {
    let (service, queue) = create_test_service(1);

    let job_id = Uuid::new_v4();
    {
        let mut q = queue.write().await;
        q.add_job(Job::with_id(job_id, "cmd".to_string(), Utc::now()));
        q.update_job_result(
            &job_id,
            JobStatus::Completed,
            1,
            Some(0),
            Some("stdout content".to_string()),
            Some("stderr content".to_string()),
            Utc::now(),
        );
    }

    let request = Request::new(GetJobOutputRequest {
        job_id: job_id.to_string(),
    });

    let response = service.get_job_output(request).await.unwrap();
    let resp = response.into_inner();

    assert!(resp.found);
    assert_eq!(resp.output, "stdout content");
    assert_eq!(resp.error, "stderr content");
}

/// Test get_job_output handles empty output gracefully.
#[tokio::test]
async fn test_get_job_output_empty_output() {
    let (service, queue) = create_test_service(1);

    let job_id = Uuid::new_v4();
    {
        let mut q = queue.write().await;
        q.add_job(Job::with_id(job_id, "true".to_string(), Utc::now()));
        q.update_job_result(
            &job_id,
            JobStatus::Completed,
            1,
            Some(0),
            None, // No output
            None,
            Utc::now(),
        );
    }

    let request = Request::new(GetJobOutputRequest {
        job_id: job_id.to_string(),
    });

    let response = service.get_job_output(request).await.unwrap();
    let resp = response.into_inner();

    assert!(resp.found);
    assert_eq!(resp.output, ""); // Empty string, not None
    assert_eq!(resp.error, "");
}

/// Test get_job_output for a job that hasn't been executed yet (no executed_by).
#[tokio::test]
async fn test_get_job_output_job_not_executed() {
    let (service, queue) = create_test_service(1);

    // Add a pending job (not yet executed)
    let job_id = Uuid::new_v4();
    {
        let mut q = queue.write().await;
        q.add_job(Job::with_id(job_id, "echo test".to_string(), Utc::now()));
        // Job is pending, executed_by is None
    }

    let request = Request::new(GetJobOutputRequest {
        job_id: job_id.to_string(),
    });

    let response = service.get_job_output(request).await.unwrap();
    let resp = response.into_inner();

    // Should return found=false since executed_by is None (doesn't match node_id)
    assert!(!resp.found);
}
