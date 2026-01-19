use nomad_lite::raft::state::Command;
use nomad_lite::scheduler::assigner::JobAssigner;
use nomad_lite::scheduler::job::{Job, JobStatus};
use nomad_lite::scheduler::queue::JobQueue;
use uuid::Uuid;

#[test]
fn test_job_creation() {
    let job = Job::new("echo hello".to_string());
    assert_eq!(job.status, JobStatus::Pending);
    assert_eq!(job.command, "echo hello");
    assert!(job.assigned_worker.is_none());
}

#[test]
fn test_job_queue_operations() {
    let mut queue = JobQueue::new();

    let job1 = Job::new("echo 1".to_string());
    let job2 = Job::new("echo 2".to_string());
    let id1 = job1.id;

    queue.add_job(job1);
    queue.add_job(job2);

    assert_eq!(queue.all_jobs().len(), 2);
    assert_eq!(queue.pending_jobs().len(), 2);

    // Get job
    let retrieved = queue.get_job(&id1).unwrap();
    assert_eq!(retrieved.command, "echo 1");

    // Update status
    queue.update_status(&id1, JobStatus::Completed, Some("output".to_string()), None);
    let updated = queue.get_job(&id1).unwrap();
    assert_eq!(updated.status, JobStatus::Completed);
    assert_eq!(updated.output, Some("output".to_string()));

    // Pending jobs should be reduced
    assert_eq!(queue.pending_jobs().len(), 1);
}

#[test]
fn test_job_assignment() {
    let mut queue = JobQueue::new();
    let mut assigner = JobAssigner::new(5000);

    // Register workers
    assigner.register_worker(1);
    assigner.register_worker(2);

    // Add jobs
    let job1 = Job::new("echo 1".to_string());
    let id1 = job1.id;
    queue.add_job(job1);

    // Assign job
    let assignment = assigner.assign_next_job(&mut queue);
    assert!(assignment.is_some());
    let (assigned_job_id, assigned_worker) = assignment.unwrap();
    assert_eq!(assigned_job_id, id1);

    // Check job is now running
    let job = queue.get_job(&id1).unwrap();
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.assigned_worker, Some(assigned_worker));
}

#[test]
fn test_no_workers_available() {
    let mut queue = JobQueue::new();
    let mut assigner = JobAssigner::new(5000);

    // Add job but no workers
    queue.add_job(Job::new("echo 1".to_string()));

    let assignment = assigner.assign_next_job(&mut queue);
    assert!(assignment.is_none());
}

#[test]
fn test_worker_heartbeat() {
    let mut assigner = JobAssigner::new(100); // 100ms timeout

    assigner.register_worker(1);
    assert_eq!(assigner.available_workers().len(), 1);

    // Simulate heartbeat
    assigner.worker_heartbeat(1);
    assert_eq!(assigner.available_workers().len(), 1);

    // Sleep longer than timeout
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(assigner.available_workers().len(), 0);
}

#[test]
fn test_job_completion() {
    let mut queue = JobQueue::new();
    let mut assigner = JobAssigner::new(5000);

    assigner.register_worker(1);

    let job = Job::new("echo test".to_string());
    let job_id = job.id;
    queue.add_job(job);

    // Assign and complete
    let (assigned_id, worker_id) = assigner.assign_next_job(&mut queue).unwrap();
    assigner.job_completed(worker_id, &assigned_id);

    queue.update_status(
        &job_id,
        JobStatus::Completed,
        Some("test\n".to_string()),
        None,
    );

    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.output, Some("test\n".to_string()));
}

#[test]
fn test_jobs_for_worker() {
    let mut queue = JobQueue::new();

    let mut job1 = Job::new("echo 1".to_string());
    let mut job2 = Job::new("echo 2".to_string());
    let mut job3 = Job::new("echo 3".to_string());

    job1.assigned_worker = Some(1);
    job2.assigned_worker = Some(1);
    job3.assigned_worker = Some(2);

    queue.add_job(job1);
    queue.add_job(job2);
    queue.add_job(job3);

    let worker1_jobs = queue.jobs_for_worker(1);
    assert_eq!(worker1_jobs.len(), 2);

    let worker2_jobs = queue.jobs_for_worker(2);
    assert_eq!(worker2_jobs.len(), 1);
}

/// Test that simulates applying committed Raft log entries to a job queue.
/// This verifies the behavior that was fixed - all nodes (including followers)
/// should apply committed entries to maintain consistent state.
#[test]
fn test_apply_committed_entries_to_queue() {
    let mut queue = JobQueue::new();

    // Simulate committed entries from Raft log (as a follower would receive)
    let job_id_1 = Uuid::new_v4();
    let job_id_2 = Uuid::new_v4();

    let committed_entries = vec![
        Command::SubmitJob {
            job_id: job_id_1,
            command: "echo first".to_string(),
        },
        Command::SubmitJob {
            job_id: job_id_2,
            command: "echo second".to_string(),
        },
    ];

    // Apply committed entries to job queue (simulating scheduler loop behavior)
    for entry in committed_entries {
        if let Command::SubmitJob { job_id, command } = entry {
            if queue.get_job(&job_id).is_none() {
                queue.add_job(Job::with_id(job_id, command));
            }
        }
    }

    // Verify jobs were added
    assert_eq!(queue.all_jobs().len(), 2);
    assert!(queue.get_job(&job_id_1).is_some());
    assert!(queue.get_job(&job_id_2).is_some());

    let job1 = queue.get_job(&job_id_1).unwrap();
    assert_eq!(job1.command, "echo first");
    assert_eq!(job1.status, JobStatus::Pending);
}

/// Test that UpdateJobStatus commands are properly applied to the queue.
/// This ensures followers can replicate job status updates.
#[test]
fn test_apply_job_status_update_from_committed_entry() {
    let mut queue = JobQueue::new();

    // First, add a job
    let job_id = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id, "echo test".to_string()));

    // Simulate receiving a committed UpdateJobStatus entry
    let status_update = Command::UpdateJobStatus {
        job_id,
        status: JobStatus::Completed,
        output: Some("test output".to_string()),
        error: None,
    };

    // Apply the status update (simulating scheduler loop behavior)
    if let Command::UpdateJobStatus {
        job_id,
        status,
        output,
        error,
    } = status_update
    {
        queue.update_status(&job_id, status, output, error);
    }

    // Verify status was updated
    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.output, Some("test output".to_string()));
}

/// Test that idempotent application of entries works correctly.
/// If the same entry is applied twice, it should not duplicate jobs.
#[test]
fn test_idempotent_entry_application() {
    let mut queue = JobQueue::new();

    let job_id = Uuid::new_v4();
    let command = Command::SubmitJob {
        job_id,
        command: "echo test".to_string(),
    };

    // Apply the same entry twice (simulating potential redelivery)
    for _ in 0..2 {
        if let Command::SubmitJob { job_id, command } = &command {
            if queue.get_job(job_id).is_none() {
                queue.add_job(Job::with_id(*job_id, command.clone()));
            }
        }
    }

    // Should only have one job
    assert_eq!(queue.all_jobs().len(), 1);
}

#[test]
fn test_queue_capacity_limit() {
    let mut queue = JobQueue::with_capacity(3);

    // Add jobs up to capacity
    assert!(queue.add_job(Job::new("echo 1".to_string())));
    assert!(queue.add_job(Job::new("echo 2".to_string())));
    assert!(queue.add_job(Job::new("echo 3".to_string())));

    // Queue should be full
    assert!(queue.is_full());
    assert_eq!(queue.len(), 3);

    // Adding more jobs should fail
    assert!(!queue.add_job(Job::new("echo 4".to_string())));
    assert_eq!(queue.len(), 3);
}

#[test]
fn test_cleanup_finished_jobs() {
    let mut queue = JobQueue::new();

    // Add jobs with different statuses
    let mut job1 = Job::new("echo 1".to_string());
    let mut job2 = Job::new("echo 2".to_string());
    let mut job3 = Job::new("echo 3".to_string());
    let mut job4 = Job::new("echo 4".to_string());

    job1.status = JobStatus::Pending;
    job2.status = JobStatus::Running;
    job3.status = JobStatus::Completed;
    job4.status = JobStatus::Failed;

    queue.add_job(job1);
    queue.add_job(job2);
    queue.add_job(job3);
    queue.add_job(job4);

    assert_eq!(queue.len(), 4);

    // Cleanup should remove completed and failed jobs
    let removed = queue.cleanup_finished_jobs();
    assert_eq!(removed, 2);
    assert_eq!(queue.len(), 2);

    // Only pending and running jobs should remain
    for job in queue.all_jobs() {
        assert!(job.status == JobStatus::Pending || job.status == JobStatus::Running);
    }
}

#[test]
fn test_queue_helper_methods() {
    let mut queue = JobQueue::with_capacity(2);

    assert!(queue.is_empty());
    assert!(!queue.is_full());
    assert_eq!(queue.len(), 0);

    queue.add_job(Job::new("echo 1".to_string()));

    assert!(!queue.is_empty());
    assert!(!queue.is_full());
    assert_eq!(queue.len(), 1);

    queue.add_job(Job::new("echo 2".to_string()));

    assert!(!queue.is_empty());
    assert!(queue.is_full());
    assert_eq!(queue.len(), 2);
}
