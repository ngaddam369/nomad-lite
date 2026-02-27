//! Worker execution engine for running jobs.
//!
//! This module handles job execution on worker nodes:
//! - **Job execution**: Runs commands in sandboxed Docker containers and captures output
//! - **Heartbeat**: Worker liveness signals are sent every 2 s from the worker loop in `node.rs`
//!
//! # Components
//!
//! - [`JobExecutor`]: Executes commands in isolated Docker containers and returns results
//!
//! # Execution Flow
//!
//! 1. Worker loop polls for assigned jobs via the local job queue
//! 2. [`JobExecutor::execute`] spawns `docker run alpine:latest sh -c <command>`
//! 3. Captures stdout/stderr and exit status within the configured timeout
//! 4. Returns [`ExecutionResult`](executor::ExecutionResult) with output and status

pub mod executor;

pub use executor::JobExecutor;
