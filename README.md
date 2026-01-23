# nomad-lite ![CI](https://github.com/ngaddam369/nomad-lite/actions/workflows/ci.yml/badge.svg) [![codecov](https://codecov.io/gh/ngaddam369/nomad-lite/graph/badge.svg)](https://codecov.io/gh/ngaddam369/nomad-lite)

This project implements a distributed job scheduler similar to Nomad, Kubernetes scheduler, or Apache Airflow.
The Jobs are simple shell commands like `echo hello`, `sleep 5` etc.

## Features

- **Custom Raft Consensus** - Leader election, log replication, and fault tolerance implemented from scratch
- **Distributed Job Scheduling** - Submit shell commands that get executed across the cluster
- **gRPC API** - Type-safe client communication using tonic
- **Web Dashboard** - Real-time cluster monitoring and job management
- **Automatic Failover** - New leader elected when current leader fails
- **Docker Sandboxing** - All jobs run in isolated Docker containers for security

## Requirements

| Dependency | Version | Required | Notes |
|------------|---------|----------|-------|
| Rust | 1.56+ | Yes | 2021 edition |
| protoc | 3.0+ | Yes | Protocol Buffers compiler for gRPC |
| Docker | 20.0+ | Yes | All jobs run in Docker containers |

### Installing Dependencies

**Rust:**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Protocol Buffers (protoc):**

```bash
# Ubuntu/Debian
sudo apt install protobuf-compiler

# macOS
brew install protobuf

# Arch Linux
sudo pacman -S protobuf
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      CLUSTER                                │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐          │
│  │   Node 1    │  │   Node 2    │  │   Node 3    │          │
│  │  (Leader)   │  │ (Follower)  │  │ (Follower)  │          │
│  │             │  │             │  │             │          │
│  │ ┌─────────┐ │  │ ┌─────────┐ │  │ ┌─────────┐ │          │
│  │ │Scheduler│ │  │ │Scheduler│ │  │ │Scheduler│ │          │
│  │ │ (active)│ │  │ │(standby)│ │  │ │(standby)│ │          │
│  │ └─────────┘ │  │ └─────────┘ │  │ └─────────┘ │          │
│  │ ┌─────────┐ │  │ ┌─────────┐ │  │ ┌─────────┐ │          │
│  │ │  Raft   │◄┼──┼─┤  Raft   │◄┼──┼─┤  Raft   │ │          │
│  │ │ Module  │─┼──┼►│ Module  │─┼──┼►│ Module  │ │          │
│  │ └─────────┘ │  │ └─────────┘ │  │ └─────────┘ │          │
│  │ ┌─────────┐ │  │ ┌─────────┐ │  │ ┌─────────┐ │          │
│  │ │ Worker  │ │  │ │ Worker  │ │  │ │ Worker  │ │          │
│  │ │Executor │ │  │ │Executor │ │  │ │Executor │ │          │
│  │ └─────────┘ │  │ └─────────┘ │  │ └─────────┘ │          │
│  └─────────────┘  └─────────────┘  └─────────────┘          │
└─────────────────────────────────────────────────────────────┘
```

### Components

- **Raft Module** - Handles leader election, log replication, and consensus
- **Scheduler** - Assigns jobs to available workers (only active on leader)
- **Worker Executor** - Executes shell commands and reports results
- **gRPC Server** - Handles client requests and inter-node communication
- **Dashboard** - Web UI for monitoring and job submission

### State Replication

All nodes maintain consistent state through Raft log replication:

| Operation | Leader | Follower |
|-----------|--------|----------|
| `SubmitJob` | Accepts and replicates | Rejects (returns leader hint) |
| `ListJobs` | Returns all jobs | Returns all jobs (read-only) |
| `GetJobStatus` | Returns job status | Returns job status (read-only) |
| `GetClusterStatus` | Returns authoritative status | Forwards to leader |

- **Write operations** (job submission) must go to the leader
- **Read operations** (list jobs, get status) can be served by any node
- **Cluster status** is always served by the leader (followers forward requests)
- Followers automatically replicate committed entries from the leader

## Quick Start

### Local Development

```bash
# Build the project
cargo build --release

# Start a single node
cargo run -- --node-id 1 --port 50051 --dashboard-port 8080

# Open dashboard at http://localhost:8080
```

### 3-Node Cluster (Local)

Terminal 1:

```bash
cargo run -- --node-id 1 --port 50051 --dashboard-port 8081 \
  --peers "2:127.0.0.1:50052,3:127.0.0.1:50053"
```

Terminal 2:

```bash
cargo run -- --node-id 2 --port 50052 --dashboard-port 8082 \
  --peers "1:127.0.0.1:50051,3:127.0.0.1:50053"
```

Terminal 3:

```bash
cargo run -- --node-id 3 --port 50053 --dashboard-port 8083 \
  --peers "1:127.0.0.1:50051,2:127.0.0.1:50052"
```

## CLI Client

```bash
# Check cluster status
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" cluster

# Submit a job
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" submit --cmd "echo hello"

# List all jobs
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" list

# List all jobs using streaming (memory-efficient for large lists)
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" list --stream

# Get job status
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" status --job-id <JOB_ID>
```

## API Reference

### gRPC Services

**SchedulerService** (Client API):

- `SubmitJob(command)` - Submit a new job
- `GetJobStatus(job_id)` - Get status of a job
- `ListJobs()` - List all jobs (paginated)
- `StreamJobs()` - Stream all jobs (memory-efficient for large lists)
- `GetClusterStatus()` - Get cluster information

**RaftService** (Internal):

- `RequestVote` - Election voting
- `AppendEntries` - Log replication / heartbeats

### REST API (Dashboard)

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/cluster` | GET | Cluster status |
| `/api/jobs` | GET | List all jobs |
| `/api/jobs` | POST | Submit a job |

## Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--node-id` | 1 | Unique node identifier |
| `--port` | 50051 | gRPC server port |
| `--dashboard-port` | - | Web dashboard port (optional) |
| `--peers` | "" | Peer addresses (format: "id:host:port,...") |
| `--image` | alpine:latest | Docker image for job execution |

## Sandboxing

All jobs run inside isolated Docker containers for security. This prevents:

- Filesystem access to host
- Network access to other services
- Privilege escalation attacks

### Security Features

Containers run with:

| Feature | Setting | Purpose |
|---------|---------|---------|
| Network | `--network=none` | No network access |
| Capabilities | `--cap-drop=ALL` | All Linux capabilities dropped |
| Filesystem | `--read-only` | Read-only root filesystem |
| Privileges | `--security-opt=no-new-privileges` | Prevent privilege escalation |
| Memory | `--memory=256m` | Memory limit |
| CPU | `--cpus=0.5` | CPU limit |

### Requirements

Docker must be installed and the user running nomad-lite must have permission to run containers:

```bash
# Ubuntu/Debian
sudo apt install docker.io
sudo usermod -aG docker $USER
# Log out and back in for group changes to take effect

# macOS
brew install --cask docker
```

### Custom Docker Image

Use a different image if you need specific tools:

```bash
cargo run -- --node-id 1 --port 50051 --dashboard-port 8080 --image ubuntu:22.04
```

### Submitting Jobs

```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" submit --cmd "echo hello"
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" submit --cmd "cat /etc/os-release"
```

**Note:** Available commands depend on the Docker image. The default `alpine:latest` includes basic utilities. Use `--image` to specify a different image if you need specific tools.

## Docker

### Running with Docker Compose

The easiest way to run a 3-node cluster is using Docker Compose.

**1. Build and start the cluster:**

```bash
# Foreground (see logs in terminal)
docker-compose up --build

# Or detached mode (background)
docker-compose up --build -d
```

**2. Verify all nodes are running:**

```bash
docker-compose ps
```

Expected output:

```
NAME          IMAGE              STATUS         PORTS
nomad-node1   nomad-lite-node1   Up (healthy)   0.0.0.0:50051->50051, 0.0.0.0:8081->8080
nomad-node2   nomad-lite-node2   Up             0.0.0.0:50052->50051, 0.0.0.0:8082->8080
nomad-node3   nomad-lite-node3   Up             0.0.0.0:50053->50051, 0.0.0.0:8083->8080
```

**3. Check cluster status:**

```bash
# Check each node (one will be "leader", others "follower")
curl http://localhost:8081/api/cluster
curl http://localhost:8082/api/cluster
curl http://localhost:8083/api/cluster
```

**4. Submit a job (to the leader):**

```bash
# First find which node is the leader, then submit to that node
curl -X POST http://localhost:8081/api/jobs \
  -H "Content-Type: application/json" \
  -d '{"command": "echo hello from docker"}'
```

**5. List jobs:**

```bash
curl http://localhost:8081/api/jobs
```

**6. View logs:**

```bash
# All nodes
docker-compose logs -f

# Specific node
docker-compose logs -f node1
```

**7. Stop the cluster:**

```bash
docker-compose down
```

### Endpoints

| Node | Dashboard | gRPC |
|------|-----------|------|
| Node 1 | <http://localhost:8081> | localhost:50051 |
| Node 2 | <http://localhost:8082> | localhost:50052 |
| Node 3 | <http://localhost:8083> | localhost:50053 |

## Raft Implementation Details

### Election Timeout

- Randomized between 150-300ms
- If no heartbeat received, node becomes candidate
- Candidate requests votes from all peers

### Heartbeats

- Leader sends every 50ms
- Contains log entries for replication
- Followers reset election timeout on receipt

### Log Replication

1. Client sends command to leader
2. Leader appends to local log
3. Leader replicates to followers via AppendEntries
4. Once majority acknowledge, entry is committed
5. Committed entries are applied to state machine

### Safety Guarantees

- Election safety: Only one leader per term
- Leader append-only: Leader never overwrites log
- Log matching: Logs with same index/term are identical
- Leader completeness: Committed entries persist through elections

### Cluster Sizing

The cluster supports **any number of nodes**, not just 3. The majority is calculated dynamically:

```rust
let total_nodes = peers.len() + 1;  // peers + self
let majority = (total_nodes / 2) + 1;
```

| Nodes | Majority Needed | Can Tolerate Failures |
|-------|-----------------|----------------------|
| 1     | 1               | 0                    |
| 3     | 2               | 1                    |
| 5     | 3               | 2                    |
| 7     | 4               | 3                    |
| 9     | 5               | 4                    |

**Why use odd numbers?** Raft works best with odd-numbered clusters:

- **3 nodes**: Tolerates 1 failure, needs 2 for majority
- **4 nodes**: Still only tolerates 1 failure (needs 3 for majority) — no benefit over 3
- **5 nodes**: Tolerates 2 failures, needs 3 for majority

Even numbers don't improve fault tolerance but add network overhead.

**Example: 5-Node Cluster**

```bash
# Node 1
cargo run -- --node-id 1 --port 50051 \
  --peers "2:127.0.0.1:50052,3:127.0.0.1:50053,4:127.0.0.1:50054,5:127.0.0.1:50055"

# Node 2
cargo run -- --node-id 2 --port 50052 \
  --peers "1:127.0.0.1:50051,3:127.0.0.1:50053,4:127.0.0.1:50054,5:127.0.0.1:50055"

# Nodes 3, 4, 5 follow the same pattern...
```

## TODO

### Critical (Security)

- [x] **Shell injection vulnerability** - All jobs run in isolated Docker containers with network disabled, dropped capabilities, and resource limits.
- [ ] **No authentication** - gRPC and REST endpoints have no auth. Add mTLS or API keys.
- [ ] **CORS allows all origins** - `src/dashboard/mod.rs` uses permissive CORS. Configure specific origins.
- [ ] **No input validation** - Commands accepted without validation. Add size limits and validation.
- [ ] **Binds to 0.0.0.0** - Exposes services on all interfaces by default.
- [ ] **No rate limiting** - No protection against job submission floods.

### High Priority (Testing)

- [x] **No integration tests** - Add tests for multi-node cluster operations.
- [x] **No failover tests** - Added 8 tests for leader failure, election, and quorum loss scenarios.
- [x] **No partition tests** - Added 7 tests for network partition and recovery scenarios.

### High Priority (Technical Debt)

- [ ] **No state persistence** - All state in memory. Use sled or rocksdb for durability.
- [ ] **No log compaction** - Log grows unbounded. Implement snapshots.

### Completed

- [x] **Job output replication** - Output stored locally on executing node, metadata (status, executed_by, exit_code) replicated via Raft. GetJobStatus forwards to executing node for output retrieval.
- [x] **Docker sandboxing** - All jobs run in isolated Docker containers (mandatory).
- [x] **Job streaming** - Added `StreamJobs` gRPC streaming endpoint for memory-efficient large job lists.
- [x] **Connection pooling** - Added client connection pool for request forwarding.
- [x] **Log cloned on heartbeat** - Now only clones entries needed for replication.
- [x] **Unbounded job queue** - Added configurable max capacity (10,000) and cleanup.
- [x] **Polling loops** - Scheduler now uses event-driven commit notifications.
- [x] **Incomplete cluster status** - Followers now forward to leader for authoritative status.
- [x] **Error handling** - Replaced unwraps with proper error handling.
- [x] **No pagination** - ListJobs now supports pagination (100 default, max 1000).
- [x] **Dashboard tests** - Added comprehensive REST API tests.
- [x] **Executor edge cases** - Added tests for empty output, large output, stderr.
- [x] **Module documentation** - Added doc comments to all modules.
- [x] **Function documentation** - Documented scheduler_loop(), run(), and public APIs.
- [x] **Raft invariants** - Documented safety invariants as comments.
