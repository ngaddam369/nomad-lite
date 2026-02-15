# Getting Started

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
