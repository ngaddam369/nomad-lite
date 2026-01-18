use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::NodeConfig;
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
            config,
            raft_node: Arc::new(raft_node),
            job_queue: Arc::new(RwLock::new(JobQueue::new())),
            job_assigner: Arc::new(RwLock::new(JobAssigner::new(5000))), // 5s worker timeout
            executor: JobExecutor::new(),
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
    /// # Note
    ///
    /// This method blocks on the gRPC server. All other components run as
    /// spawned tasks. If the gRPC server fails, the node will stop.
    pub async fn run(self, raft_rx: tokio::sync::mpsc::Receiver<crate::raft::node::RaftMessage>) {
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
        tokio::spawn(async move {
            Self::worker_loop(node_id, worker_raft, worker_queue, worker_assigner).await;
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
            self.raft_node.clone(),
            self.job_queue.clone(),
        );
        if let Err(e) = server.run().await {
            tracing::error!(error = %e, "gRPC server failed");
        }
    }

    /// Scheduler loop that maintains consistent state and assigns jobs.
    ///
    /// This loop runs on all nodes and performs two key functions:
    ///
    /// ## 1. Apply Committed Entries (all nodes)
    /// Polls Raft for newly committed log entries and applies them to local state:
    /// - `SubmitJob`: Adds job to queue if not already present (idempotent)
    /// - `UpdateJobStatus`: Updates job status, output, and error
    /// - `RegisterWorker`: Registers worker with the job assigner
    ///
    /// This ensures all nodes maintain identical job queue state.
    ///
    /// ## 2. Assign Jobs (leader only)
    /// The leader assigns pending jobs to available workers using
    /// least-loaded scheduling. Followers skip this step.
    ///
    /// # Polling Interval
    /// Runs every 100ms. See TODO for event-driven improvements.
    async fn scheduler_loop(
        raft_node: Arc<RaftNode>,
        job_queue: Arc<RwLock<JobQueue>>,
        job_assigner: Arc<RwLock<JobAssigner>>,
    ) {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(100));

        loop {
            interval.tick().await;

            // All nodes apply committed entries to maintain consistent state
            let entries = raft_node.get_committed_entries().await;
            for entry in entries {
                match entry.command {
                    Command::SubmitJob { job_id, command } => {
                        let mut queue = job_queue.write().await;
                        if queue.get_job(&job_id).is_none() {
                            queue.add_job(Job::with_id(job_id, command));
                            tracing::debug!(job_id = %job_id, "Job added from committed entry");
                        }
                    }
                    Command::UpdateJobStatus {
                        job_id,
                        status,
                        output,
                        error,
                    } => {
                        let mut queue = job_queue.write().await;
                        queue.update_status(&job_id, status, output, error);
                        tracing::debug!(job_id = %job_id, status = %status, "Job status updated");
                    }
                    Command::RegisterWorker { worker_id } => {
                        job_assigner.write().await.register_worker(worker_id);
                    }
                    Command::Noop => {}
                }
            }

            // Only leader assigns pending jobs to available workers
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
    ) {
        let executor = JobExecutor::new();
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

                // Update job status
                {
                    let mut queue = job_queue.write().await;
                    queue.update_status(&job_id, result.status, result.output, result.error);
                }

                // Mark job completed in assigner
                {
                    let mut assigner = job_assigner.write().await;
                    assigner.job_completed(node_id, &job_id);
                }

                // If leader, replicate the status update
                if raft_node.is_leader().await {
                    let command = Command::UpdateJobStatus {
                        job_id,
                        status: result.status,
                        output: None, // Don't replicate full output
                        error: None,
                    };
                    let (tx, _rx) = tokio::sync::oneshot::channel();
                    let _ = raft_node
                        .message_sender()
                        .send(crate::raft::node::RaftMessage::AppendCommand {
                            command,
                            response_tx: tx,
                        })
                        .await;
                }
            }
        }
    }
}
