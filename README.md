# nomad-lite

![CI](https://github.com/ngaddam369/nomad-lite/actions/workflows/ci.yml/badge.svg) [![codecov](https://codecov.io/gh/ngaddam369/nomad-lite/graph/badge.svg)](https://codecov.io/gh/ngaddam369/nomad-lite)

A distributed job scheduler with custom Raft consensus, similar to Nomad or Kubernetes scheduler. Jobs are shell commands executed in isolated Docker containers across a cluster.

## Features

- **Custom Raft Consensus** - Leader election, log replication, and fault tolerance from scratch
- **Distributed Scheduling** - Jobs executed across cluster with automatic failover
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

## Quick Start

```bash
# Build
cargo build --release

# Run single node with dashboard
cargo run -- --node-id 1 --port 50051 --dashboard-port 8080

# Open http://localhost:8080
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        CLUSTER                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐          │
│  │   Node 1    │  │   Node 2    │  │   Node 3    │          │
│  │  (Leader)   │  │ (Follower)  │  │ (Follower)  │          │
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
│  │ └─────────┘ │  │ └─────────┘ │  │ └─────────┘ │          │
│  └─────────────┘  └─────────────┘  └─────────────┘          │
└─────────────────────────────────────────────────────────────┘
```

**Components:** Raft Module (consensus) → Scheduler (job assignment, leader only) → Worker (execution) → gRPC Server (client/inter-node) → Dashboard (web UI)

## Configuration

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

## Running a Cluster

### Local (3 nodes)

```bash
# Terminal 1
cargo run -- --node-id 1 --port 50051 --dashboard-port 8081 \
  --peers "2:127.0.0.1:50052,3:127.0.0.1:50053"

# Terminal 2
cargo run -- --node-id 2 --port 50052 --dashboard-port 8082 \
  --peers "1:127.0.0.1:50051,3:127.0.0.1:50053"

# Terminal 3
cargo run -- --node-id 3 --port 50053 --dashboard-port 8083 \
  --peers "1:127.0.0.1:50051,2:127.0.0.1:50052"
```

### Docker Compose

```bash
docker-compose up --build        # Foreground
docker-compose up --build -d     # Background
docker-compose down              # Stop
```

**Endpoints:** Node 1: `localhost:50051` (gRPC), `localhost:8081` (dashboard)

### With mTLS

```bash
# Generate certificates
./scripts/gen-test-certs.sh ./certs

# Start with TLS (add to each node)
--tls --ca-cert ./certs/ca.crt --cert ./certs/node1.crt --key ./certs/node1.key
```

## Usage Examples

### CLI Client

Base command (without TLS):
```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" <command>
```

With TLS:
```bash
cargo run --example submit_job -- --addr "https://127.0.0.1:50051" \
  --ca-cert ./certs/ca.crt --cert ./certs/client.crt --key ./certs/client.key <command>
```

**Get cluster status:**
```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" cluster
# Output:
# Cluster Status:
#   Leader: Node 1
#   Term: 5
#
# Nodes:
#   Node 1: 0.0.0.0:50051 (alive)
#   Node 2: 127.0.0.1:50052 (alive)
#   Node 3: 127.0.0.1:50053 (alive)
```

**Submit a job:**
```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" submit --cmd "echo hello"
# Output:
# Job submitted successfully!
# Job ID: ef319e40-c888-490d-8349-e9c05f78cf5a
```

**Get job status:**
```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" status --job-id ef319e40-c888-490d-8349-e9c05f78cf5a
# Output:
# Job ID: ef319e40-c888-490d-8349-e9c05f78cf5a
# Status: Completed
# Output: hello
# Assigned Worker: 1
```

**List all jobs:**
```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" list
# Output:
# JOB ID                                   STATUS          WORKER     COMMAND
# --------------------------------------------------------------------------------
# ef319e40-c888-490d-8349-e9c05f78cf5a     Completed       1          echo hello
# Total jobs: 1
```

**Stream jobs (memory-efficient):**
```bash
cargo run --example submit_job -- --addr "http://127.0.0.1:50051" list --stream
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
