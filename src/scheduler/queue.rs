use std::collections::HashMap;
use uuid::Uuid;

use crate::scheduler::job::{Job, JobStatus};

/// Manages the job queue and job state
#[derive(Debug, Default)]
pub struct JobQueue {
    jobs: HashMap<Uuid, Job>,
}

impl JobQueue {
    pub fn new() -> Self {
        Self {
            jobs: HashMap::new(),
        }
    }

    /// Add a new job to the queue
    pub fn add_job(&mut self, job: Job) {
        self.jobs.insert(job.id, job);
    }

    /// Get a job by ID
    pub fn get_job(&self, id: &Uuid) -> Option<&Job> {
        self.jobs.get(id)
    }

    /// Get a mutable reference to a job by ID
    pub fn get_job_mut(&mut self, id: &Uuid) -> Option<&mut Job> {
        self.jobs.get_mut(id)
    }

    /// Update job status
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

    /// Get all jobs
    pub fn all_jobs(&self) -> Vec<&Job> {
        self.jobs.values().collect()
    }

    /// Get jobs assigned to a specific worker
    pub fn jobs_for_worker(&self, worker_id: u64) -> Vec<&Job> {
        self.jobs
            .values()
            .filter(|j| j.assigned_worker == Some(worker_id))
            .collect()
    }
}
