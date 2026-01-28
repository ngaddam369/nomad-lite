use clap::Parser;
use nomad_lite::proto::scheduler_service_client::SchedulerServiceClient;
use nomad_lite::proto::{
    GetClusterStatusRequest, GetJobStatusRequest, ListJobsRequest, StreamJobsRequest,
    SubmitJobRequest,
};
use std::path::PathBuf;
use tokio_stream::StreamExt;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};

#[derive(Parser, Debug)]
#[command(name = "submit-job")]
#[command(about = "CLI client for nomad-lite scheduler")]
struct Args {
    /// Server address (use https:// for TLS)
    #[arg(long, default_value = "http://127.0.0.1:50051")]
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

    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Submit a new job
    Submit {
        /// The command to execute
        #[arg(short, long)]
        cmd: String,
    },
    /// Get status of a job
    Status {
        /// The job ID
        #[arg(short, long)]
        job_id: String,
    },
    /// List all jobs
    List {
        /// Use streaming API (more efficient for large job lists)
        #[arg(short, long)]
        stream: bool,
    },
    /// Get cluster status
    Cluster,
}

async fn create_channel(args: &Args) -> Result<Channel, Box<dyn std::error::Error>> {
    let endpoint = Channel::from_shared(args.addr.clone())?;

    // Check if TLS is configured
    let has_tls = args.ca_cert.is_some() || args.addr.starts_with("https://");

    if has_tls {
        let mut tls_config = ClientTlsConfig::new().domain_name("nomad-lite-cluster");

        // Load CA certificate if provided
        if let Some(ca_path) = &args.ca_cert {
            let ca_cert = tokio::fs::read(ca_path).await?;
            let ca_cert = Certificate::from_pem(ca_cert);
            tls_config = tls_config.ca_certificate(ca_cert);
        }

        // Load client certificate and key for mTLS if both provided
        if let (Some(cert_path), Some(key_path)) = (&args.cert, &args.key) {
            let cert = tokio::fs::read(cert_path).await?;
            let key = tokio::fs::read(key_path).await?;
            let identity = Identity::from_pem(cert, key);
            tls_config = tls_config.identity(identity);
        }

        Ok(endpoint.tls_config(tls_config)?.connect().await?)
    } else {
        Ok(endpoint.connect().await?)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let channel = create_channel(&args).await?;
    let mut client = SchedulerServiceClient::new(channel);

    match args.command {
        Commands::Submit { cmd } => {
            match client
                .submit_job(SubmitJobRequest {
                    command: cmd.clone(),
                })
                .await
            {
                Ok(response) => {
                    let resp = response.into_inner();
                    println!("Job submitted successfully!");
                    println!("Job ID: {}", resp.job_id);
                }
                Err(status) => {
                    println!("Job submission failed: {}", status.message());
                }
            }
        }
        Commands::Status { job_id } => {
            let response = client
                .get_job_status(GetJobStatusRequest { job_id })
                .await?
                .into_inner();

            println!("Job ID: {}", response.job_id);
            println!(
                "Status: {:?}",
                nomad_lite::proto::JobStatus::try_from(response.status)
                    .unwrap_or(nomad_lite::proto::JobStatus::Unspecified)
            );
            if !response.output.is_empty() {
                println!("Output: {}", response.output);
            }
            if !response.error.is_empty() {
                println!("Error: {}", response.error);
            }
            if response.assigned_worker > 0 {
                println!("Assigned Worker: {}", response.assigned_worker);
            }
        }
        Commands::List { stream } => {
            println!(
                "{:<40} {:<15} {:<10} {}",
                "JOB ID", "STATUS", "WORKER", "COMMAND"
            );
            println!("{}", "-".repeat(80));

            if stream {
                // Use streaming API for efficient memory usage
                let mut stream = client
                    .stream_jobs(StreamJobsRequest { status_filter: 0 })
                    .await?
                    .into_inner();

                let mut count = 0;
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(job) => {
                            count += 1;
                            let status = nomad_lite::proto::JobStatus::try_from(job.status)
                                .unwrap_or(nomad_lite::proto::JobStatus::Unspecified);
                            println!(
                                "{:<40} {:<15} {:<10} {}",
                                job.job_id,
                                format!("{:?}", status),
                                if job.assigned_worker > 0 {
                                    job.assigned_worker.to_string()
                                } else {
                                    "-".to_string()
                                },
                                job.command
                            );
                        }
                        Err(e) => {
                            eprintln!("Stream error: {}", e);
                            break;
                        }
                    }
                }
                println!();
                println!("Total jobs: {}", count);
            } else {
                // Use paginated API
                let response = client
                    .list_jobs(ListJobsRequest {
                        page_size: 100,
                        page_token: String::new(),
                    })
                    .await?
                    .into_inner();

                if response.jobs.is_empty() {
                    println!("No jobs found.");
                } else {
                    for job in response.jobs {
                        let status = nomad_lite::proto::JobStatus::try_from(job.status)
                            .unwrap_or(nomad_lite::proto::JobStatus::Unspecified);
                        println!(
                            "{:<40} {:<15} {:<10} {}",
                            job.job_id,
                            format!("{:?}", status),
                            if job.assigned_worker > 0 {
                                job.assigned_worker.to_string()
                            } else {
                                "-".to_string()
                            },
                            job.command
                        );
                    }
                    println!();
                    println!("Total jobs: {}", response.total_count);
                    if !response.next_page_token.is_empty() {
                        println!("(More results available)");
                    }
                }
            }
        }
        Commands::Cluster => {
            let response = client
                .get_cluster_status(GetClusterStatusRequest {})
                .await?
                .into_inner();

            println!("Cluster Status:");
            println!("  Leader: Node {}", response.leader_id);
            println!("  Term: {}", response.current_term);
            println!();
            println!("Nodes:");
            for node in response.nodes {
                println!(
                    "  Node {}: {} ({})",
                    node.node_id,
                    node.address,
                    if node.is_alive { "alive" } else { "dead" }
                );
            }
        }
    }

    Ok(())
}
