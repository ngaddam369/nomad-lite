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

    /// Add a new job to the queue. Returns false if the active-job limit is reached.
    /// Completed and failed jobs do not count toward the limit.
    pub fn add_job(&mut self, job: Job) -> bool {
        if self.active_job_count() >= self.max_jobs {
            return false;
        }
        self.jobs.insert(job.id, job);
        true
    }

    /// Number of jobs that are Pending or Running (i.e. consuming a capacity slot).
    fn active_job_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|j| matches!(j.status, JobStatus::Pending | JobStatus::Running))
            .count()
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
            // Ensure terminal jobs always carry a completion timestamp so that
            // TTL-based eviction in cleanup_finished_jobs has a value to compare.
            if matches!(status, JobStatus::Completed | JobStatus::Failed)
                && job.completed_at.is_none()
            {
                job.completed_at = Some(Utc::now());
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

    /// Cancel a pending or running job. Returns true if the job was found and
    /// was in a cancellable state (Pending or Running).
    pub fn cancel_job(&mut self, id: &Uuid) -> bool {
        if let Some(job) = self.jobs.get_mut(id) {
            if matches!(job.status, JobStatus::Pending | JobStatus::Running) {
                job.status = JobStatus::Cancelled;
                job.completed_at = Some(Utc::now());
                return true;
            }
        }
        false
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

    /// Return jobs that match all provided filters, sorted chronologically.
    ///
    /// `None` for any parameter means "no restriction on that dimension".
    /// `worker_id_filter` matches on either `assigned_worker` or `executed_by`.
    /// `command_filter` is a case-insensitive substring match.
    pub fn filtered_jobs(
        &self,
        status_filter: Option<JobStatus>,
        worker_id_filter: Option<u64>,
        command_filter: Option<&str>,
        created_after_ms: Option<i64>,
        created_before_ms: Option<i64>,
    ) -> Vec<&Job> {
        let command_lower = command_filter.map(|s| s.to_lowercase());
        let created_after = created_after_ms
            .and_then(|ms| chrono::TimeZone::timestamp_millis_opt(&Utc, ms).single());
        let created_before = created_before_ms
            .and_then(|ms| chrono::TimeZone::timestamp_millis_opt(&Utc, ms).single());

        let mut jobs: Vec<&Job> = self
            .jobs
            .values()
            .filter(|j| {
                if let Some(status) = status_filter {
                    if j.status != status {
                        return false;
                    }
                }
                if let Some(worker_id) = worker_id_filter {
                    if j.assigned_worker != Some(worker_id) && j.executed_by != Some(worker_id) {
                        return false;
                    }
                }
                if let Some(ref lower) = command_lower {
                    if !j.command.to_lowercase().contains(lower.as_str()) {
                        return false;
                    }
                }
                if let Some(after) = created_after {
                    if j.created_at < after {
                        return false;
                    }
                }
                if let Some(before) = created_before {
                    if j.created_at > before {
                        return false;
                    }
                }
                true
            })
            .collect();

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

    /// Remove completed and failed jobs whose `completed_at` is older than
    /// `retention_secs`. Jobs with no `completed_at` timestamp (shouldn't
    /// happen in practice) are retained. Returns the number of jobs removed.
    pub fn cleanup_finished_jobs(&mut self, retention_secs: u64) -> usize {
        let before = self.jobs.len();
        let cutoff = Utc::now() - chrono::Duration::seconds(retention_secs as i64);
        self.jobs.retain(|_, job| match job.status {
            JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled => {
                // Keep if completed recently; retain if timestamp is missing.
                job.completed_at.is_none_or(|t| t > cutoff)
            }
            _ => true,
        });
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

    /// Returns true if the active-job limit has been reached.
    /// Completed and failed jobs do not count toward the limit.
    pub fn is_full(&self) -> bool {
        self.active_job_count() >= self.max_jobs
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

    #[test]
    fn test_cancel_pending_job() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);
        assert_eq!(queue.get_job(&id).unwrap().status, JobStatus::Pending);

        let result = queue.cancel_job(&id);
        assert!(result);

        let job = queue.get_job(&id).unwrap();
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(
            job.completed_at.is_some(),
            "completed_at must be set on cancel"
        );
    }

    #[test]
    fn test_cancel_running_job() {
        let mut queue = JobQueue::new();
        let job = Job::new("sleep 30".to_string());
        let id = job.id;
        queue.add_job(job);
        queue.assign_job(&id, 1);
        assert_eq!(queue.get_job(&id).unwrap().status, JobStatus::Running);

        let result = queue.cancel_job(&id);
        assert!(result);
        assert_eq!(queue.get_job(&id).unwrap().status, JobStatus::Cancelled);
    }

    #[test]
    fn test_cancel_completed_returns_false() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);
        queue.update_status(&id, JobStatus::Completed, None, None);

        assert!(!queue.cancel_job(&id));
        assert_eq!(queue.get_job(&id).unwrap().status, JobStatus::Completed);
    }

    #[test]
    fn test_cancel_failed_returns_false() {
        let mut queue = JobQueue::new();
        let job = Job::new("false".to_string());
        let id = job.id;
        queue.add_job(job);
        queue.update_status(&id, JobStatus::Failed, None, None);

        assert!(!queue.cancel_job(&id));
        assert_eq!(queue.get_job(&id).unwrap().status, JobStatus::Failed);
    }

    #[test]
    fn test_cancel_already_cancelled_is_idempotent() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);

        assert!(queue.cancel_job(&id)); // first cancel succeeds
        assert!(!queue.cancel_job(&id)); // second returns false — already terminal
        assert_eq!(queue.get_job(&id).unwrap().status, JobStatus::Cancelled);
    }

    #[test]
    fn test_cancel_nonexistent_returns_false() {
        let mut queue = JobQueue::new();
        assert!(!queue.cancel_job(&Uuid::new_v4()));
    }

    #[test]
    fn test_cancelled_does_not_count_toward_capacity() {
        let mut queue = JobQueue::with_capacity(1);
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);
        assert!(queue.is_full(), "queue should be full with one pending job");

        // Cancelling frees the capacity slot
        assert!(queue.cancel_job(&id));
        assert!(
            !queue.is_full(),
            "cancelled job must not count toward capacity"
        );

        // A new job can now be admitted
        assert!(queue.add_job(Job::new("echo world".to_string())));
    }

    #[test]
    fn test_cleanup_evicts_old_cancelled_job() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);
        assert!(queue.cancel_job(&id));

        // Backdate completed_at to 2 minutes ago so it falls outside a 60s window
        if let Some(j) = queue.get_job_mut(&id) {
            j.completed_at = Some(Utc::now() - chrono::Duration::seconds(120));
        }

        let removed = queue.cleanup_finished_jobs(60);
        assert_eq!(removed, 1);
        assert!(queue.get_job(&id).is_none());
    }

    #[test]
    fn test_cleanup_retains_recently_cancelled_job() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo hello".to_string());
        let id = job.id;
        queue.add_job(job);
        assert!(queue.cancel_job(&id));
        // completed_at is set to now by cancel_job; well within any retention window

        let removed = queue.cleanup_finished_jobs(3600);
        assert_eq!(removed, 0);
        assert!(queue.get_job(&id).is_some());
    }

    // =========================================================================
    // filtered_jobs tests
    // =========================================================================

    #[test]
    fn test_filtered_jobs_no_filters_matches_all_jobs() {
        let mut queue = JobQueue::new();
        queue.add_job(Job::new("echo a".to_string()));
        queue.add_job(Job::new("echo b".to_string()));
        queue.add_job(Job::new("sleep 1".to_string()));

        let all = queue.all_jobs();
        let filtered = queue.filtered_jobs(None, None, None, None, None);
        assert_eq!(filtered.len(), all.len());
    }

    #[test]
    fn test_filtered_jobs_by_status_pending() {
        let mut queue = JobQueue::new();
        let job_a = Job::new("echo a".to_string());
        let id_a = job_a.id;
        let job_b = Job::new("echo b".to_string());
        queue.add_job(job_a);
        queue.add_job(job_b);

        // Complete job_a
        queue.update_status(&id_a, JobStatus::Completed, None, None);

        let pending = queue.filtered_jobs(Some(JobStatus::Pending), None, None, None, None);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, JobStatus::Pending);
    }

    #[test]
    fn test_filtered_jobs_by_status_completed() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo x".to_string());
        let id = job.id;
        queue.add_job(job);
        queue.update_status(&id, JobStatus::Completed, None, None);
        queue.add_job(Job::new("echo y".to_string())); // stays pending

        let completed = queue.filtered_jobs(Some(JobStatus::Completed), None, None, None, None);
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, id);
    }

    #[test]
    fn test_filtered_jobs_by_worker_assigned() {
        let mut queue = JobQueue::new();
        let job1 = Job::new("cmd1".to_string());
        let id1 = job1.id;
        let job2 = Job::new("cmd2".to_string());
        let id2 = job2.id;
        queue.add_job(job1);
        queue.add_job(job2);
        queue.assign_job(&id1, 1);
        queue.assign_job(&id2, 2);

        let worker1_jobs = queue.filtered_jobs(None, Some(1), None, None, None);
        assert_eq!(worker1_jobs.len(), 1);
        assert_eq!(worker1_jobs[0].id, id1);
    }

    #[test]
    fn test_filtered_jobs_by_worker_executed_by() {
        let mut queue = JobQueue::new();
        let job = Job::new("echo z".to_string());
        let id = job.id;
        queue.add_job(job);
        // Mark as executed by worker 3 (no assigned_worker set)
        queue.update_job_result(
            &id,
            JobStatus::Completed,
            3,
            Some(0),
            None,
            None,
            Utc::now(),
        );

        let results = queue.filtered_jobs(None, Some(3), None, None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);

        // Worker 99 matches nothing
        let empty = queue.filtered_jobs(None, Some(99), None, None, None);
        assert!(empty.is_empty());
    }

    #[test]
    fn test_filtered_jobs_command_substring_case_insensitive() {
        let mut queue = JobQueue::new();
        queue.add_job(Job::new("echo hello".to_string()));
        queue.add_job(Job::new("sleep 30".to_string()));

        // Uppercase filter should still match
        let results = queue.filtered_jobs(None, None, Some("ECHO"), None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].command, "echo hello");
    }

    #[test]
    fn test_filtered_jobs_command_substring_no_match() {
        let mut queue = JobQueue::new();
        queue.add_job(Job::new("echo hello".to_string()));

        let results = queue.filtered_jobs(None, None, Some("python"), None, None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_filtered_jobs_created_after_ms() {
        let mut queue = JobQueue::new();
        let old_time = Utc::now() - chrono::Duration::hours(2);
        let recent_time = Utc::now();

        let old_job = Job::with_id(Uuid::new_v4(), "old cmd".to_string(), old_time);
        let new_job = Job::with_id(Uuid::new_v4(), "new cmd".to_string(), recent_time);
        let old_id = old_job.id;
        queue.add_job(old_job);
        queue.add_job(new_job);

        // Cutoff 1 hour ago — old job should be excluded
        let cutoff_ms = (Utc::now() - chrono::Duration::hours(1)).timestamp_millis();
        let results = queue.filtered_jobs(None, None, None, Some(cutoff_ms), None);
        assert_eq!(results.len(), 1);
        assert_ne!(results[0].id, old_id);
    }

    #[test]
    fn test_filtered_jobs_created_before_ms() {
        let mut queue = JobQueue::new();
        let old_time = Utc::now() - chrono::Duration::hours(2);
        let recent_time = Utc::now();

        let old_job = Job::with_id(Uuid::new_v4(), "old cmd".to_string(), old_time);
        let new_job = Job::with_id(Uuid::new_v4(), "new cmd".to_string(), recent_time);
        let new_id = new_job.id;
        queue.add_job(old_job);
        queue.add_job(new_job);

        // Cutoff 1 hour ago — only old job was created before that
        let cutoff_ms = (Utc::now() - chrono::Duration::hours(1)).timestamp_millis();
        let results = queue.filtered_jobs(None, None, None, None, Some(cutoff_ms));
        assert_eq!(results.len(), 1);
        assert_ne!(results[0].id, new_id);
    }

    #[test]
    fn test_filtered_jobs_combined_status_and_command() {
        let mut queue = JobQueue::new();
        let job1 = Job::new("echo pending".to_string());
        let id1 = job1.id;
        let job2 = Job::new("echo completed".to_string());
        let id2 = job2.id;
        let job3 = Job::new("sleep 10".to_string());
        queue.add_job(job1);
        queue.add_job(job2);
        queue.add_job(job3);
        queue.update_status(&id2, JobStatus::Completed, None, None);

        // Pending + "echo" → only job1
        let results = queue.filtered_jobs(Some(JobStatus::Pending), None, Some("echo"), None, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id1);
    }

    #[test]
    fn test_filtered_jobs_no_matches_returns_empty() {
        let mut queue = JobQueue::new();
        queue.add_job(Job::new("echo hello".to_string()));

        // Filter by completed — nothing is completed
        let results = queue.filtered_jobs(Some(JobStatus::Completed), None, None, None, None);
        assert!(results.is_empty());
    }

    #[test]
    fn test_filtered_jobs_sorted_chronologically() {
        let mut queue = JobQueue::new();
        let t1 = Utc::now() - chrono::Duration::seconds(10);
        let t2 = Utc::now() - chrono::Duration::seconds(5);
        let t3 = Utc::now();

        let j1 = Job::with_id(Uuid::new_v4(), "echo 1".to_string(), t1);
        let j2 = Job::with_id(Uuid::new_v4(), "echo 2".to_string(), t2);
        let j3 = Job::with_id(Uuid::new_v4(), "echo 3".to_string(), t3);
        let (id1, id2, id3) = (j1.id, j2.id, j3.id);
        // Insert in reverse order to test sorting
        queue.add_job(j3);
        queue.add_job(j1);
        queue.add_job(j2);

        let results = queue.filtered_jobs(None, None, Some("echo"), None, None);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, id1);
        assert_eq!(results[1].id, id2);
        assert_eq!(results[2].id, id3);
    }
}
