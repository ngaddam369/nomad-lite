//! Job scheduling and assignment system.
//!
//! This module manages job lifecycle from submission to completion:
//! - **Job queue**: Stores all jobs with their current status
//! - **Job assignment**: Distributes pending jobs to available workers
//! - **Worker tracking**: Monitors worker health via heartbeats
//!
//! # Components
//!
//! - [`Job`]: Represents a shell command to execute with its status
//! - [`JobQueue`]: Thread-safe storage for all jobs in the cluster
//! - [`JobAssigner`](assigner::JobAssigner): Assigns jobs to least-loaded workers
//!
//! # Job Lifecycle
//!
//! 1. Job submitted via gRPC → replicated through Raft
//! 2. Committed job added to queue with `Pending` status
//! 3. Leader assigns job to available worker → status becomes `Running`
//! 4. Worker executes and reports result → status becomes `Completed`/`Failed`

pub mod assigner;
pub mod job;
pub mod queue;

pub use job::{Job, JobStatus};
pub use queue::JobQueue;
