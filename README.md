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

# Run single node with dashboard
nomad-lite server --node-id 1 --port 50051 --dashboard-port 8080

# Open http://localhost:8080

# Submit a job (in another terminal)
nomad-lite job submit "echo hello"

# Check job status
nomad-lite job status <job-id>
```

## Documentation

**[Read the docs online](https://ngaddam369.github.io/nomad-lite/)** (deployed automatically via GitHub Pages)

Or serve locally:

```bash
cargo install mdbook mdbook-mermaid
mdbook serve docs
# Open http://localhost:3000
```

Topics covered:

- [Getting Started](docs/src/getting-started.md) - Running a cluster with Docker Compose or locally
- [CLI Reference](docs/src/cli-reference.md) - All commands, options, and examples
- [API Reference](docs/src/api-reference.md) - gRPC and REST API details
- [Architecture](docs/src/architecture.md) - Cluster overview, node internals, and data flow diagrams
- [Security](docs/src/security.md) - mTLS and Docker sandboxing
- [Raft Consensus](docs/src/raft.md) - Timing, replication, compaction, and safety guarantees
- [Testing](docs/src/testing.md) - Test suites and how to run them
