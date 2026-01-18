use clap::Parser;
use nomad_lite::proto::scheduler_service_client::SchedulerServiceClient;
use nomad_lite::proto::{
    GetClusterStatusRequest, GetJobStatusRequest, ListJobsRequest, SubmitJobRequest,
};

#[derive(Parser, Debug)]
#[command(name = "submit-job")]
#[command(about = "CLI client for nomad-lite scheduler")]
struct Args {
    /// Server address
    #[arg(long, default_value = "http://127.0.0.1:50051")]
    addr: String,

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
    List,
    /// Get cluster status
    Cluster,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut client = SchedulerServiceClient::connect(args.addr.clone()).await?;

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
        Commands::List => {
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
                println!("Total jobs: {}", response.total_count);
                println!();
                println!(
                    "{:<40} {:<15} {:<10} {}",
                    "JOB ID", "STATUS", "WORKER", "COMMAND"
                );
                println!("{}", "-".repeat(80));
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
                if !response.next_page_token.is_empty() {
                    println!();
                    println!("(More results available)");
                }
            }
        }
        Commands::Cluster => {
            let response = client
                .get_cluster_status(GetClusterStatusRequest {})
                .await?
                .into_inner();

            println!("Cluster Status:");
            println!("  Current Node: {}", response.node_id);
            println!("  Role: {}", response.role);
            println!("  Term: {}", response.current_term);
            println!(
                "  Leader: {}",
                response
                    .leader_id
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "unknown".to_string())
            );
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
