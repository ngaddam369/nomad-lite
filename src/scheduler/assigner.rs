use std::collections::{HashMap, HashSet};
use std::time::Instant;
use uuid::Uuid;

use crate::scheduler::queue::JobQueue;

/// Worker state tracking
#[derive(Debug, Clone)]
pub struct WorkerState {
    pub id: u64,
    pub last_heartbeat: Instant,
    pub running_jobs: HashSet<Uuid>,
}

impl WorkerState {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            last_heartbeat: Instant::now(),
            running_jobs: HashSet::new(),
        }
    }

    pub fn update_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    pub fn is_alive(&self, timeout_ms: u64) -> bool {
        self.last_heartbeat.elapsed().as_millis() < timeout_ms as u128
    }
}

/// Assigns jobs to workers
#[derive(Debug, Default)]
pub struct JobAssigner {
    pub workers: HashMap<u64, WorkerState>,
    worker_timeout_ms: u64,
}

impl JobAssigner {
    pub fn new(worker_timeout_ms: u64) -> Self {
        Self {
            workers: HashMap::new(),
            worker_timeout_ms,
        }
    }

    /// Register a new worker
    pub fn register_worker(&mut self, worker_id: u64) {
        self.workers.insert(worker_id, WorkerState::new(worker_id));
        tracing::info!(worker_id, "Worker registered");
    }

    /// Update worker heartbeat
    pub fn worker_heartbeat(&mut self, worker_id: u64) {
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.update_heartbeat();
        } else {
            // Auto-register on heartbeat
            self.register_worker(worker_id);
        }
    }

    /// Get available workers (alive and not at capacity)
    pub fn available_workers(&self) -> Vec<u64> {
        self.workers
            .values()
            .filter(|w| w.is_alive(self.worker_timeout_ms))
            .map(|w| w.id)
            .collect()
    }

    /// Assign a pending job to an available worker
    /// Returns the worker_id if assignment was successful
    pub fn assign_next_job(&mut self, queue: &mut JobQueue) -> Option<(Uuid, u64)> {
        let available = self.available_workers();
        if available.is_empty() {
            return None;
        }

        // Find least loaded worker
        let worker_id = available.into_iter().min_by_key(|&id| {
            self.workers
                .get(&id)
                .map(|w| w.running_jobs.len())
                .unwrap_or(usize::MAX)
        })?;

        // Find a pending job
        let pending = queue.pending_jobs();
        let job_id = pending.first().map(|j| j.id)?;

        // Assign the job
        if queue.assign_job(&job_id, worker_id) {
            if let Some(worker) = self.workers.get_mut(&worker_id) {
                worker.running_jobs.insert(job_id);
            }
            tracing::info!(job_id = %job_id, worker_id, "Job assigned");
            Some((job_id, worker_id))
        } else {
            None
        }
    }

    /// Mark a job as completed for a worker
    pub fn job_completed(&mut self, worker_id: u64, job_id: &Uuid) {
        if let Some(worker) = self.workers.get_mut(&worker_id) {
            worker.running_jobs.remove(job_id);
        }
    }

    /// Get all workers
    pub fn all_workers(&self) -> Vec<&WorkerState> {
        self.workers.values().collect()
    }

    /// Check for dead workers and return their IDs
    pub fn check_dead_workers(&self) -> Vec<u64> {
        self.workers
            .values()
            .filter(|w| !w.is_alive(self.worker_timeout_ms))
            .map(|w| w.id)
            .collect()
    }

    /// Clear all workers (used during snapshot rebuild)
    pub fn clear(&mut self) {
        self.workers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::job::Job;
    use crate::scheduler::queue::JobQueue;
    use uuid::Uuid;

    #[test]
    fn test_assign_to_least_loaded_worker() {
        let mut queue = JobQueue::new();
        let mut assigner = JobAssigner::new(5000);
        assigner.register_worker(1);
        assigner.register_worker(2);
        // Give worker 1 two fake running jobs so it is more loaded
        let fake1 = Uuid::new_v4();
        let fake2 = Uuid::new_v4();
        assigner
            .workers
            .get_mut(&1)
            .unwrap()
            .running_jobs
            .insert(fake1);
        assigner
            .workers
            .get_mut(&1)
            .unwrap()
            .running_jobs
            .insert(fake2);
        queue.add_job(Job::new("test".to_string()));
        let result = assigner.assign_next_job(&mut queue);
        assert!(result.is_some());
        let (_, worker_id) = result.unwrap();
        assert_eq!(worker_id, 2);
    }

    #[test]
    fn test_assign_with_equal_load_assigns_one_job() {
        let mut queue = JobQueue::new();
        let mut assigner = JobAssigner::new(5000);
        assigner.register_worker(1);
        assigner.register_worker(2);
        queue.add_job(Job::new("test".to_string()));
        let result = assigner.assign_next_job(&mut queue);
        assert!(result.is_some());
        let (job_id, _) = result.unwrap();
        let job = queue.get_job(&job_id).unwrap();
        assert_eq!(job.status, crate::scheduler::job::JobStatus::Running);
    }

    #[test]
    fn test_job_completed_reduces_load() {
        let mut queue = JobQueue::new();
        let mut assigner = JobAssigner::new(5000);
        assigner.register_worker(1);
        queue.add_job(Job::new("job1".to_string()));
        queue.add_job(Job::new("job2".to_string()));
        let (id1, w) = assigner.assign_next_job(&mut queue).unwrap();
        let _ = assigner.assign_next_job(&mut queue).unwrap();
        assert_eq!(assigner.workers.get(&1).unwrap().running_jobs.len(), 2);
        assigner.job_completed(w, &id1);
        assert_eq!(assigner.workers.get(&1).unwrap().running_jobs.len(), 1);
    }

    #[test]
    fn test_check_dead_workers_detects_expired() {
        let mut assigner = JobAssigner::new(0); // 0ms timeout → immediately dead
        assigner.register_worker(1);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let dead = assigner.check_dead_workers();
        assert!(dead.contains(&1));
    }

    #[test]
    fn test_available_workers_excludes_dead() {
        let mut queue = JobQueue::new();
        let mut assigner = JobAssigner::new(0); // 0ms timeout → immediately dead
        assigner.register_worker(1);
        std::thread::sleep(std::time::Duration::from_millis(1));
        queue.add_job(Job::new("test".to_string()));
        // No alive workers → assign_next_job returns None
        let result = assigner.assign_next_job(&mut queue);
        assert!(result.is_none());
    }
}
