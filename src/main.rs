use clap::{Parser, ValueEnum};
use serde::Serialize;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio_stream::StreamExt;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use tracing_subscriber::EnvFilter;

use nomad_lite::config::{NodeConfig, PeerConfig, SandboxConfig, TlsConfig};
use nomad_lite::node::Node;
use nomad_lite::proto::scheduler_service_client::SchedulerServiceClient;
use nomad_lite::proto::{
    CancelJobRequest, DrainNodeRequest, GetClusterStatusRequest, GetJobStatusRequest,
    GetRaftLogEntriesRequest, JobStatus, ListJobsRequest, StreamJobsRequest, SubmitJobRequest,
    TransferLeadershipRequest,
};
use nomad_lite::tls::TlsIdentity;

#[derive(Parser, Debug)]
#[command(name = "nomad-lite")]
#[command(version)]
#[command(about = "A distributed job scheduler with Raft consensus")]
#[command(propagate_version = true)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Start a nomad-lite server node
    Server(ServerArgs),

    /// Job management commands
    Job {
        #[command(flatten)]
        client: ClientArgs,

        #[command(subcommand)]
        command: JobCommands,
    },

    /// Cluster management commands
    Cluster {
        #[command(flatten)]
        client: ClientArgs,

        #[command(subcommand)]
        command: ClusterCommands,
    },

    /// Raft log commands
    Log {
        #[command(flatten)]
        client: ClientArgs,

        #[command(subcommand)]
        command: LogCommands,
    },
}

// =============================================================================
// Server Arguments
// =============================================================================

#[derive(Parser, Debug)]
struct ServerArgs {
    /// Node ID (unique identifier for this node)
    #[arg(long, default_value = "1")]
    node_id: u64,

    /// Port to listen on for gRPC
    #[arg(long, default_value = "50051")]
    port: u16,

    /// Port for the web dashboard (optional)
    #[arg(long)]
    dashboard_port: Option<u16>,

    /// Peer addresses (comma-separated, format: "id:host:port")
    /// Example: "2:127.0.0.1:50052,3:127.0.0.1:50053"
    #[arg(long, default_value = "")]
    peers: String,

    /// Docker image to use for job execution
    #[arg(long, default_value = "alpine:latest")]
    image: String,

    // === TLS Options ===
    /// Enable TLS for all gRPC communication
    #[arg(long)]
    tls: bool,

    /// Path to CA certificate (PEM format)
    #[arg(long, requires = "tls")]
    ca_cert: Option<PathBuf>,

    /// Path to node certificate (PEM format)
    #[arg(long, requires = "tls")]
    cert: Option<PathBuf>,

    /// Path to node private key (PEM format)
    #[arg(long, requires = "tls")]
    key: Option<PathBuf>,

    /// Allow running without TLS even when --tls is specified but certs are missing.
    /// Useful for development. NOT recommended for production.
    #[arg(long)]
    allow_insecure: bool,

    /// Directory for RocksDB state persistence (optional).
    /// When set, the node survives restarts without losing committed state.
    /// Omit for in-memory-only operation (default).
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Address to advertise to peers and report in cluster status.
    /// Defaults to the listen address with 0.0.0.0 replaced by 127.0.0.1.
    /// Override when nodes must be reachable at a specific hostname or external IP.
    /// Format: host:port (e.g., "192.168.1.10:50051")
    #[arg(long)]
    advertise_addr: Option<String>,
}

// =============================================================================
// Client Arguments (shared by job and cluster commands)
// =============================================================================

#[derive(Parser, Debug)]
struct ClientArgs {
    /// Server address (use https:// for TLS)
    #[arg(long, short = 'a', default_value = "http://127.0.0.1:50051")]
    addr: String,

    /// Path to CA certificate (PEM format) for TLS
    #[arg(long)]
    ca_cert: Option<PathBuf>,

    /// Path to client certificate (PEM format) for mTLS
    #[arg(long)]
    cert: Option<PathBuf>,

    /// Path to client private key (PEM format) for mTLS
    #[arg(long)]
    key: Option<PathBuf>,

    /// Output format
    #[arg(long, short = 'o', default_value = "table")]
    output: OutputFormat,
}

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

/// Render output in the requested format.
/// Serialises `data` as pretty JSON, or calls `table` for table output.
fn render<T: Serialize>(
    fmt: &OutputFormat,
    data: &T,
    table: impl FnOnce(),
) -> Result<(), Box<dyn std::error::Error>> {
    match fmt {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(data)?),
        OutputFormat::Table => table(),
    }
    Ok(())
}

// =============================================================================
// Job Commands
// =============================================================================

#[derive(clap::Subcommand, Debug)]
enum JobCommands {
    /// Submit a new job to the cluster
    Submit {
        /// The command to execute (e.g., "echo hello")
        command: String,
    },
    /// Get status of a specific job
    Status {
        /// The job ID (UUID)
        job_id: String,
    },
    /// Cancel a pending or running job
    Cancel {
        /// The job ID to cancel (UUID)
        job_id: String,
    },
    /// List all jobs
    List {
        /// Use streaming API (memory-efficient for large job lists)
        #[arg(short, long)]
        stream: bool,

        /// Number of jobs per page (default: 100, max: 1000)
        #[arg(long, default_value = "100")]
        page_size: u32,

        /// Fetch all pages automatically
        #[arg(long)]
        all: bool,

        /// Filter by job status: pending, running, completed, failed, cancelled
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,

        /// Filter by worker node ID (matches assigned_worker or executed_by)
        #[arg(long, value_name = "WORKER_ID")]
        worker: Option<u64>,

        /// Filter by command substring (case-insensitive)
        #[arg(long, value_name = "SUBSTR")]
        command_substr: Option<String>,

        /// Only jobs created at or after this Unix timestamp in milliseconds
        #[arg(long, value_name = "MS")]
        created_after_ms: Option<i64>,

        /// Only jobs created at or before this Unix timestamp in milliseconds
        #[arg(long, value_name = "MS")]
        created_before_ms: Option<i64>,
    },
}

// =============================================================================
// Cluster Commands
// =============================================================================

#[derive(clap::Subcommand, Debug)]
enum ClusterCommands {
    /// Get cluster status and node information
    Status,
    /// Transfer leadership to another node
    TransferLeader {
        /// Target node ID (omit for auto-select)
        #[arg(long)]
        to: Option<u64>,
    },
    /// Drain this node: stop accepting jobs, finish running jobs, transfer leadership
    Drain,
}

// =============================================================================
// Log Commands
// =============================================================================

#[derive(clap::Subcommand, Debug)]
enum LogCommands {
    /// List Raft log entries
    List {
        /// Starting index (1-indexed, 0 means from beginning)
        #[arg(long, default_value = "0")]
        start_index: u64,

        /// Maximum number of entries to return
        #[arg(long, default_value = "100")]
        limit: u32,
    },
}

// =============================================================================
// List Jobs Filters
// =============================================================================

struct ListJobsFilters {
    status_str: Option<String>,
    worker_id: Option<u64>,
    command_substr: Option<String>,
    created_after_ms: Option<i64>,
    created_before_ms: Option<i64>,
}

// =============================================================================
// JSON Output Types
// =============================================================================

#[derive(Serialize)]
struct JobSubmitOutput {
    job_id: String,
    created_at_ms: i64,
}

#[derive(Serialize)]
struct JobStatusOutput {
    job_id: String,
    status: String,
    output: String,
    error: String,
    assigned_worker: u64,
    executed_by: u64,
    exit_code: Option<i32>,
    created_at_ms: i64,
    completed_at_ms: Option<i64>,
}

#[derive(Serialize)]
struct JobListItem {
    job_id: String,
    status: String,
    command: String,
    assigned_worker: u64,
    created_at_ms: i64,
}

#[derive(Serialize)]
struct JobListOutput {
    jobs: Vec<JobListItem>,
    total_count: u32,
    has_more: bool,
}

#[derive(Serialize)]
struct NodeInfoOutput {
    node_id: u64,
    address: String,
    is_alive: bool,
}

#[derive(Serialize)]
struct ClusterStatusOutput {
    current_term: u64,
    leader_id: u64,
    nodes: Vec<NodeInfoOutput>,
}

#[derive(Serialize)]
struct TransferLeaderOutput {
    success: bool,
    message: String,
    new_leader_id: u64,
}

#[derive(Serialize)]
struct DrainOutput {
    success: bool,
    message: String,
}

#[derive(Serialize)]
struct RaftLogEntryOutput {
    index: u64,
    term: u64,
    command_type: String,
    command_details: String,
    is_committed: bool,
}

#[derive(Serialize)]
struct RaftLogOutput {
    entries: Vec<RaftLogEntryOutput>,
    commit_index: u64,
    last_log_index: u64,
    first_available_index: u64,
}

// =============================================================================
// Helper Functions
// =============================================================================

fn job_status_to_string(status: i32) -> String {
    match JobStatus::try_from(status) {
        Ok(JobStatus::Pending) => "PENDING".to_string(),
        Ok(JobStatus::Running) => "RUNNING".to_string(),
        Ok(JobStatus::Completed) => "COMPLETED".to_string(),
        Ok(JobStatus::Failed) => "FAILED".to_string(),
        Ok(JobStatus::Cancelled) => "CANCELLED".to_string(),
        _ => "UNKNOWN".to_string(),
    }
}

fn parse_peers(peers_str: &str) -> Vec<PeerConfig> {
    if peers_str.is_empty() {
        return Vec::new();
    }

    peers_str
        .split(',')
        .filter_map(|peer| {
            let parts: Vec<&str> = peer.trim().split(':').collect();
            if parts.len() == 3 {
                let node_id: u64 = parts[0].parse().ok()?;
                let host = parts[1];
                let port = parts[2];
                let addr = format!("{}:{}", host, port);
                Some(PeerConfig { node_id, addr })
            } else {
                tracing::warn!(peer, "Invalid peer format, expected id:host:port");
                None
            }
        })
        .collect()
}

async fn create_client_channel(args: &ClientArgs) -> Result<Channel, Box<dyn std::error::Error>> {
    create_channel_with_tls(&args.addr, &args.ca_cert, &args.cert, &args.key).await
}

async fn create_channel_with_tls(
    addr: &str,
    ca_cert: &Option<PathBuf>,
    cert: &Option<PathBuf>,
    key: &Option<PathBuf>,
) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = Channel::from_shared(addr.to_string())?;

    // Check if TLS is configured
    let has_tls = ca_cert.is_some() || addr.starts_with("https://");

    if has_tls {
        let mut tls_config = ClientTlsConfig::new().domain_name("nomad-lite-cluster");

        // Load CA certificate if provided
        if let Some(ca_path) = ca_cert {
            let ca_cert_data = tokio::fs::read(ca_path).await?;
            let ca_cert = Certificate::from_pem(ca_cert_data);
            tls_config = tls_config.ca_certificate(ca_cert);
        }

        // Load client certificate and key for mTLS if both provided
        if let (Some(cert_path), Some(key_path)) = (cert, key) {
            let cert_data = tokio::fs::read(cert_path).await?;
            let key_data = tokio::fs::read(key_path).await?;
            let identity = Identity::from_pem(cert_data, key_data);
            tls_config = tls_config.identity(identity);
        }

        Ok(endpoint.tls_config(tls_config)?.connect().await?)
    } else {
        Ok(endpoint.connect().await?)
    }
}

/// Parse the leader's network address from a not-leader error message.
///
/// The server embeds the address in messages of the form
/// `"Not the leader. Try <addr>"`. Returns `Some(addr)` on success so the
/// CLI can redirect without an extra RPC.
fn leader_addr_from_error(msg: &str) -> Option<&str> {
    msg.strip_prefix("Not the leader. Try ")
}

/// Resolve the full leader URL for redirect.
///
/// First tries to parse the address from the error message (fast path, no
/// extra RPC). Falls back to a `get_cluster_status` query when the error
/// message doesn't embed an address.
async fn resolve_leader_url(
    msg: &str,
    client: &mut SchedulerServiceClient<Channel>,
    client_args: &ClientArgs,
) -> Option<String> {
    let scheme = if client_args.addr.starts_with("https://") {
        "https://"
    } else {
        "http://"
    };
    if let Some(addr) = leader_addr_from_error(msg) {
        return Some(format!("{}{}", scheme, addr));
    }
    if let Ok(Some(addr)) = find_leader_addr(client).await {
        return Some(format!("{}{}", scheme, addr));
    }
    None
}

/// Find the leader's address by querying cluster status
async fn find_leader_addr(
    client: &mut SchedulerServiceClient<Channel>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let response = client
        .get_cluster_status(GetClusterStatusRequest {})
        .await?
        .into_inner();

    let leader_id = response.leader_id;
    for node in response.nodes {
        if node.node_id == leader_id {
            return Ok(Some(node.address));
        }
    }
    Ok(None)
}

// =============================================================================
// Server Implementation
// =============================================================================

async fn run_server(args: ServerArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Build TLS configuration
    let tls_config = TlsConfig {
        enabled: args.tls,
        ca_cert_path: args.ca_cert,
        cert_path: args.cert,
        key_path: args.key,
        allow_insecure: args.allow_insecure,
    };

    // Validate and load TLS identity if configured
    let tls_identity = if tls_config.is_complete() {
        match TlsIdentity::load(&tls_config).await {
            Ok(identity) => {
                tracing::info!("TLS enabled with mTLS authentication");
                Some(identity)
            }
            Err(e) => {
                if tls_config.allow_insecure {
                    tracing::warn!(
                        error = %e,
                        "TLS certificate loading failed, running in insecure mode"
                    );
                    None
                } else {
                    return Err(format!("TLS certificate loading failed: {}", e).into());
                }
            }
        }
    } else if tls_config.enabled {
        // TLS requested but not all paths provided
        if tls_config.allow_insecure {
            tracing::warn!(
                "TLS enabled but certificate paths incomplete, running in insecure mode"
            );
            None
        } else {
            return Err("TLS enabled but missing required paths (--ca-cert, --cert, --key)".into());
        }
    } else {
        // TLS not requested
        if !args.peers.is_empty() {
            tracing::warn!(
                "Running without TLS in a multi-node cluster. \
                 Consider using --tls for production deployments."
            );
        }
        None
    };

    let listen_addr: SocketAddr = format!("0.0.0.0:{}", args.port).parse()?;
    let dashboard_addr: Option<SocketAddr> = match args.dashboard_port {
        Some(p) => Some(format!("0.0.0.0:{}", p).parse()?),
        None => None,
    };
    let peers = parse_peers(&args.peers);

    // Compute the advertise address: use --advertise-addr if provided, otherwise
    // replace the wildcard bind address (0.0.0.0) with loopback (127.0.0.1).
    let advertise_addr: SocketAddr = match args.advertise_addr {
        Some(ref s) => s
            .parse()
            .map_err(|e| format!("invalid --advertise-addr: {e}"))?,
        None => format!("127.0.0.1:{}", args.port).parse()?,
    };

    let sandbox = SandboxConfig {
        image: args.image,
        ..SandboxConfig::default()
    };

    let config = NodeConfig {
        node_id: args.node_id,
        listen_addr,
        advertise_addr,
        peers,
        election_timeout_min_ms: 150,
        election_timeout_max_ms: 300,
        heartbeat_interval_ms: 50,
        sandbox,
        tls: tls_config,
        data_dir: args.data_dir,
    };

    tracing::info!(
        node_id = config.node_id,
        listen_addr = %config.listen_addr,
        dashboard_addr = ?dashboard_addr,
        tls_enabled = tls_identity.is_some(),
        peers = ?config.peers.iter().map(|p| format!("{}:{}", p.node_id, p.addr)).collect::<Vec<_>>(),
        "Starting nomad-lite node"
    );

    let (node, raft_rx) = Node::new(config, dashboard_addr, tls_identity);
    node.run(raft_rx).await?;

    Ok(())
}

// =============================================================================
// Client Command Handlers
// =============================================================================

async fn handle_job_submit(
    client: &mut SchedulerServiceClient<Channel>,
    cmd: String,
    output_format: &OutputFormat,
    client_args: &ClientArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    match client
        .submit_job(SubmitJobRequest {
            command: cmd.clone(),
        })
        .await
    {
        Ok(response) => {
            let resp = response.into_inner();
            let job_id = resp.job_id.clone();
            let output = JobSubmitOutput {
                job_id: resp.job_id,
                created_at_ms: resp.created_at_ms,
            };
            render(output_format, &output, || {
                println!("Job submitted successfully!");
                println!("Job ID: {}", job_id);
            })?;
        }
        Err(status) => {
            let msg = status.message().to_owned();
            // Check if this is a "not the leader" error and try to redirect
            if msg.contains("Not the leader") {
                if let Some(leader_url) = resolve_leader_url(&msg, client, client_args).await {
                    eprintln!("Redirecting to leader at {}...", leader_url);
                    let channel = create_channel_with_tls(
                        &leader_url,
                        &client_args.ca_cert,
                        &client_args.cert,
                        &client_args.key,
                    )
                    .await?;
                    let mut leader_client = SchedulerServiceClient::new(channel);
                    match leader_client
                        .submit_job(SubmitJobRequest { command: cmd })
                        .await
                    {
                        Ok(response) => {
                            let resp = response.into_inner();
                            let job_id = resp.job_id.clone();
                            let output = JobSubmitOutput {
                                job_id: resp.job_id,
                                created_at_ms: resp.created_at_ms,
                            };
                            render(output_format, &output, || {
                                println!("Job submitted successfully!");
                                println!("Job ID: {}", job_id);
                            })?;
                            return Ok(());
                        }
                        Err(e) => {
                            eprintln!(
                                "Error: Job submission failed after redirect: {}",
                                e.message()
                            );
                            std::process::exit(1);
                        }
                    }
                } else {
                    eprintln!("Error: {}", msg);
                    eprintln!("Hint: Use -a to specify the leader's address, e.g.:");
                    eprintln!("  nomad-lite -a http://<leader-ip>:<port> job submit ...");
                    std::process::exit(1);
                }
            } else {
                eprintln!("Error: Job submission failed: {}", msg);
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

async fn handle_job_cancel(
    client: &mut SchedulerServiceClient<Channel>,
    job_id: String,
    output_format: &OutputFormat,
    client_args: &ClientArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Serialize)]
    struct CancelOutput {
        success: bool,
        message: String,
    }

    match client
        .cancel_job(CancelJobRequest {
            job_id: job_id.clone(),
        })
        .await
    {
        Ok(response) => {
            let resp = response.into_inner();
            let output = CancelOutput {
                success: resp.success,
                message: resp.message,
            };
            render(output_format, &output, || {
                if output.success {
                    println!("{}", output.message);
                } else {
                    eprintln!("Cancel failed: {}", output.message);
                    std::process::exit(1);
                }
            })?;
        }
        Err(status) => {
            let msg = status.message().to_owned();
            if msg.contains("Not the leader") {
                if let Some(leader_url) = resolve_leader_url(&msg, client, client_args).await {
                    eprintln!("Redirecting to leader at {}...", leader_url);
                    let channel = create_channel_with_tls(
                        &leader_url,
                        &client_args.ca_cert,
                        &client_args.cert,
                        &client_args.key,
                    )
                    .await?;
                    let mut leader_client = SchedulerServiceClient::new(channel);
                    match leader_client.cancel_job(CancelJobRequest { job_id }).await {
                        Ok(response) => {
                            let resp = response.into_inner();
                            let output = CancelOutput {
                                success: resp.success,
                                message: resp.message,
                            };
                            render(output_format, &output, || {
                                if output.success {
                                    println!("{}", output.message);
                                } else {
                                    eprintln!("Cancel failed: {}", output.message);
                                    std::process::exit(1);
                                }
                            })?;
                        }
                        Err(e) => {
                            eprintln!("Error: Cancel failed after redirect: {}", e.message());
                            std::process::exit(1);
                        }
                    }
                } else {
                    eprintln!("Error: {}", msg);
                    eprintln!("Hint: Use -a to specify the leader's address, e.g.:");
                    eprintln!("  nomad-lite -a http://<leader-ip>:<port> job cancel ...");
                    std::process::exit(1);
                }
            } else {
                eprintln!("Error: Cancel failed: {}", msg);
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

async fn handle_job_status(
    client: &mut SchedulerServiceClient<Channel>,
    job_id: String,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_job_status(GetJobStatusRequest { job_id })
        .await?
        .into_inner();

    let output = JobStatusOutput {
        job_id: response.job_id,
        status: job_status_to_string(response.status),
        output: response.output,
        error: response.error,
        assigned_worker: response.assigned_worker,
        executed_by: response.executed_by,
        exit_code: response.exit_code,
        created_at_ms: response.created_at_ms,
        completed_at_ms: response.completed_at_ms,
    };
    render(output_format, &output, || {
        println!("Job ID:          {}", output.job_id);
        println!("Status:          {}", output.status);
        if let Some(exit_code) = output.exit_code {
            println!("Exit Code:       {}", exit_code);
        }
        if output.assigned_worker > 0 {
            println!("Assigned Worker: {}", output.assigned_worker);
        }
        if output.executed_by > 0 {
            println!("Executed By:     {}", output.executed_by);
        }
        if !output.output.is_empty() {
            println!("Output:");
            for line in output.output.lines() {
                println!("  {}", line);
            }
        }
        if !output.error.is_empty() {
            println!("Error:");
            for line in output.error.lines() {
                println!("  {}", line);
            }
        }
    })?;
    Ok(())
}

async fn handle_job_list(
    client: &mut SchedulerServiceClient<Channel>,
    stream: bool,
    page_size: u32,
    all: bool,
    filters: ListJobsFilters,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let ListJobsFilters {
        status_str,
        worker_id,
        command_substr,
        created_after_ms,
        created_before_ms,
    } = filters;
    // Parse --status string into the proto enum integer value
    let status_filter: i32 = match status_str.as_deref().map(|s| s.to_ascii_lowercase()) {
        None => 0, // JOB_STATUS_UNSPECIFIED = no filter
        Some(ref s) => match s.as_str() {
            "pending" => JobStatus::Pending as i32,
            "running" => JobStatus::Running as i32,
            "completed" => JobStatus::Completed as i32,
            "failed" => JobStatus::Failed as i32,
            "cancelled" => JobStatus::Cancelled as i32,
            other => {
                eprintln!(
                    "Error: Unknown status '{}'. Valid values: pending, running, completed, failed, cancelled",
                    other
                );
                std::process::exit(1);
            }
        },
    };

    let mut all_jobs: Vec<JobListItem> = Vec::new();
    #[allow(unused_assignments)]
    let mut total_count = 0u32;
    let mut has_more = false;

    if stream {
        // Streaming only supports the status filter; warn if others are set
        if worker_id.is_some()
            || command_substr.is_some()
            || created_after_ms.is_some()
            || created_before_ms.is_some()
        {
            eprintln!(
                "Warning: --stream only supports --status; \
                 --worker, --command-substr, and time filters are ignored"
            );
        }

        let mut job_stream = client
            .stream_jobs(StreamJobsRequest { status_filter })
            .await?
            .into_inner();

        while let Some(result) = job_stream.next().await {
            match result {
                Ok(job) => {
                    all_jobs.push(JobListItem {
                        job_id: job.job_id,
                        status: job_status_to_string(job.status),
                        command: job.command,
                        assigned_worker: job.assigned_worker,
                        created_at_ms: job.created_at_ms,
                    });
                }
                Err(e) => {
                    eprintln!("Stream error: {}", e);
                    break;
                }
            }
        }
        total_count = all_jobs.len() as u32;
    } else {
        // Use paginated API with full filter support
        let mut page_token = String::new();
        loop {
            let response = client
                .list_jobs(ListJobsRequest {
                    page_size,
                    page_token: page_token.clone(),
                    status_filter,
                    worker_id_filter: worker_id.unwrap_or(0),
                    command_filter: command_substr.clone().unwrap_or_default(),
                    created_after_ms: created_after_ms.unwrap_or(0),
                    created_before_ms: created_before_ms.unwrap_or(0),
                })
                .await?
                .into_inner();

            total_count = response.total_count;

            for job in response.jobs {
                all_jobs.push(JobListItem {
                    job_id: job.job_id,
                    status: job_status_to_string(job.status),
                    command: job.command,
                    assigned_worker: job.assigned_worker,
                    created_at_ms: job.created_at_ms,
                });
            }

            if response.next_page_token.is_empty() || !all {
                has_more = !response.next_page_token.is_empty();
                break;
            }
            page_token = response.next_page_token;
        }
    }

    let output = JobListOutput {
        jobs: all_jobs,
        total_count,
        has_more,
    };
    render(output_format, &output, || {
        if output.jobs.is_empty() {
            println!("No jobs found.");
        } else {
            println!("{:<38} {:<12} {:<8} COMMAND", "JOB ID", "STATUS", "WORKER");
            println!("{}", "-".repeat(78));

            for job in &output.jobs {
                let worker = if job.assigned_worker > 0 {
                    job.assigned_worker.to_string()
                } else {
                    "-".to_string()
                };
                // Truncate command if too long
                let cmd_display = if job.command.len() > 20 {
                    format!("{}...", &job.command[..17])
                } else {
                    job.command.clone()
                };
                println!(
                    "{:<38} {:<12} {:<8} {}",
                    job.job_id, job.status, worker, cmd_display
                );
            }
            println!();
            println!(
                "Showing {} of {} jobs",
                output.jobs.len(),
                output.total_count
            );
            if output.has_more {
                println!("(Use --all to fetch all pages)");
            }
        }
    })?;
    Ok(())
}

async fn handle_cluster_status(
    client: &mut SchedulerServiceClient<Channel>,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_cluster_status(GetClusterStatusRequest {})
        .await?
        .into_inner();

    let output = ClusterStatusOutput {
        current_term: response.current_term,
        leader_id: response.leader_id,
        nodes: response
            .nodes
            .into_iter()
            .map(|n| NodeInfoOutput {
                node_id: n.node_id,
                address: n.address,
                is_alive: n.is_alive,
            })
            .collect(),
    };
    render(output_format, &output, || {
        println!("Cluster Status");
        println!("{}", "=".repeat(40));
        println!("Term:   {}", output.current_term);
        println!("Leader: Node {}", output.leader_id);
        println!();
        println!("Nodes:");
        println!("{:<8} {:<25} STATUS", "ID", "ADDRESS");
        println!("{}", "-".repeat(45));
        for node in &output.nodes {
            let status = if node.is_alive { "alive" } else { "dead" };
            let status_icon = if node.is_alive { "[+]" } else { "[-]" };
            println!(
                "{:<8} {:<25} {} {}",
                node.node_id, node.address, status_icon, status
            );
        }
    })?;
    Ok(())
}

async fn handle_log_list(
    client: &mut SchedulerServiceClient<Channel>,
    start_index: u64,
    limit: u32,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_raft_log_entries(GetRaftLogEntriesRequest { start_index, limit })
        .await?
        .into_inner();

    // Convert proto entries to output format
    let entries: Vec<RaftLogEntryOutput> = response
        .entries
        .into_iter()
        .map(|entry| {
            let (command_type, command_details) = format_command(&entry.command);
            RaftLogEntryOutput {
                index: entry.index,
                term: entry.term,
                command_type,
                command_details,
                is_committed: entry.is_committed,
            }
        })
        .collect();

    let output = RaftLogOutput {
        entries,
        commit_index: response.commit_index,
        last_log_index: response.last_log_index,
        first_available_index: response.first_available_index,
    };
    render(output_format, &output, || {
        if output.entries.is_empty() {
            println!("No log entries found.");
        } else {
            println!("Raft Log Entries");
            println!("{}", "=".repeat(80));
            if output.first_available_index > 1 {
                println!(
                    "First Available: {}  |  Commit Index: {}  |  Last Log Index: {}",
                    output.first_available_index, output.commit_index, output.last_log_index
                );
                println!(
                    "(Entries 1-{} were compacted into a snapshot)",
                    output.first_available_index - 1
                );
            } else {
                println!(
                    "Commit Index: {}  |  Last Log Index: {}",
                    output.commit_index, output.last_log_index
                );
            }
            println!();
            println!(
                "{:<6} {:<6} {:<10} {:<20} DETAILS",
                "INDEX", "TERM", "COMMITTED", "TYPE"
            );
            println!("{}", "-".repeat(80));

            for entry in &output.entries {
                let committed = if entry.is_committed { "yes" } else { "no" };
                // Truncate details if too long
                let details = if entry.command_details.len() > 30 {
                    format!("{}...", &entry.command_details[..27])
                } else {
                    entry.command_details.clone()
                };
                println!(
                    "{:<6} {:<6} {:<10} {:<20} {}",
                    entry.index, entry.term, committed, entry.command_type, details
                );
            }
            println!();
            println!("Showing {} entries", output.entries.len());
        }
    })?;
    Ok(())
}

async fn handle_transfer_leader(
    client: &mut SchedulerServiceClient<Channel>,
    target: Option<u64>,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .transfer_leadership(TransferLeadershipRequest {
            target_node_id: target.unwrap_or(0),
        })
        .await?
        .into_inner();

    let output = TransferLeaderOutput {
        success: response.success,
        message: response.message,
        new_leader_id: response.new_leader_id,
    };
    render(output_format, &output, || {
        if output.success {
            println!("Leadership transferred successfully!");
            println!("New leader: Node {}", output.new_leader_id);
        } else {
            eprintln!("Transfer failed: {}", output.message);
            std::process::exit(1);
        }
    })?;
    Ok(())
}

async fn handle_drain(
    client: &mut SchedulerServiceClient<Channel>,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("Draining node...");
    let response = client.drain_node(DrainNodeRequest {}).await?.into_inner();

    let output = DrainOutput {
        success: response.success,
        message: response.message,
    };
    render(output_format, &output, || {
        if output.success {
            println!("Node drained successfully.");
            println!("{}", output.message);
        } else {
            eprintln!("Drain failed: {}", output.message);
            std::process::exit(1);
        }
    })?;
    Ok(())
}

fn format_command(command: &Option<nomad_lite::proto::Command>) -> (String, String) {
    use nomad_lite::proto::command::CommandType;

    match command {
        Some(cmd) => match &cmd.command_type {
            Some(CommandType::SubmitJob(submit)) => (
                "SubmitJob".to_string(),
                format!("job_id={}, cmd={}", submit.job_id, submit.command),
            ),
            Some(CommandType::UpdateJobStatus(update)) => {
                let status = match nomad_lite::proto::JobStatus::try_from(update.status) {
                    Ok(s) => format!("{:?}", s),
                    Err(_) => "Unknown".to_string(),
                };
                (
                    "UpdateJobStatus".to_string(),
                    format!("job_id={}, status={}", update.job_id, status),
                )
            }
            Some(CommandType::RegisterWorker(register)) => (
                "RegisterWorker".to_string(),
                format!("worker_id={}", register.worker_id),
            ),
            Some(CommandType::BatchUpdateJobStatus(batch)) => (
                "BatchUpdateJobStatus".to_string(),
                format!("{} updates", batch.updates.len()),
            ),
            Some(CommandType::AssignJob(assign)) => (
                "AssignJob".to_string(),
                format!("job_id={}, worker_id={}", assign.job_id, assign.worker_id),
            ),
            Some(CommandType::CancelJob(c)) => ("CancelJob".to_string(), c.job_id.clone()),
            None => ("Noop".to_string(), String::new()),
        },
        None => ("Noop".to_string(), String::new()),
    }
}

// =============================================================================
// Main Entry Point
// =============================================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    match args.command {
        Commands::Server(server_args) => {
            run_server(server_args).await?;
        }
        Commands::Job { client, command } => {
            let channel = create_client_channel(&client).await?;
            let mut grpc_client = SchedulerServiceClient::new(channel);

            match command {
                JobCommands::Submit { command: cmd } => {
                    handle_job_submit(&mut grpc_client, cmd, &client.output, &client).await?;
                }
                JobCommands::Status { job_id } => {
                    handle_job_status(&mut grpc_client, job_id, &client.output).await?;
                }
                JobCommands::Cancel { job_id } => {
                    handle_job_cancel(&mut grpc_client, job_id, &client.output, &client).await?;
                }
                JobCommands::List {
                    stream,
                    page_size,
                    all,
                    status,
                    worker,
                    command_substr,
                    created_after_ms,
                    created_before_ms,
                } => {
                    handle_job_list(
                        &mut grpc_client,
                        stream,
                        page_size,
                        all,
                        ListJobsFilters {
                            status_str: status,
                            worker_id: worker,
                            command_substr,
                            created_after_ms,
                            created_before_ms,
                        },
                        &client.output,
                    )
                    .await?;
                }
            }
        }
        Commands::Cluster { client, command } => {
            let channel = create_client_channel(&client).await?;
            let mut grpc_client = SchedulerServiceClient::new(channel);

            match command {
                ClusterCommands::Status => {
                    handle_cluster_status(&mut grpc_client, &client.output).await?;
                }
                ClusterCommands::TransferLeader { to } => {
                    handle_transfer_leader(&mut grpc_client, to, &client.output).await?;
                }
                ClusterCommands::Drain => {
                    handle_drain(&mut grpc_client, &client.output).await?;
                }
            }
        }
        Commands::Log { client, command } => {
            let channel = create_client_channel(&client).await?;
            let mut grpc_client = SchedulerServiceClient::new(channel);

            match command {
                LogCommands::List { start_index, limit } => {
                    handle_log_list(&mut grpc_client, start_index, limit, &client.output).await?;
                }
            }
        }
    }

    Ok(())
}
