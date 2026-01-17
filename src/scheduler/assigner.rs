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
    workers: HashMap<u64, WorkerState>,
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
}
