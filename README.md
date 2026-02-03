# nomad-lite

![CI](https://github.com/ngaddam369/nomad-lite/actions/workflows/ci.yml/badge.svg) [![codecov](https://codecov.io/gh/ngaddam369/nomad-lite/graph/badge.svg)](https://codecov.io/gh/ngaddam369/nomad-lite)

A distributed job scheduler with custom Raft consensus, similar to Nomad or Kubernetes scheduler. Jobs are shell commands executed in isolated Docker containers across a cluster.

## Features

- **Custom Raft Consensus** - Leader election, log replication, and fault tolerance from scratch
- **Distributed Scheduling** - Jobs executed across cluster with automatic failover
- **Unified CLI** - Single binary for both server and client with automatic leader redirect
- **mTLS Security** - Mutual TLS for all gRPC communication
- **Docker Sandboxing** - Jobs run in isolated containers with restricted capabilities
- **Web Dashboard** - Real-time monitoring and job management
- **gRPC + REST APIs** - Type-safe client communication

## Requirements

| Dependency | Version | Installation |
|------------|---------|--------------|
| Rust | 1.56+ | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| protoc | 3.0+ | `apt install protobuf-compiler` / `brew install protobuf` |
| Docker | 20.0+ | `apt install docker.io` / `brew install --cask docker` |

## Architecture

```mermaid
graph TB
    subgraph "Client Layer"
        CLI[CLI Client<br/>nomad-lite job/cluster]
        REST[REST API Client<br/>HTTP requests]
        GRPC_CLIENT[gRPC Client<br/>Direct gRPC calls]
    end

    subgraph "Cluster - 3 Nodes with Raft Consensus"
        subgraph "Node 1 - Leader"
            direction TB
            N1_GRPC[gRPC Server<br/>:50051<br/>mTLS enabled]
            N1_DASH[Dashboard<br/>:8081<br/>Web UI + REST]
            N1_RAFT[Raft Module<br/>Leader]
            N1_SCHED[Scheduler<br/>ACTIVE]
            N1_WORKER[Worker<br/>Job Executor]
            N1_STATE[State Machine<br/>Job Queue]
            N1_LOG[Raft Log<br/>In-Memory]
            
            N1_GRPC --> N1_RAFT
            N1_DASH --> N1_GRPC
            N1_RAFT --> N1_LOG
            N1_RAFT --> N1_STATE
            N1_STATE --> N1_SCHED
            N1_SCHED --> N1_WORKER
        end

        subgraph "Node 2 - Follower"
            direction TB
            N2_GRPC[gRPC Server<br/>:50052<br/>mTLS enabled]
            N2_DASH[Dashboard<br/>:8082<br/>Web UI + REST]
            N2_RAFT[Raft Module<br/>Follower]
            N2_SCHED[Scheduler<br/>STANDBY]
            N2_WORKER[Worker<br/>Job Executor]
            N2_STATE[State Machine<br/>Job Queue]
            N2_LOG[Raft Log<br/>In-Memory]
            
            N2_GRPC --> N2_RAFT
            N2_DASH --> N2_GRPC
            N2_RAFT --> N2_LOG
            N2_RAFT --> N2_STATE
            N2_STATE --> N2_SCHED
            N2_SCHED --> N2_WORKER
        end

        subgraph "Node 3 - Follower"
            direction TB
            N3_GRPC[gRPC Server<br/>:50053<br/>mTLS enabled]
            N3_DASH[Dashboard<br/>:8083<br/>Web UI + REST]
            N3_RAFT[Raft Module<br/>Follower]
            N3_SCHED[Scheduler<br/>STANDBY]
            N3_WORKER[Worker<br/>Job Executor]
            N3_STATE[State Machine<br/>Job Queue]
            N3_LOG[Raft Log<br/>In-Memory]
            
            N3_GRPC --> N3_RAFT
            N3_DASH --> N3_GRPC
            N3_RAFT --> N3_LOG
            N3_RAFT --> N3_STATE
            N3_STATE --> N3_SCHED
            N3_SCHED --> N3_WORKER
        end
    end

    subgraph "Docker Environment"
        D1[Docker Container<br/>alpine:latest<br/>--network=none<br/>--read-only]
        D2[Docker Container<br/>alpine:latest<br/>--network=none<br/>--read-only]
        D3[Docker Container<br/>alpine:latest<br/>--network=none<br/>--read-only]
    end

    %% Client to Cluster connections
    CLI -.->|1. Submit Job| N1_GRPC
    CLI -.->|Auto-redirect if follower| N2_GRPC
    REST -.->|HTTP POST /api/jobs| N1_DASH
    GRPC_CLIENT -.->|SubmitJob RPC| N1_GRPC

    %% Raft Consensus - Heartbeats (50ms interval)
    N1_RAFT <-.->|AppendEntries<br/>Heartbeat 50ms| N2_RAFT
    N1_RAFT <-.->|AppendEntries<br/>Heartbeat 50ms| N3_RAFT
    N2_RAFT <-.->|RequestVote<br/>Election timeout 150-300ms| N3_RAFT

    %% Worker to Docker connections
    N1_WORKER -->|Execute Job| D1
    N2_WORKER -->|Execute Job| D2
    N3_WORKER -->|Execute Job| D3

    %% Styling
    classDef leader fill:#90EE90,stroke:#006400,stroke-width:3px
    classDef follower fill:#FFE4B5,stroke:#FF8C00,stroke-width:2px
    classDef client fill:#87CEEB,stroke:#4682B4,stroke-width:2px
    classDef docker fill:#E6E6FA,stroke:#9370DB,stroke-width:2px

    class N1_GRPC,N1_DASH,N1_RAFT,N1_SCHED,N1_WORKER,N1_STATE,N1_LOG leader
    class N2_GRPC,N2_DASH,N2_RAFT,N2_SCHED,N2_WORKER,N2_STATE,N2_LOG follower
    class N3_GRPC,N3_DASH,N3_RAFT,N3_SCHED,N3_WORKER,N3_STATE,N3_LOG follower
    class CLI,REST,GRPC_CLIENT client
    class D1,D2,D3 docker
```

**Components:** Raft Module (consensus) → Scheduler (job assignment, leader only) → Worker (execution) → gRPC Server (client/inter-node) → Dashboard (web UI)

## Data Flow

```mermaid
sequenceDiagram
    participant Client as CLI Client
    participant N1 as Node 1 (Leader)<br/>gRPC Server
    participant Raft1 as Node 1<br/>Raft Module
    participant N2 as Node 2 (Follower)<br/>Raft Module
    participant N3 as Node 3 (Follower)<br/>Raft Module
    participant Log1 as Node 1<br/>Raft Log
    participant State1 as Node 1<br/>State Machine
    participant Sched as Scheduler<br/>(Leader Only)
    participant Worker as Worker 2<br/>(Selected Node)
    participant Docker as Docker Container

    Note over Client,Docker: Job Submission Flow (Write Operation)

    Client->>N1: 1. SubmitJob("echo hello")
    N1->>Raft1: 2. Propose command
    Raft1->>Log1: 3. Append to local log<br/>(index=N, term=T)
    
    par Replicate to Followers
        Raft1->>N2: 4a. AppendEntries RPC<br/>(log entry)
        Raft1->>N3: 4b. AppendEntries RPC<br/>(log entry)
    end

    N2-->>Raft1: 5a. ACK (success)
    N3-->>Raft1: 5b. ACK (success)

    Note over Raft1: Majority reached (2/3)

    Raft1->>State1: 6. Commit log entry<br/>Apply to state machine
    State1->>State1: 7. Create Job object<br/>Status: PENDING
    State1->>Sched: 8. Notify new job
    
    Sched->>Sched: 9. Select worker node<br/>Round-robin: Node 2
    Sched->>State1: 10. Update Job<br/>assigned_worker=2
    
    Note over Raft1,N3: Leader replicates assignment<br/>via AppendEntries
    
    Sched->>Worker: 11. Execute job via gRPC<br/>ExecuteJob(job_id, "echo hello")
    
    Worker->>Docker: 12. docker run alpine:latest<br/>--network=none --read-only<br/>--memory=256m --cpus=0.5<br/>echo hello
    
    Docker-->>Worker: 13. Output: "hello\n"<br/>Exit code: 0
    
    Worker->>State1: 14. Update job status<br/>Status: COMPLETED<br/>Output: "hello"
    
    State1->>Raft1: 15. Replicate completion
    Raft1->>N2: AppendEntries (job completion)
    Raft1->>N3: AppendEntries (job completion)
    
    N1-->>Client: 16. Response: Job submitted<br/>job_id: uuid

    Note over Client,Docker: Job Status Query Flow (Read Operation)

    Client->>N1: 17. GetJobStatus(job_id)
    N1->>State1: 18. Query state machine
    State1-->>N1: 19. Job details
    N1-->>Client: 20. Response: COMPLETED<br/>Output: "hello"

    Note over Client,Docker: Leader Election Flow (Failure Scenario)

    Note over Raft1: Leader crashes! ❌
    
    Note over N2,N3: Heartbeat timeout (150-300ms)
    
    N2->>N2: Election timeout triggered
    N2->>N2: Increment term, become candidate
    N2->>N3: RequestVote RPC (term=T+1)
    N3-->>N2: VoteGranted
    
    Note over N2: Won election with majority
    
    N2->>N2: Become Leader
    N2->>Sched: Activate Scheduler
    
    Note over N2,N3: Node 2 now leads cluster
```

## Quick Start

```bash
# Build and install
cargo install --path .

# Run single node with dashboard
nomad-lite server --node-id 1 --port 50051 --dashboard-port 8080

# Open http://localhost:8080

# Submit a job (in another terminal)
nomad-lite job submit "echo hello"

# Check job status
nomad-lite job status <job-id>

# View cluster status
nomad-lite cluster status
```

## Running a Cluster

### Option 1: Docker Compose (Recommended)

The easiest way to run a 3-node cluster. Choose between production (mTLS) or development (no TLS) setup.

#### Production Setup (mTLS)

```bash
# Generate certificates (one-time setup)
./scripts/gen-test-certs.sh ./certs

# Start cluster
docker-compose up --build

# Stop cluster
docker-compose down
```

**Using the CLI with mTLS:**

```bash
# Check cluster status
nomad-lite cluster \
  --addr "https://127.0.0.1:50051" \
  --ca-cert ./certs/ca.crt \
  --cert ./certs/client.crt \
  --key ./certs/client.key \
  status

# Find the leader node from cluster status output, then submit to it
# Example: if Node 3 is leader, use port 50053
nomad-lite job \
  --addr "https://127.0.0.1:50053" \
  --ca-cert ./certs/ca.crt \
  --cert ./certs/client.crt \
  --key ./certs/client.key \
  submit "echo hello from mTLS"

# Get job status
nomad-lite job \
  --addr "https://127.0.0.1:50051" \
  --ca-cert ./certs/ca.crt \
  --cert ./certs/client.crt \
  --key ./certs/client.key \
  status <job-id>

# List all jobs
nomad-lite job \
  --addr "https://127.0.0.1:50051" \
  --ca-cert ./certs/ca.crt \
  --cert ./certs/client.crt \
  --key ./certs/client.key \
  list
```

#### Development Setup (No TLS)

```bash
# Start cluster
docker-compose -f docker-compose.dev.yml up --build

# Stop cluster
docker-compose -f docker-compose.dev.yml down
```

**Using the CLI without TLS:**

```bash
# Check cluster status
nomad-lite cluster --addr "http://127.0.0.1:50051" status

# Find the leader node from cluster status output, then submit to it
# Example: if Node 1 is leader, use port 50051
nomad-lite job --addr "http://127.0.0.1:50051" submit "echo hello"

# Get job status
nomad-lite job --addr "http://127.0.0.1:50051" status <job-id>

# List all jobs
nomad-lite job --addr "http://127.0.0.1:50051" list
```

**Cluster Endpoints:**

| Node | gRPC | Dashboard |
|------|------|-----------|
| 1 | `localhost:50051` | `localhost:8081` |
| 2 | `localhost:50052` | `localhost:8082` |
| 3 | `localhost:50053` | `localhost:8083` |

> **Note:** Auto-redirect doesn't work with Docker Compose because nodes report internal addresses (e.g., `node1:50051`) that aren't accessible from the host. Always check which node is leader using `cluster status` and connect directly to it for job submissions.

### Option 2: Local Multi-Node

Run nodes directly without Docker:

```bash
# Terminal 1
nomad-lite server --node-id 1 --port 50051 --dashboard-port 8081 \
  --peers "2:127.0.0.1:50052,3:127.0.0.1:50053"

# Terminal 2
nomad-lite server --node-id 2 --port 50052 --dashboard-port 8082 \
  --peers "1:127.0.0.1:50051,3:127.0.0.1:50053"

# Terminal 3
nomad-lite server --node-id 3 --port 50053 --dashboard-port 8083 \
  --peers "1:127.0.0.1:50051,2:127.0.0.1:50052"
```

**With mTLS:**

```bash
# Generate certificates first
./scripts/gen-test-certs.sh ./certs

# Add TLS flags to each node
--tls --ca-cert ./certs/ca.crt --cert ./certs/node1.crt --key ./certs/node1.key
```

## CLI Reference

The `nomad-lite` binary provides both server and client functionality.

```
nomad-lite
├── server                        # Start a server node
├── job                           # Job management
│   ├── submit <COMMAND>         # Submit a new job
│   ├── status <JOB_ID>          # Get job status
│   └── list                     # List all jobs
└── cluster                       # Cluster management
    └── status                   # Get cluster info
```

### Server Options

| Flag | Default | Description |
|------|---------|-------------|
| `--node-id` | 1 | Unique node identifier |
| `--port` | 50051 | gRPC server port |
| `--dashboard-port` | - | Web dashboard port (optional) |
| `--peers` | "" | Peer addresses: `"id:host:port,..."` |
| `--image` | alpine:latest | Docker image for jobs |
| `--tls` | false | Enable mTLS |
| `--ca-cert` | - | CA certificate path |
| `--cert` | - | Node certificate path |
| `--key` | - | Node private key path |
| `--allow-insecure` | false | Run without TLS if certs fail |

### Client Options

| Flag | Default | Description |
|------|---------|-------------|
| `-a, --addr` | `http://127.0.0.1:50051` | Server address |
| `-o, --output` | `table` | Output format: `table` or `json` |
| `--ca-cert` | - | CA certificate for TLS |
| `--cert` | - | Client certificate for mTLS |
| `--key` | - | Client private key for mTLS |

### Command Examples

**Submit a job:**

```bash
nomad-lite job submit "echo hello"
# Job submitted successfully!
# Job ID: ef319e40-c888-490d-8349-e9c05f78cf5a
```

**Get job status:**

```bash
nomad-lite job status ef319e40-c888-490d-8349-e9c05f78cf5a
# Job ID:          ef319e40-c888-490d-8349-e9c05f78cf5a
# Status:          COMPLETED
# Exit Code:       0
# Assigned Worker: 1
# Executed By:     1
# Output:
#   hello
```

**List all jobs:**

```bash
nomad-lite job list
# JOB ID                                 STATUS       WORKER   COMMAND
# ------------------------------------------------------------------------------
# ef319e40-c888-490d-8349-e9c05f78cf5a   COMPLETED    1        echo hello
#
# Showing 1 of 1 jobs
```

**List jobs with pagination:**

```bash
nomad-lite job list --page-size 50 --all  # Fetch all pages
nomad-lite job list --stream              # Use streaming API
```

**Get cluster status:**

```bash
nomad-lite cluster status
# Cluster Status
# ========================================
# Term:   5
# Leader: Node 1
#
# Nodes:
# ID       ADDRESS                   STATUS
# ---------------------------------------------
# 1        0.0.0.0:50051             [+] alive
# 2        127.0.0.1:50052           [+] alive
# 3        127.0.0.1:50053           [+] alive
```

**JSON output:**

```bash
nomad-lite job -o json list
nomad-lite cluster -o json status
```

### Automatic Leader Redirect

For local clusters (non-Docker), the CLI automatically redirects to the leader if you connect to a follower:

```bash
# Connect to follower node (port 50052), CLI auto-redirects to leader
nomad-lite job -a http://127.0.0.1:50052 submit "echo hello"
# Redirecting to leader at 127.0.0.1:50051...
# Job submitted successfully!
```

### REST API (Dashboard)

**Get cluster status:**

```bash
curl http://localhost:8081/api/cluster
# Response:
# {
#   "node_id": 1,
#   "role": "leader",
#   "current_term": 5,
#   "leader_id": 1,
#   "commit_index": 3,
#   "last_applied": 3,
#   "log_length": 3
# }
```

**Submit a job:**

```bash
curl -X POST http://localhost:8081/api/jobs \
  -H "Content-Type: application/json" \
  -d '{"command": "echo hello"}'
# Response:
# {
#   "job_id": "ef319e40-c888-490d-8349-e9c05f78cf5a",
#   "status": "pending"
# }
```

**List all jobs:**

```bash
curl http://localhost:8081/api/jobs
# Response:
# [
#   {
#     "id": "ef319e40-c888-490d-8349-e9c05f78cf5a",
#     "command": "echo hello",
#     "status": "completed",
#     "executed_by": 1,
#     "output": "hello\n",
#     "error": null,
#     "created_at": "2026-01-28T12:45:41.231558433+00:00",
#     "completed_at": "2026-01-28T12:45:41.678341558+00:00"
#   }
# ]
```

### gRPC API

| Method | Description | Leader Only |
|--------|-------------|-------------|
| `SubmitJob(command)` | Submit a job | Yes |
| `GetJobStatus(job_id)` | Get job status | No |
| `ListJobs()` | List jobs (paginated) | No |
| `StreamJobs()` | Stream jobs | No |
| `GetClusterStatus()` | Cluster info | Forwarded to leader |

## Security

### mTLS

All gRPC communication (node-to-node, client-to-node) can be secured with mutual TLS:

- Both parties authenticate via certificates signed by cluster CA
- All traffic encrypted with TLS 1.2+
- Generate certs: `./scripts/gen-test-certs.sh ./certs`

### Docker Sandboxing

Jobs run in isolated containers with:

| Restriction | Setting |
|-------------|---------|
| Network | `--network=none` |
| Capabilities | `--cap-drop=ALL` |
| Filesystem | `--read-only` |
| Privileges | `--security-opt=no-new-privileges` |
| Memory | `--memory=256m` |
| CPU | `--cpus=0.5` |

## Raft Implementation

### Timing

- **Election timeout:** 150-300ms (randomized)
- **Heartbeat interval:** 50ms

### Log Replication

1. Client sends command to leader
2. Leader appends to log and replicates via `AppendEntries`
3. Majority acknowledgment → committed
4. Applied to state machine

### Safety Guarantees

- Election safety: One leader per term
- Leader append-only: Never overwrites log
- Log matching: Same index/term = identical
- Leader completeness: Committed entries persist

### Cluster Sizing

| Nodes | Majority | Fault Tolerance |
|-------|----------|-----------------|
| 3 | 2 | 1 failure |
| 5 | 3 | 2 failures |
| 7 | 4 | 3 failures |

Use odd numbers—even numbers add overhead without improving fault tolerance.

## TODO

### Security

- [x] ~~No authentication~~ - mTLS implemented
- [ ] CORS allows all origins
- [ ] No input validation
- [ ] Binds to 0.0.0.0
- [ ] No rate limiting

### Technical Debt

- [ ] No state persistence (in-memory only)
- [ ] No log compaction (unbounded growth)
