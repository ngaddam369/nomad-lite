use std::collections::HashMap;

use chrono::{Duration, Utc};
use nomad_lite::raft::state::Command;
use nomad_lite::scheduler::assigner::{JobAssigner, WorkerState};
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
            created_at: Utc::now(),
        },
        Command::SubmitJob {
            job_id: job_id_2,
            command: "echo second".to_string(),
            created_at: Utc::now(),
        },
    ];

    // Apply committed entries to job queue (simulating scheduler loop behavior)
    for entry in committed_entries {
        if let Command::SubmitJob {
            job_id,
            command,
            created_at,
        } = entry
        {
            if queue.get_job(&job_id).is_none() {
                queue.add_job(Job::with_id(job_id, command, created_at));
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
    queue.add_job(Job::with_id(job_id, "echo test".to_string(), Utc::now()));

    // Simulate receiving a committed UpdateJobStatus entry
    // Note: Output is now stored locally, not replicated through Raft
    let status_update = Command::UpdateJobStatus {
        job_id,
        status: JobStatus::Completed,
        executed_by: 1,
        exit_code: Some(0),
        completed_at: Some(Utc::now()),
    };

    // Apply the status update (simulating scheduler loop behavior)
    if let Command::UpdateJobStatus {
        job_id,
        status,
        executed_by,
        exit_code,
        completed_at,
    } = status_update
    {
        queue.update_status_metadata(&job_id, status, executed_by, exit_code, completed_at);
    }

    // Verify status was updated
    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.executed_by, Some(1));
    assert_eq!(job.exit_code, Some(0));
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
        created_at: Utc::now(),
    };

    // Apply the same entry twice (simulating potential redelivery)
    for _ in 0..2 {
        if let Command::SubmitJob {
            job_id,
            command,
            created_at,
        } = &command
        {
            if queue.get_job(job_id).is_none() {
                queue.add_job(Job::with_id(*job_id, command.clone(), *created_at));
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

// ==================== Tests for update_job_result() ====================

/// Test that update_job_result() stores all fields including output locally.
/// This is used by the executing node to store the full execution result.
#[test]
fn test_update_job_result_stores_all_fields() {
    let mut queue = JobQueue::new();

    let job_id = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id, "echo hello".to_string(), Utc::now()));

    // Update with full result (as the executing node would)
    let updated = queue.update_job_result(
        &job_id,
        JobStatus::Completed,
        1,                           // executed_by
        Some(0),                     // exit_code
        Some("hello\n".to_string()), // output
        None,                        // error
        Utc::now(),
    );
    assert!(updated);

    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.executed_by, Some(1));
    assert_eq!(job.exit_code, Some(0));
    assert_eq!(job.output, Some("hello\n".to_string()));
    assert_eq!(job.error, None);
}

/// Test update_job_result() with a failed job including error message.
#[test]
fn test_update_job_result_with_failure() {
    let mut queue = JobQueue::new();

    let job_id = Uuid::new_v4();
    queue.add_job(Job::with_id(
        job_id,
        "invalid_command".to_string(),
        Utc::now(),
    ));

    let updated = queue.update_job_result(
        &job_id,
        JobStatus::Failed,
        2,                                     // executed_by
        Some(127),                             // exit_code (command not found)
        None,                                  // output
        Some("command not found".to_string()), // error
        Utc::now(),
    );
    assert!(updated);

    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(job.executed_by, Some(2));
    assert_eq!(job.exit_code, Some(127));
    assert_eq!(job.output, None);
    assert_eq!(job.error, Some("command not found".to_string()));
}

/// Test update_job_result() returns false for non-existent job.
#[test]
fn test_update_job_result_nonexistent_job() {
    let mut queue = JobQueue::new();

    let nonexistent_id = Uuid::new_v4();
    let updated = queue.update_job_result(
        &nonexistent_id,
        JobStatus::Completed,
        1,
        Some(0),
        Some("output".to_string()),
        None,
        Utc::now(),
    );
    assert!(!updated);
}

/// Test update_job_result() with both output and error (edge case).
#[test]
fn test_update_job_result_with_output_and_error() {
    let mut queue = JobQueue::new();

    let job_id = Uuid::new_v4();
    queue.add_job(Job::with_id(
        job_id,
        "echo hello >&2; echo world".to_string(),
        Utc::now(),
    ));

    let updated = queue.update_job_result(
        &job_id,
        JobStatus::Completed,
        3,
        Some(0),
        Some("world\n".to_string()), // stdout
        Some("hello\n".to_string()), // stderr (captured as error)
        Utc::now(),
    );
    assert!(updated);

    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.output, Some("world\n".to_string()));
    assert_eq!(job.error, Some("hello\n".to_string()));
}

// ==================== Tests for update_status_metadata() ====================

/// Test that update_status_metadata() does NOT update output field.
/// Output is stored locally on the executing node, not replicated.
#[test]
fn test_update_status_metadata_does_not_update_output() {
    let mut queue = JobQueue::new();

    let job_id = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id, "echo test".to_string(), Utc::now()));

    // First, set output locally (simulating executing node)
    queue.update_job_result(
        &job_id,
        JobStatus::Running,
        1,
        None,
        Some("local output".to_string()),
        None,
        Utc::now(),
    );

    // Now apply metadata update (simulating Raft replication)
    // This should NOT overwrite the output
    queue.update_status_metadata(&job_id, JobStatus::Completed, 1, Some(0), Some(Utc::now()));

    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    // Output should be preserved (not overwritten by metadata update)
    assert_eq!(job.output, Some("local output".to_string()));
}

/// Test that update_status_metadata() does NOT update error field.
#[test]
fn test_update_status_metadata_does_not_update_error() {
    let mut queue = JobQueue::new();

    let job_id = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id, "bad_command".to_string(), Utc::now()));

    // Set error locally
    queue.update_job_result(
        &job_id,
        JobStatus::Running,
        1,
        None,
        None,
        Some("local error".to_string()),
        Utc::now(),
    );

    // Apply metadata update
    queue.update_status_metadata(&job_id, JobStatus::Failed, 1, Some(1), Some(Utc::now()));

    let job = queue.get_job(&job_id).unwrap();
    assert_eq!(job.status, JobStatus::Failed);
    // Error should be preserved
    assert_eq!(job.error, Some("local error".to_string()));
}

/// Test update_status_metadata() with various exit codes.
#[test]
fn test_update_status_metadata_exit_codes() {
    let mut queue = JobQueue::new();

    // Test with exit code 0 (success)
    let job_id1 = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id1, "true".to_string(), Utc::now()));
    queue.update_status_metadata(&job_id1, JobStatus::Completed, 1, Some(0), Some(Utc::now()));
    assert_eq!(queue.get_job(&job_id1).unwrap().exit_code, Some(0));

    // Test with exit code 1 (general error)
    let job_id2 = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id2, "false".to_string(), Utc::now()));
    queue.update_status_metadata(&job_id2, JobStatus::Failed, 1, Some(1), Some(Utc::now()));
    assert_eq!(queue.get_job(&job_id2).unwrap().exit_code, Some(1));

    // Test with exit code 127 (command not found)
    let job_id3 = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id3, "nonexistent".to_string(), Utc::now()));
    queue.update_status_metadata(&job_id3, JobStatus::Failed, 1, Some(127), Some(Utc::now()));
    assert_eq!(queue.get_job(&job_id3).unwrap().exit_code, Some(127));

    // Test with None exit code (e.g., killed by signal)
    let job_id4 = Uuid::new_v4();
    queue.add_job(Job::with_id(job_id4, "killed".to_string(), Utc::now()));
    queue.update_status_metadata(&job_id4, JobStatus::Failed, 1, None, Some(Utc::now()));
    assert_eq!(queue.get_job(&job_id4).unwrap().exit_code, None);
}

/// Test update_status_metadata() returns false for non-existent job.
#[test]
fn test_update_status_metadata_nonexistent_job() {
    let mut queue = JobQueue::new();

    let nonexistent_id = Uuid::new_v4();
    let updated = queue.update_status_metadata(
        &nonexistent_id,
        JobStatus::Completed,
        1,
        Some(0),
        Some(Utc::now()),
    );
    assert!(!updated);
}

// ==================== Tests for Job struct new fields ====================

/// Test that Job struct initializes executed_by and exit_code as None.
#[test]
fn test_job_new_fields_initialized() {
    let job = Job::new("echo test".to_string());
    assert_eq!(job.executed_by, None);
    assert_eq!(job.exit_code, None);

    let job_with_id = Job::with_id(Uuid::new_v4(), "echo test".to_string(), Utc::now());
    assert_eq!(job_with_id.executed_by, None);
    assert_eq!(job_with_id.exit_code, None);
}

// ==================== job.rs coverage tests ====================

#[test]
fn test_job_status_display() {
    assert_eq!(format!("{}", JobStatus::Pending), "pending");
    assert_eq!(format!("{}", JobStatus::Running), "running");
    assert_eq!(format!("{}", JobStatus::Completed), "completed");
    assert_eq!(format!("{}", JobStatus::Failed), "failed");
}

#[test]
fn test_job_serde_round_trip() {
    let original = Job::new("echo round-trip".to_string());
    let json = serde_json::to_string(&original).expect("serialize");
    let deserialized: Job = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(deserialized.id, original.id);
    assert_eq!(deserialized.command, original.command);
    assert_eq!(deserialized.status, original.status);
    assert_eq!(deserialized.assigned_worker, original.assigned_worker);
    assert_eq!(deserialized.executed_by, original.executed_by);
    assert_eq!(deserialized.exit_code, original.exit_code);
    assert_eq!(deserialized.output, original.output);
    assert_eq!(deserialized.error, original.error);
    assert_eq!(deserialized.created_at, original.created_at);
    assert_eq!(deserialized.completed_at, original.completed_at);
}

#[test]
fn test_job_with_id_preserves_id_and_created_at() {
    let id = Uuid::new_v4();
    let created_at = Utc::now() - Duration::hours(3);
    let job = Job::with_id(id, "echo preserved".to_string(), created_at);

    assert_eq!(job.id, id);
    assert_eq!(job.created_at, created_at);
    assert_eq!(job.command, "echo preserved");
    assert_eq!(job.status, JobStatus::Pending);
}

// ==================== queue.rs coverage tests ====================

#[test]
fn test_job_queue_default_impl() {
    let queue = JobQueue::default();
    assert!(queue.is_empty());
    assert!(!queue.is_full());
}

#[test]
fn test_get_job_mut() {
    let mut queue = JobQueue::new();

    let job = Job::new("echo mutable".to_string());
    let id = job.id;
    queue.add_job(job);

    // Mutate through get_job_mut
    let job_mut = queue.get_job_mut(&id).unwrap();
    job_mut.output = Some("mutated".to_string());

    // Verify mutation persisted
    let job = queue.get_job(&id).unwrap();
    assert_eq!(job.output, Some("mutated".to_string()));

    // Non-existent ID returns None
    let missing = Uuid::new_v4();
    assert!(queue.get_job_mut(&missing).is_none());
}

#[test]
fn test_assign_job_direct() {
    let mut queue = JobQueue::new();

    let job = Job::new("echo assign".to_string());
    let id = job.id;
    queue.add_job(job);

    assert!(queue.assign_job(&id, 42));

    let job = queue.get_job(&id).unwrap();
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.assigned_worker, Some(42));
}

#[test]
fn test_assign_job_nonexistent() {
    let mut queue = JobQueue::new();
    let missing = Uuid::new_v4();
    assert!(!queue.assign_job(&missing, 1));
}

#[test]
fn test_update_status_nonexistent_job() {
    let mut queue = JobQueue::new();
    let missing = Uuid::new_v4();
    assert!(!queue.update_status(&missing, JobStatus::Completed, None, None));
}

#[test]
fn test_update_status_none_preserves_existing_values() {
    let mut queue = JobQueue::new();

    let job = Job::new("echo preserve".to_string());
    let id = job.id;
    queue.add_job(job);

    // Set output and error via update_status
    queue.update_status(
        &id,
        JobStatus::Running,
        Some("initial output".to_string()),
        Some("initial error".to_string()),
    );

    // Update status again with None output/error — should NOT overwrite
    queue.update_status(&id, JobStatus::Completed, None, None);

    let job = queue.get_job(&id).unwrap();
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(job.output, Some("initial output".to_string()));
    assert_eq!(job.error, Some("initial error".to_string()));
}

#[test]
fn test_all_jobs_sort_order() {
    let mut queue = JobQueue::new();

    let now = Utc::now();
    let job_old = Job::with_id(
        Uuid::new_v4(),
        "oldest".to_string(),
        now - Duration::hours(2),
    );
    let job_mid = Job::with_id(
        Uuid::new_v4(),
        "middle".to_string(),
        now - Duration::hours(1),
    );
    let job_new = Job::with_id(Uuid::new_v4(), "newest".to_string(), now);

    // Add in non-chronological order
    queue.add_job(job_mid);
    queue.add_job(job_new);
    queue.add_job(job_old);

    let all = queue.all_jobs();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].command, "oldest");
    assert_eq!(all[1].command, "middle");
    assert_eq!(all[2].command, "newest");
}

#[test]
fn test_jobs_for_nonexistent_worker_returns_empty() {
    let mut queue = JobQueue::new();
    queue.add_job(Job::new("echo solo".to_string()));

    let jobs = queue.jobs_for_worker(999);
    assert!(jobs.is_empty());
}

#[test]
fn test_cleanup_finished_jobs_edge_cases() {
    // Empty queue
    let mut queue = JobQueue::new();
    assert_eq!(queue.cleanup_finished_jobs(), 0);

    // Queue with only pending/running jobs — nothing to clean
    let mut job1 = Job::new("echo 1".to_string());
    let mut job2 = Job::new("echo 2".to_string());
    job1.status = JobStatus::Pending;
    job2.status = JobStatus::Running;
    queue.add_job(job1);
    queue.add_job(job2);

    assert_eq!(queue.cleanup_finished_jobs(), 0);
    assert_eq!(queue.len(), 2);
}

// ==================== assigner.rs coverage tests ====================

#[test]
fn test_worker_state_new_direct() {
    let ws = WorkerState::new(7);
    assert_eq!(ws.id, 7);
    assert!(ws.running_jobs.is_empty());
    assert!(ws.is_alive(5000));
}

#[test]
fn test_worker_state_is_alive_direct() {
    let ws = WorkerState::new(1);
    // Just created — should be alive with a generous timeout
    assert!(ws.is_alive(5000));

    // With a zero timeout it should be considered dead immediately
    // (elapsed > 0 since creation)
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert!(!ws.is_alive(0));
}

#[test]
fn test_worker_heartbeat_auto_registration() {
    let mut assigner = JobAssigner::new(5000);

    // No workers yet
    assert!(assigner.available_workers().is_empty());

    // Heartbeat from unknown worker should auto-register it
    assigner.worker_heartbeat(42);
    assert_eq!(assigner.available_workers().len(), 1);
    assert!(assigner.available_workers().contains(&42));
}

#[test]
fn test_assign_next_job_least_loaded_balancing() {
    let mut queue = JobQueue::new();
    let mut assigner = JobAssigner::new(5000);

    assigner.register_worker(1);
    assigner.register_worker(2);

    // Add 4 jobs and assign them all
    for i in 0..4 {
        queue.add_job(Job::new(format!("echo {i}")));
    }

    let mut assignments: HashMap<u64, usize> = HashMap::new();
    for _ in 0..4 {
        let (_, worker_id) = assigner.assign_next_job(&mut queue).unwrap();
        *assignments.entry(worker_id).or_default() += 1;
    }

    // Each worker should get exactly 2 jobs (least-loaded balancing)
    assert_eq!(*assignments.get(&1).unwrap_or(&0), 2);
    assert_eq!(*assignments.get(&2).unwrap_or(&0), 2);
}

#[test]
fn test_assign_next_job_no_pending_jobs() {
    let mut queue = JobQueue::new();
    let mut assigner = JobAssigner::new(5000);

    assigner.register_worker(1);

    // No jobs in queue
    assert!(assigner.assign_next_job(&mut queue).is_none());
}

#[test]
fn test_all_workers() {
    let mut assigner = JobAssigner::new(5000);

    assigner.register_worker(10);
    assigner.register_worker(20);
    assigner.register_worker(30);

    let workers = assigner.all_workers();
    assert_eq!(workers.len(), 3);

    let ids: Vec<u64> = workers.iter().map(|w| w.id).collect();
    assert!(ids.contains(&10));
    assert!(ids.contains(&20));
    assert!(ids.contains(&30));
}

#[test]
fn test_check_dead_workers() {
    let mut assigner = JobAssigner::new(100); // 100ms timeout

    assigner.register_worker(1);
    assigner.register_worker(2);

    // Let timeout expire
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Heartbeat only worker 1
    assigner.worker_heartbeat(1);

    let dead = assigner.check_dead_workers();
    assert_eq!(dead.len(), 1);
    assert!(dead.contains(&2));
    assert!(!dead.contains(&1));
}

#[test]
fn test_job_completed_nonexistent_worker() {
    let mut assigner = JobAssigner::new(5000);
    let job_id = Uuid::new_v4();

    // Should not panic
    assigner.job_completed(999, &job_id);
}
