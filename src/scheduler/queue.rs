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

    /// Get jobs assigned to a specific worker
    pub fn jobs_for_worker(&self, worker_id: u64) -> Vec<&Job> {
        self.jobs
            .values()
            .filter(|j| j.assigned_worker == Some(worker_id))
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
}
