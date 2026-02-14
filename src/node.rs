use chrono::Utc;
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{Notify, RwLock};
use tokio_util::sync::CancellationToken;

use crate::config::{NodeConfig, SandboxConfig};
use crate::dashboard::{run_dashboard, DashboardState};
use crate::grpc::GrpcServer;
use crate::raft::{Command, RaftNode};
use crate::scheduler::assigner::JobAssigner;
use crate::scheduler::{Job, JobQueue, JobStatus};
use crate::shutdown::install_shutdown_handler;
use crate::tls::TlsIdentity;
use crate::worker::JobExecutor;

/// Main node that orchestrates all components
pub struct Node {
    pub config: NodeConfig,
    pub raft_node: Arc<RaftNode>,
    pub job_queue: Arc<RwLock<JobQueue>>,
    pub job_assigner: Arc<RwLock<JobAssigner>>,
    pub executor: JobExecutor,
    pub dashboard_addr: Option<SocketAddr>,
    pub tls_identity: Option<TlsIdentity>,
    pub job_notify: Arc<Notify>,
    pub worker_notify: Arc<Notify>,
    pub draining: Arc<AtomicBool>,
}

impl Node {
    pub fn new(
        config: NodeConfig,
        dashboard_addr: Option<SocketAddr>,
        tls_identity: Option<TlsIdentity>,
    ) -> (
        Self,
        tokio::sync::mpsc::Receiver<crate::raft::node::RaftMessage>,
    ) {
        let (raft_node, raft_rx) = RaftNode::new(config.clone(), tls_identity.clone());

        let node = Self {
            executor: JobExecutor::new(config.sandbox.clone()),
            config,
            raft_node: Arc::new(raft_node),
            job_queue: Arc::new(RwLock::new(JobQueue::new())),
            job_assigner: Arc::new(RwLock::new(JobAssigner::new(5000))), // 5s worker timeout
            dashboard_addr,
            tls_identity,
            job_notify: Arc::new(Notify::new()),
            worker_notify: Arc::new(Notify::new()),
            draining: Arc::new(AtomicBool::new(false)),
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
        let shutdown_token = install_shutdown_handler();

        // Connect to peers (best-effort initial attempt)
        self.raft_node.connect_to_peers().await;

        // Spawn background task to retry connecting to any peers that weren't available at startup
        let retry_raft = self.raft_node.clone();
        let retry_token = shutdown_token.clone();
        tokio::spawn(async move {
            loop {
                if retry_raft.all_peers_connected().await {
                    tracing::info!("All peers connected");
                    break;
                }
                tokio::select! {
                    _ = retry_token.cancelled() => break,
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(500)) => {}
                }
                retry_raft.connect_to_peers().await;
            }
        });

        // Spawn Raft node
        let raft_node = self.raft_node.clone();
        let raft_token = shutdown_token.clone();
        let raft_handle = tokio::spawn(async move {
            raft_node.run(raft_rx, raft_token).await;
        });

        // Spawn scheduler loop (processes committed entries and assigns jobs)
        let scheduler_raft = self.raft_node.clone();
        let scheduler_queue = self.job_queue.clone();
        let scheduler_assigner = self.job_assigner.clone();
        let scheduler_job_notify = self.job_notify.clone();
        let scheduler_worker_notify = self.worker_notify.clone();
        let scheduler_token = shutdown_token.clone();
        let scheduler_handle = tokio::spawn(async move {
            Self::scheduler_loop(
                scheduler_raft,
                scheduler_queue,
                scheduler_assigner,
                scheduler_job_notify,
                scheduler_worker_notify,
                scheduler_token,
            )
            .await;
        });

        // Spawn worker loop (executes assigned jobs)
        let worker_raft = self.raft_node.clone();
        let worker_queue = self.job_queue.clone();
        let worker_assigner = self.job_assigner.clone();
        let node_id = self.config.node_id;
        let sandbox_config = self.config.sandbox.clone();
        let loop_worker_notify = self.worker_notify.clone();
        let worker_token = shutdown_token.clone();
        let worker_handle = tokio::spawn(async move {
            Self::worker_loop(
                node_id,
                worker_raft,
                worker_queue,
                worker_assigner,
                sandbox_config,
                loop_worker_notify,
                worker_token,
            )
            .await;
        });

        // Spawn dashboard server if configured
        if let Some(dashboard_addr) = self.dashboard_addr {
            let dashboard_state = DashboardState {
                raft_node: self.raft_node.clone(),
                job_queue: self.job_queue.clone(),
                draining: self.draining.clone(),
            };
            let dashboard_token = shutdown_token.clone();
            tokio::spawn(async move {
                run_dashboard(dashboard_addr, dashboard_state, dashboard_token).await;
            });
        }

        // Run gRPC server (blocks until shutdown signal)
        let server = GrpcServer::new(
            self.config.listen_addr,
            self.config.clone(),
            self.raft_node.clone(),
            self.job_queue.clone(),
            self.tls_identity.clone(),
            self.draining.clone(),
        );
        server.run(shutdown_token).await?;
        tracing::info!("gRPC server stopped, waiting for subsystems to drain");

        // Wait for worker to finish in-flight jobs (up to 30s)
        if tokio::time::timeout(std::time::Duration::from_secs(30), worker_handle)
            .await
            .is_err()
        {
            tracing::warn!("Worker loop did not finish within 30s timeout");
        }

        // Wait for raft and scheduler to stop (up to 5s)
        let drain = futures::future::join(raft_handle, scheduler_handle);
        if tokio::time::timeout(std::time::Duration::from_secs(5), drain)
            .await
            .is_err()
        {
            tracing::warn!("Raft/scheduler did not finish within 5s timeout");
        }

        tracing::info!("Graceful shutdown complete");
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
        job_notify: Arc<Notify>,
        worker_notify: Arc<Notify>,
        shutdown_token: CancellationToken,
    ) {
        let mut commit_rx = raft_node.subscribe_commits();

        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    tracing::info!("Scheduler loop shutting down");
                    break;
                }

                // Wait for commit notifications
                result = commit_rx.changed() => {
                    if result.is_err() {
                        // Channel closed, exit loop
                        break;
                    }

                    // Apply committed entries
                    let entries = raft_node.get_committed_entries().await;
                    let mut should_wake_assigner = false;
                    for entry in &entries {
                        match &entry.command {
                            Command::SubmitJob { job_id, command, created_at } => {
                                let mut queue = job_queue.write().await;
                                if queue.get_job(job_id).is_none() {
                                    if queue.add_job(Job::with_id(*job_id, command.clone(), *created_at)) {
                                        tracing::debug!(job_id = %job_id, created_at = %created_at, "Job added from committed entry");
                                    } else {
                                        tracing::warn!(job_id = %job_id, "Job queue at capacity, job dropped");
                                    }
                                }
                                // Always wake assigner — the gRPC handler may have
                                // already added the job to the queue before this
                                // commit notification arrived.
                                should_wake_assigner = true;
                            }
                            Command::UpdateJobStatus {
                                job_id,
                                status,
                                executed_by,
                                exit_code,
                                completed_at,
                            } => {
                                let mut queue = job_queue.write().await;
                                queue.update_status_metadata(job_id, *status, *executed_by, *exit_code, *completed_at);
                                tracing::debug!(
                                    job_id = %job_id,
                                    status = %status,
                                    executed_by,
                                    completed_at = completed_at.map(|dt| dt.to_rfc3339()).as_deref().unwrap_or("n/a"),
                                    "Job status updated"
                                );
                            }
                            Command::RegisterWorker { worker_id } => {
                                job_assigner.write().await.register_worker(*worker_id);
                                should_wake_assigner = true;
                            }
                            Command::Noop => {}
                        }
                    }
                    if should_wake_assigner {
                        job_notify.notify_one();
                    }
                }

                // Leader job assignment — wakes on new jobs or new workers
                _ = job_notify.notified() => {
                    if !raft_node.is_leader().await {
                        continue;
                    }

                    let mut queue = job_queue.write().await;
                    let mut assigner = job_assigner.write().await;
                    while let Some((_job_id, _worker_id)) = assigner.assign_next_job(&mut queue) {
                        worker_notify.notify_one();
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
    /// 2. **Sends heartbeats** every 2s to stay registered
    /// 3. **Wakes immediately** when jobs are assigned via `worker_notify`
    /// 4. **Executes jobs** via shell and captures output
    /// 5. **Updates local state** with job results
    /// 6. **Replicates status** through Raft (leader only)
    async fn worker_loop(
        node_id: u64,
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
        job_assigner: Arc<RwLock<JobAssigner>>,
        sandbox_config: SandboxConfig,
        worker_notify: Arc<Notify>,
        shutdown_token: CancellationToken,
    ) {
        let executor = JobExecutor::new(sandbox_config);
        let mut heartbeat_interval =
            tokio::time::interval(tokio::time::Duration::from_millis(2000));

        // Register self as worker
        {
            let mut assigner = job_assigner.write().await;
            assigner.register_worker(node_id);
        }

        loop {
            tokio::select! {
                _ = shutdown_token.cancelled() => {
                    tracing::info!("Worker loop shutting down");
                    break;
                }
                _ = worker_notify.notified() => {}
                _ = heartbeat_interval.tick() => {}
            }

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
                let completed_at = Utc::now();
                {
                    let mut queue = job_queue.write().await;
                    queue.update_job_result(
                        &job_id,
                        result.status,
                        node_id,
                        result.exit_code,
                        result.output,
                        result.error,
                        completed_at,
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
                        completed_at: Some(completed_at),
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
