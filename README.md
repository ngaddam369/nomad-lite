# nomad-lite ![CI](https://github.com/ngaddam369/nomad-lite/actions/workflows/ci.yml/badge.svg)

This project implements a distributed job scheduler similar to Nomad, Kubernetes scheduler, or Apache Airflow.
The Jobs are simple shell commands like `echo hello`, `sleep 5` etc.

## Features

- **Custom Raft Consensus** - Leader election, log replication, and fault tolerance implemented from scratch
- **Distributed Job Scheduling** - Submit shell commands that get executed across the cluster
- **gRPC API** - Type-safe client communication using tonic
- **Web Dashboard** - Real-time cluster monitoring and job management
- **Automatic Failover** - New leader elected when current leader fails

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
| `GetClusterStatus` | Returns cluster info | Returns cluster info |

- **Write operations** (job submission) must go to the leader
- **Read operations** (list jobs, get status) can be served by any node
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

### Docker Compose

```bash
# Start a 3-node cluster
docker-compose up --build

# Dashboards:
# - http://localhost:8081 (Node 1)
# - http://localhost:8082 (Node 2)
# - http://localhost:8083 (Node 3)

# gRPC endpoints:
# - localhost:50051 (Node 1)
# - localhost:50052 (Node 2)
# - localhost:50053 (Node 3)
```

## CLI Client

```bash
# Check cluster status
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" cluster

# Submit a job
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" submit --cmd "echo hello"

# List all jobs
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" list

# Get job status
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" status --job-id <JOB_ID>
```

## API Reference

### gRPC Services

**SchedulerService** (Client API):

- `SubmitJob(command)` - Submit a new job
- `GetJobStatus(job_id)` - Get status of a job
- `ListJobs()` - List all jobs
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

## Project Structure

```
nomad-lite/
├── proto/scheduler.proto       # gRPC definitions
├── src/
│   ├── main.rs                 # Entry point
│   ├── lib.rs                  # Library exports
│   ├── config.rs               # Configuration
│   ├── error.rs                # Error types
│   ├── node.rs                 # Main orchestration
│   ├── raft/
│   │   ├── state.rs            # Raft state & log
│   │   ├── node.rs             # Raft logic
│   │   ├── rpc.rs              # RPC handlers
│   │   └── timer.rs            # Election timeouts
│   ├── scheduler/
│   │   ├── job.rs              # Job model
│   │   ├── queue.rs            # Job queue
│   │   └── assigner.rs         # Job assignment
│   ├── worker/
│   │   ├── executor.rs         # Command execution
│   │   └── heartbeat.rs        # Worker heartbeats
│   ├── grpc/
│   │   ├── server.rs           # gRPC server setup
│   │   ├── client_service.rs   # SchedulerService impl
│   │   └── cluster_service.rs  # RaftService impl
│   └── dashboard/              # Web UI
├── examples/submit_job.rs      # CLI client
├── Dockerfile
└── docker-compose.yml
```

## Technologies

- **Rust** - Systems programming language
- **Tokio** - Async runtime
- **Tonic** - gRPC framework
- **Axum** - Web framework (dashboard)
- **Prost** - Protocol Buffers

## TODO

### Critical (Security)

- [ ] **Shell injection vulnerability** - `src/worker/executor.rs:26-35` runs arbitrary commands without sanitization. Implement command allowlist or sandboxing.
- [ ] **No authentication** - gRPC (`src/grpc/server.rs`) and REST (`src/dashboard/mod.rs`) endpoints have no auth. Add mTLS or API keys.
- [ ] **CORS allows all origins** - `src/dashboard/mod.rs:60` uses permissive CORS. Configure specific origins.
- [ ] **No input validation** - `src/grpc/client_service.rs:36` accepts commands without validation. Add size limits and validation.
- [ ] **Binds to 0.0.0.0** - `src/main.rs:64` exposes services on all interfaces by default.
- [ ] **No rate limiting** - No protection against job submission floods.

### High Priority (Error Handling)

- [x] **Unwrap in config** - `src/config.rs:23` uses `parse().unwrap()` in Default impl.
- [x] **Unwrap in main** - `src/main.rs:67` uses `parse().unwrap()` on dashboard_addr.
- [x] **Unwrap in dashboard** - `src/dashboard/mod.rs:74-75` uses `unwrap()` on server bind.
- [x] **Silent gRPC failures** - `src/node.rs:88-92` logs errors but node silently stops.
- [x] **Ignored send errors** - `src/raft/node.rs:222-228` silently ignores channel send failures.

### High Priority (Testing)

- [ ] **No integration tests** - Add tests for multi-node cluster operations.
- [ ] **No failover tests** - Add tests for leader failure and election.
- [ ] **No partition tests** - Add tests for network partition recovery.
- [ ] **No dashboard tests** - `src/dashboard/mod.rs` has no REST API tests.
- [x] **No executor edge cases** - `src/worker/executor.rs` needs tests for empty output, large output, timeout.

### High Priority (Technical Debt)

- [ ] **No state persistence** - `src/raft/state.rs:50-51` keeps all state in memory. Use sled or rocksdb.
- [ ] **No log compaction** - Log grows unbounded. Implement snapshots.
- [ ] **Job output not replicated** - `src/node.rs:218` only leader has job output.

### Medium Priority (Performance)

- [ ] **Log cloned on heartbeat** - `src/raft/node.rs:220-221` clones full log every heartbeat (O(log_size)).
- [ ] **Unbounded job queue** - `src/scheduler/queue.rs` has no size limits or cleanup policy.
- [ ] **Polling loops** - `src/node.rs:141-143` uses polling instead of event-driven channels.
- [ ] **No connection pooling** - `src/raft/node.rs:162` creates new client per request.
- [ ] **ListJobs allocates Vec** - `src/grpc/client_service.rs:133-142` should use streaming.

### Medium Priority (API Design)

- [x] **Error in response fields** - `src/grpc/client_service.rs:41-50` uses response fields for errors instead of gRPC status codes.
- [x] **No pagination** - `ListJobs` returns all jobs without pagination.
- [x] **Incomplete cluster status** - `GetClusterStatusResponse` only returns current node info.

### Medium Priority (Documentation)

- [x] **Missing module docs** - Add doc comments to `src/raft/mod.rs`, `src/scheduler/mod.rs`, `src/worker/mod.rs`.
- [x] **Missing function docs** - Document `scheduler_loop()`, `run()`, and public APIs.
- [x] **No invariants documented** - Document Raft safety invariants as comments.
