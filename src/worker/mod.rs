//! Worker execution engine for running jobs.
//!
//! This module handles the actual execution of shell commands on worker nodes:
//! - **Job execution**: Spawns shell processes and captures output
//! - **Heartbeat**: Keeps worker registered as alive with the scheduler
//!
//! # Components
//!
//! - [`JobExecutor`]: Executes shell commands and returns results
//! - [`heartbeat`]: Worker heartbeat management (registration, keep-alive)
//!
//! # Execution Flow
//!
//! 1. Worker loop polls for assigned jobs
//! 2. [`JobExecutor::execute`] spawns `sh -c <command>`
//! 3. Captures stdout/stderr and exit status
//! 4. Returns [`JobResult`](executor::JobResult) with output and status
//!
//! # Security Note
//!
//! Commands are executed directly via shell without sandboxing.
//! See TODO list for planned security improvements.

pub mod executor;
pub mod heartbeat;

pub use executor::JobExecutor;
