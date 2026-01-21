use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::{NodeConfig, SandboxConfig};
use crate::dashboard::{run_dashboard, DashboardState};
use crate::grpc::GrpcServer;
use crate::raft::{Command, RaftNode};
use crate::scheduler::assigner::JobAssigner;
use crate::scheduler::{Job, JobQueue, JobStatus};
use crate::worker::JobExecutor;

/// Main node that orchestrates all components
pub struct Node {
    pub config: NodeConfig,
    pub raft_node: Arc<RaftNode>,
    pub job_queue: Arc<RwLock<JobQueue>>,
    pub job_assigner: Arc<RwLock<JobAssigner>>,
    pub executor: JobExecutor,
    pub dashboard_addr: Option<SocketAddr>,
}

impl Node {
    pub fn new(
        config: NodeConfig,
        dashboard_addr: Option<SocketAddr>,
    ) -> (
        Self,
        tokio::sync::mpsc::Receiver<crate::raft::node::RaftMessage>,
    ) {
        let (raft_node, raft_rx) = RaftNode::new(config.clone());

        let node = Self {
            executor: JobExecutor::with_sandbox(config.sandbox.clone()),
            config,
            raft_node: Arc::new(raft_node),
            job_queue: Arc::new(RwLock::new(JobQueue::new())),
            job_assigner: Arc::new(RwLock::new(JobAssigner::new(5000))), // 5s worker timeout
            dashboard_addr,
        };

        (node, raft_rx)
    }

    /// Run the node with all components.
    ///
    /// This is the main entry point that starts all node subsystems:
    /// 1. Connects to peer nodes for Raft communication
    /// 2. Spawns the Raft consensus loop (leader election, log replication)
    /// 3. Spawns the scheduler loop (applies committed entries, assigns jobs)
    /// 4. Spawns the worker loop (executes assigned jobs)
    /// 5. Optionally spawns the web dashboard server
    /// 6. Runs the gRPC server (blocking)
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC server fails to start or encounters a fatal error.
    /// Other components run as spawned tasks and log their own errors.
    pub async fn run(
        self,
        raft_rx: tokio::sync::mpsc::Receiver<crate::raft::node::RaftMessage>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Connect to peers
        self.raft_node.connect_to_peers().await;

        // Spawn Raft node
        let raft_node = self.raft_node.clone();
        tokio::spawn(async move {
            raft_node.run(raft_rx).await;
        });

        // Spawn scheduler loop (processes committed entries and assigns jobs)
        let scheduler_raft = self.raft_node.clone();
        let scheduler_queue = self.job_queue.clone();
        let scheduler_assigner = self.job_assigner.clone();
        tokio::spawn(async move {
            Self::scheduler_loop(scheduler_raft, scheduler_queue, scheduler_assigner).await;
        });

        // Spawn worker loop (executes assigned jobs)
        let worker_raft = self.raft_node.clone();
        let worker_queue = self.job_queue.clone();
        let worker_assigner = self.job_assigner.clone();
        let node_id = self.config.node_id;
        let sandbox_config = self.config.sandbox.clone();
        tokio::spawn(async move {
            Self::worker_loop(
                node_id,
                worker_raft,
                worker_queue,
                worker_assigner,
                sandbox_config,
            )
            .await;
        });

        // Spawn dashboard server if configured
        if let Some(dashboard_addr) = self.dashboard_addr {
            let dashboard_state = DashboardState {
                raft_node: self.raft_node.clone(),
                job_queue: self.job_queue.clone(),
            };
            tokio::spawn(async move {
                run_dashboard(dashboard_addr, dashboard_state).await;
            });
        }

        // Run gRPC server (blocks)
        let server = GrpcServer::new(
            self.config.listen_addr,
            self.config.clone(),
            self.raft_node.clone(),
            self.job_queue.clone(),
        );
        server.run().await?;
        Ok(())
    }

    /// Scheduler loop that maintains consistent state and assigns jobs.
    ///
    /// This loop runs on all nodes and performs two key functions:
    ///
    /// ## 1. Apply Committed Entries (all nodes)
    /// Waits for commit notifications from Raft and applies them to local state:
    /// - `SubmitJob`: Adds job to queue if not already present (idempotent)
    /// - `UpdateJobStatus`: Updates job status, output, and error
    /// - `RegisterWorker`: Registers worker with the job assigner
    ///
    /// This ensures all nodes maintain identical job queue state.
    ///
    /// ## 2. Assign Jobs (leader only)
    /// The leader assigns pending jobs to available workers using
    /// least-loaded scheduling. Followers skip this step.
    async fn scheduler_loop(
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
        job_assigner: Arc<RwLock<JobAssigner>>,
    ) {
        let mut commit_rx = raft_node.subscribe_commits();
        // Interval for leader job assignment (doesn't need to be as frequent)
        let mut assign_interval = tokio::time::interval(tokio::time::Duration::from_millis(100));

        loop {
            tokio::select! {
                // Wait for commit notifications
                result = commit_rx.changed() => {
                    if result.is_err() {
                        // Channel closed, exit loop
                        break;
                    }

                    // Apply committed entries
                    let entries = raft_node.get_committed_entries().await;
                    for entry in entries {
                        match entry.command {
                            Command::SubmitJob { job_id, command } => {
                                let mut queue = job_queue.write().await;
                                if queue.get_job(&job_id).is_none() {
                                    if queue.add_job(Job::with_id(job_id, command)) {
                                        tracing::debug!(job_id = %job_id, "Job added from committed entry");
                                    } else {
                                        tracing::warn!(job_id = %job_id, "Job queue at capacity, job dropped");
                                    }
                                }
                            }
                            Command::UpdateJobStatus {
                                job_id,
                                status,
                                executed_by,
                                exit_code,
                            } => {
                                let mut queue = job_queue.write().await;
                                queue.update_status_metadata(&job_id, status, executed_by, exit_code);
                                tracing::debug!(job_id = %job_id, status = %status, executed_by, "Job status updated");
                            }
                            Command::RegisterWorker { worker_id } => {
                                job_assigner.write().await.register_worker(worker_id);
                            }
                            Command::Noop => {}
                        }
                    }
                }

                // Leader job assignment on interval
                _ = assign_interval.tick() => {
                    if !raft_node.is_leader().await {
                        continue;
                    }

                    let mut queue = job_queue.write().await;
                    let mut assigner = job_assigner.write().await;
                    while let Some((_job_id, _worker_id)) = assigner.assign_next_job(&mut queue) {
                        // Job assigned
                    }
                }
            }
        }
    }

    /// Worker loop that executes jobs assigned to this node.
    ///
    /// Each node acts as both a potential leader and a worker. This loop:
    ///
    /// 1. **Registers** this node as a worker on startup
    /// 2. **Sends heartbeats** every 500ms to stay registered
    /// 3. **Polls for assigned jobs** that are in `Running` status
    /// 4. **Executes jobs** via shell and captures output
    /// 5. **Updates local state** with job results
    /// 6. **Replicates status** through Raft (leader only)
    ///
    /// # Polling Interval
    /// Runs every 500ms. See TODO for event-driven improvements.
    async fn worker_loop(
        node_id: u64,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
        job_assigner: Arc<RwLock<JobAssigner>>,
        sandbox_config: SandboxConfig,
    ) {
        let executor = JobExecutor::with_sandbox(sandbox_config);
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));

        // Register self as worker
        {
            let mut assigner = job_assigner.write().await;
            assigner.register_worker(node_id);
        }

        loop {
            interval.tick().await;

            // Send heartbeat
            {
                let mut assigner = job_assigner.write().await;
                assigner.worker_heartbeat(node_id);
            }

            // Check for jobs assigned to this worker
            let jobs_to_run: Vec<(uuid::Uuid, String)> = {
                let queue = job_queue.read().await;
                let assigner = job_assigner.read().await;
                assigner
                    .all_workers()
                    .iter()
                    .find(|w| w.id == node_id)
                    .map(|w| {
                        w.running_jobs
                            .iter()
                            .filter_map(|job_id| {
                                queue.get_job(job_id).and_then(|job| {
                                    if job.status == JobStatus::Running {
                                        Some((*job_id, job.command.clone()))
                                    } else {
                                        None
                                    }
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };

            // Execute jobs
            for (job_id, command) in jobs_to_run {
                let result = executor.execute(job_id, &command).await;

                // Update local job with full result (output stored locally only)
                {
                    let mut queue = job_queue.write().await;
                    queue.update_job_result(
                        &job_id,
                        result.status,
                        node_id,
                        result.exit_code,
                        result.output,
                        result.error,
                    );
                }

                // Mark job completed in assigner
                {
                    let mut assigner = job_assigner.write().await;
                    assigner.job_completed(node_id, &job_id);
                }

                // If leader, replicate the metadata (not output) through Raft
                if raft_node.is_leader().await {
                    let command = Command::UpdateJobStatus {
                        job_id,
                        status: result.status,
                        executed_by: node_id,
                        exit_code: result.exit_code,
                    };
                    let (tx, _rx) = tokio::sync::oneshot::channel();
                    if let Err(e) = raft_node
                        .message_sender()
                        .send(crate::raft::node::RaftMessage::AppendCommand {
                            command,
                            response_tx: tx,
                        })
                        .await
                    {
                        tracing::warn!(
                            job_id = %job_id,
                            error = %e,
                            "Failed to send job status update to Raft"
                        );
                    }
                }
            }
        }
    }
}
