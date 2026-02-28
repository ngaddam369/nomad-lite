# nomad-lite

![CI](https://github.com/ngaddam369/nomad-lite/actions/workflows/ci.yml/badge.svg) [![codecov](https://codecov.io/gh/ngaddam369/nomad-lite/graph/badge.svg)](https://codecov.io/gh/ngaddam369/nomad-lite)

A distributed job scheduler with custom Raft consensus, similar to Nomad or Kubernetes scheduler. Jobs are shell commands executed in isolated Docker containers across a cluster.

## Requirements

| Dependency | Version | Installation |
|------------|---------|--------------|
| Rust | 1.56+ | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| protoc | 3.0+ | `apt install protobuf-compiler` / `brew install protobuf` |
| Docker | 20.0+ | `apt install docker.io` / `brew install --cask docker` |

## Quick Start

```bash
# Build and install
cargo install --path .

# Run single node with dashboard (in-memory state)
nomad-lite server --node-id 1 --port 50051 --dashboard-port 8080

# Run single node with persistent state (survives restarts)
nomad-lite server --node-id 1 --port 50051 --dashboard-port 8080 --data-dir /var/lib/nomad-lite/node1

# Open http://localhost:8080

# Submit a job (in another terminal)
nomad-lite job submit "echo hello"

# Submit a job using a specific Docker image (overrides the server default)
nomad-lite job submit --image python:3.12-alpine "python3 -c 'print(42)'"

# Check job status
nomad-lite job status <job-id>

# Cancel a pending or running job
nomad-lite job cancel <job-id>
```

## Documentation

**[Read the Documentation](https://ngaddam369.github.io/nomad-lite/)**

Or serve locally:

```bash
cargo install mdbook mdbook-mermaid
mdbook serve docs
# Open http://localhost:3000
```

