use std::collections::HashMap;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::scheduler::job::{Job, JobStatus};

const DEFAULT_MAX_JOBS: usize = 10_000;

/// Manages the job queue and job state
#[derive(Debug)]
pub struct JobQueue {
    jobs: HashMap<Uuid, Job>,
    max_jobs: usize,
}

impl Default for JobQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl JobQueue {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_JOBS)
    }

    pub fn with_capacity(max_jobs: usize) -> Self {
        Self {
            jobs: HashMap::new(),
            max_jobs,
        }
    }

    /// Add a new job to the queue. Returns false if the queue is at capacity.
    pub fn add_job(&mut self, job: Job) -> bool {
        if self.jobs.len() >= self.max_jobs {
            return false;
        }
        self.jobs.insert(job.id, job);
        true
    }

    /// Get a job by ID
    pub fn get_job(&self, id: &Uuid) -> Option<&Job> {
        self.jobs.get(id)
    }

    /// Get a mutable reference to a job by ID
    pub fn get_job_mut(&mut self, id: &Uuid) -> Option<&mut Job> {
        self.jobs.get_mut(id)
    }

    /// Update job status (legacy method for backwards compatibility)
    pub fn update_status(
        &mut self,
        id: &Uuid,
        status: JobStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> bool {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = status;
            if output.is_some() {
                job.output = output;
            }
            if error.is_some() {
                job.error = error;
            }
            true
        } else {
            false
        }
    }

    /// Update job with full execution result (used by executing node).
    /// This stores output locally - it won't be replicated through Raft.
    #[allow(clippy::too_many_arguments)]
    pub fn update_job_result(
        &mut self,
        id: &Uuid,
        status: JobStatus,
        executed_by: u64,
        exit_code: Option<i32>,
        output: Option<String>,
        error: Option<String>,
        completed_at: DateTime<Utc>,
    ) -> bool {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = status;
            job.executed_by = Some(executed_by);
            job.exit_code = exit_code;
            job.output = output;
            job.error = error;
            job.completed_at = Some(completed_at);
            true
        } else {
            false
        }
    }

    /// Update job metadata from Raft replication (no output).
    /// Output stays on the executing node - only metadata is replicated.
    pub fn update_status_metadata(
        &mut self,
        id: &Uuid,
        status: JobStatus,
        executed_by: u64,
        exit_code: Option<i32>,
        completed_at: Option<DateTime<Utc>>,
    ) -> bool {
        if let Some(job) = self.jobs.get_mut(id) {
            job.status = status;
            job.executed_by = Some(executed_by);
            job.exit_code = exit_code;
            job.completed_at = completed_at;
            // Note: output and error are NOT updated here
            // They remain None on non-executing nodes
            true
        } else {
            false
        }
    }

    /// Assign a job to a worker
    pub fn assign_job(&mut self, job_id: &Uuid, worker_id: u64) -> bool {
        if let Some(job) = self.jobs.get_mut(job_id) {
            job.assigned_worker = Some(worker_id);
            job.status = JobStatus::Running;
            true
        } else {
            false
        }
    }

    /// Get all pending jobs
    pub fn pending_jobs(&self) -> Vec<&Job> {
        self.jobs
            .values()
            .filter(|j| j.status == JobStatus::Pending)
            .collect()
    }

    /// Get all jobs sorted chronologically by creation time
    pub fn all_jobs(&self) -> Vec<&Job> {
        let mut jobs: Vec<&Job> = self.jobs.values().collect();
        jobs.sort_by_key(|j| j.created_at);
        jobs
    }

    /// Count jobs currently running on a specific worker
    pub fn running_jobs_on_worker(&self, worker_id: u64) -> usize {
        self.jobs
            .values()
            .filter(|j| j.status == JobStatus::Running && j.assigned_worker == Some(worker_id))
            .count()
    }

    /// Get jobs assigned to a specific worker
    pub fn jobs_for_worker(&self, worker_id: u64) -> Vec<&Job> {
        self.jobs
            .values()
            .filter(|j| j.assigned_worker == Some(worker_id))
            .collect()
    }

    /// Returns (job_id, command) pairs for all Running jobs assigned to `worker_id`.
    pub fn jobs_assigned_to(&self, worker_id: u64) -> Vec<(Uuid, String)> {
        self.jobs
            .values()
            .filter(|j| j.assigned_worker == Some(worker_id) && j.status == JobStatus::Running)
            .map(|j| (j.id, j.command.clone()))
            .collect()
    }

    /// Remove completed and failed jobs from the queue. Returns the number of jobs removed.
    pub fn cleanup_finished_jobs(&mut self) -> usize {
        let before = self.jobs.len();
        self.jobs
            .retain(|_, job| job.status != JobStatus::Completed && job.status != JobStatus::Failed);
        before - self.jobs.len()
    }

    /// Returns the current number of jobs in the queue
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Returns true if the queue is empty
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Returns true if the queue is at capacity
    pub fn is_full(&self) -> bool {
        self.jobs.len() >= self.max_jobs
    }

    /// Clear all jobs from the queue (used during snapshot rebuild)
    pub fn clear(&mut self) {
        self.jobs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::job::{Job, JobStatus};
    use chrono::Utc;
    use uuid::Uuid;

    #[test]
    fn test_update_status_preserves_none_output() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);
        // Set output via update_job_result
        queue.update_job_result(
            &id,
            JobStatus::Running,
            1,
            None,
            Some("hello".to_string()),
            None,
            Utc::now(),
        );
        // update_status with None should NOT overwrite existing output
        queue.update_status(&id, JobStatus::Completed, None, None);
        let job = queue.get_job(&id).unwrap();
        assert_eq!(job.output, Some("hello".to_string()));
    }

    #[test]
    fn test_update_status_overwrites_some_output() {
        let mut queue = JobQueue::new();
        let job = Job::new("test".to_string());
        let id = job.id;
        queue.add_job(job);
        queue.update_status(
            &id,
            JobStatus::Completed,
            Some("new".to_string()),
            Some("err".to_string()),
        );
        let job = queue.get_job(&id).unwrap();
        assert_eq!(job.output, Some("new".to_string()));
        assert_eq!(job.error, Some("err".to_string()));
    }

    #[test]
    fn test_update_status_nonexistent_returns_false() {
        let mut queue = JobQueue::new();
        let result = queue.update_status(&Uuid::new_v4(), JobStatus::Completed, None, None);
        assert!(!result);
    }

    #[test]
    fn test_update_job_result_sets_all_fields() {
        let mut queue = JobQueue::new();
        let job = Job::new("test".to_string());
        let id = job.id;
        queue.add_job(job);
        let now = Utc::now();
        queue.update_job_result(
            &id,
            JobStatus::Completed,
            2,
            Some(0),
            Some("out".to_string()),
            None,
            now,
        );
        let job = queue.get_job(&id).unwrap();
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.executed_by, Some(2));
        assert_eq!(job.exit_code, Some(0));
        assert_eq!(job.output, Some("out".to_string()));
        assert_eq!(job.error, None);
    }

    #[test]
    fn test_update_status_metadata_does_not_touch_output() {
        let mut queue = JobQueue::new();
        let job = Job::new("test".to_string());
        let id = job.id;
        queue.add_job(job);
        // Set output locally
        queue.update_job_result(
            &id,
            JobStatus::Running,
            1,
            None,
            Some("local output".to_string()),
            None,
            Utc::now(),
        );
        // Metadata update must not overwrite output
        queue.update_status_metadata(&id, JobStatus::Completed, 1, Some(0), Some(Utc::now()));
        let job = queue.get_job(&id).unwrap();
        assert_eq!(job.output, Some("local output".to_string()));
    }

    #[test]
    fn test_assign_job_sets_running_status() {
        let mut queue = JobQueue::new();
        let job = Job::new("test".to_string());
        let id = job.id;
        queue.add_job(job);
        let result = queue.assign_job(&id, 2);
        assert!(result);
        let job = queue.get_job(&id).unwrap();
        assert_eq!(job.status, JobStatus::Running);
        assert_eq!(job.assigned_worker, Some(2));
    }

    #[test]
    fn test_assign_job_nonexistent_returns_false() {
        let mut queue = JobQueue::new();
        assert!(!queue.assign_job(&Uuid::new_v4(), 1));
    }

    #[test]
    fn test_jobs_assigned_to_filters_running_only() {
        let mut queue = JobQueue::new();
        let job1 = Job::new("cmd1".to_string());
        let id1 = job1.id;
        let job2 = Job::new("cmd2".to_string());
        let id2 = job2.id;
        queue.add_job(job1);
        queue.add_job(job2);
        queue.assign_job(&id1, 1);
        queue.assign_job(&id2, 1);
        // Complete job2 — only job1 should remain as Running
        queue.update_status(&id2, JobStatus::Completed, None, None);
        let assigned = queue.jobs_assigned_to(1);
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].0, id1);
    }
}
