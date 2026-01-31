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
    GetClusterStatusRequest, GetJobStatusRequest, JobStatus, ListJobsRequest, StreamJobsRequest,
    SubmitJobRequest,
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
    },
}

// =============================================================================
// Cluster Commands
// =============================================================================

#[derive(clap::Subcommand, Debug)]
enum ClusterCommands {
    /// Get cluster status and node information
    Status,
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

// =============================================================================
// Helper Functions
// =============================================================================

fn job_status_to_string(status: i32) -> String {
    match JobStatus::try_from(status) {
        Ok(JobStatus::Pending) => "PENDING".to_string(),
        Ok(JobStatus::Running) => "RUNNING".to_string(),
        Ok(JobStatus::Completed) => "COMPLETED".to_string(),
        Ok(JobStatus::Failed) => "FAILED".to_string(),
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

/// Find the leader's address by querying cluster status
/// Returns (leader_address, leader_id) if found
async fn find_leader_addr(
    client: &mut SchedulerServiceClient<Channel>,
    original_addr: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let response = client
        .get_cluster_status(GetClusterStatusRequest {})
        .await?
        .into_inner();

    let leader_id = response.leader_id;
    for node in response.nodes {
        if node.node_id == leader_id {
            // Handle 0.0.0.0 addresses - use the original host with leader's port
            let addr = if node.address.starts_with("0.0.0.0:") {
                // Extract port from leader's address
                if let Some(port) = node.address.strip_prefix("0.0.0.0:") {
                    // Extract host from original address (e.g., "http://127.0.0.1:50051" -> "127.0.0.1")
                    let original_host = original_addr
                        .trim_start_matches("http://")
                        .trim_start_matches("https://")
                        .split(':')
                        .next()
                        .unwrap_or("127.0.0.1");
                    format!("{}:{}", original_host, port)
                } else {
                    return Ok(None);
                }
            } else {
                node.address
            };
            return Ok(Some(addr));
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

    let sandbox = SandboxConfig {
        image: args.image,
        ..SandboxConfig::default()
    };

    let config = NodeConfig {
        node_id: args.node_id,
        listen_addr,
        peers,
        election_timeout_min_ms: 150,
        election_timeout_max_ms: 300,
        heartbeat_interval_ms: 50,
        sandbox,
        tls: tls_config,
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
            match output_format {
                OutputFormat::Json => {
                    let output = JobSubmitOutput {
                        job_id: resp.job_id,
                        created_at_ms: resp.created_at_ms,
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                OutputFormat::Table => {
                    println!("Job submitted successfully!");
                    println!("Job ID: {}", resp.job_id);
                }
            }
        }
        Err(status) => {
            let msg = status.message();
            // Check if this is a "not the leader" error
            if msg.contains("Not the leader") {
                // Try to find the leader and redirect
                if let Ok(Some(leader_addr)) = find_leader_addr(client, &client_args.addr).await {
                    let scheme = if client_args.addr.starts_with("https://") {
                        "https://"
                    } else {
                        "http://"
                    };
                    let leader_url = format!("{}{}", scheme, leader_addr);

                    eprintln!("Redirecting to leader at {}...", leader_addr);

                    // Create new connection to leader
                    let channel = create_channel_with_tls(
                        &leader_url,
                        &client_args.ca_cert,
                        &client_args.cert,
                        &client_args.key,
                    )
                    .await?;
                    let mut leader_client = SchedulerServiceClient::new(channel);

                    // Retry the request
                    match leader_client
                        .submit_job(SubmitJobRequest { command: cmd })
                        .await
                    {
                        Ok(response) => {
                            let resp = response.into_inner();
                            match output_format {
                                OutputFormat::Json => {
                                    let output = JobSubmitOutput {
                                        job_id: resp.job_id,
                                        created_at_ms: resp.created_at_ms,
                                    };
                                    println!("{}", serde_json::to_string_pretty(&output)?);
                                }
                                OutputFormat::Table => {
                                    println!("Job submitted successfully!");
                                    println!("Job ID: {}", resp.job_id);
                                }
                            }
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

async fn handle_job_status(
    client: &mut SchedulerServiceClient<Channel>,
    job_id: String,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let response = client
        .get_job_status(GetJobStatusRequest { job_id })
        .await?
        .into_inner();

    match output_format {
        OutputFormat::Json => {
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
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Table => {
            println!("Job ID:          {}", response.job_id);
            println!("Status:          {}", job_status_to_string(response.status));
            if let Some(exit_code) = response.exit_code {
                println!("Exit Code:       {}", exit_code);
            }
            if response.assigned_worker > 0 {
                println!("Assigned Worker: {}", response.assigned_worker);
            }
            if response.executed_by > 0 {
                println!("Executed By:     {}", response.executed_by);
            }
            if !response.output.is_empty() {
                println!("Output:");
                for line in response.output.lines() {
                    println!("  {}", line);
                }
            }
            if !response.error.is_empty() {
                println!("Error:");
                for line in response.error.lines() {
                    println!("  {}", line);
                }
            }
        }
    }
    Ok(())
}

async fn handle_job_list(
    client: &mut SchedulerServiceClient<Channel>,
    stream: bool,
    page_size: u32,
    all: bool,
    output_format: &OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut all_jobs: Vec<JobListItem> = Vec::new();
    #[allow(unused_assignments)]
    let mut total_count = 0u32;
    let mut has_more = false;

    if stream {
        // Use streaming API for efficient memory usage
        let mut job_stream = client
            .stream_jobs(StreamJobsRequest { status_filter: 0 })
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
        // Use paginated API
        let mut page_token = String::new();
        loop {
            let response = client
                .list_jobs(ListJobsRequest {
                    page_size,
                    page_token: page_token.clone(),
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

    match output_format {
        OutputFormat::Json => {
            let output = JobListOutput {
                jobs: all_jobs,
                total_count,
                has_more,
            };
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Table => {
            if all_jobs.is_empty() {
                println!("No jobs found.");
            } else {
                println!("{:<38} {:<12} {:<8} COMMAND", "JOB ID", "STATUS", "WORKER");
                println!("{}", "-".repeat(78));

                for job in &all_jobs {
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
                println!("Showing {} of {} jobs", all_jobs.len(), total_count);
                if has_more {
                    println!("(Use --all to fetch all pages)");
                }
            }
        }
    }
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

    match output_format {
        OutputFormat::Json => {
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
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        OutputFormat::Table => {
            println!("Cluster Status");
            println!("{}", "=".repeat(40));
            println!("Term:   {}", response.current_term);
            println!("Leader: Node {}", response.leader_id);
            println!();
            println!("Nodes:");
            println!("{:<8} {:<25} STATUS", "ID", "ADDRESS");
            println!("{}", "-".repeat(45));
            for node in response.nodes {
                let status = if node.is_alive { "alive" } else { "dead" };
                let status_icon = if node.is_alive { "[+]" } else { "[-]" };
                println!(
                    "{:<8} {:<25} {} {}",
                    node.node_id, node.address, status_icon, status
                );
            }
        }
    }
    Ok(())
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
                JobCommands::List {
                    stream,
                    page_size,
                    all,
                } => {
                    handle_job_list(&mut grpc_client, stream, page_size, all, &client.output)
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
            }
        }
    }

    Ok(())
}
