# Introduction

**nomad-lite** is a distributed job scheduler with custom Raft consensus, similar to Nomad or Kubernetes scheduler. Jobs are shell commands executed in isolated Docker containers across a cluster.

## Features

- **Custom Raft Consensus** - Leader election, log replication, and fault tolerance from scratch
- **Distributed Scheduling** - Jobs executed across cluster with automatic failover
- **Unified CLI** - Single binary for both server and client with automatic leader redirect
- **mTLS Security** - Mutual TLS for all gRPC communication
- **Docker Sandboxing** - Jobs run in isolated containers with restricted capabilities
- **Web Dashboard** - Real-time monitoring and job management
- **gRPC + REST APIs** - Type-safe client communication
- **Graceful Shutdown** - SIGTERM/SIGINT handling with drain period for in-flight work
- **Leader Draining & Transfer** - Voluntary leadership transfer and node draining for safe maintenance
- **Batch Replication** - Multiple job status updates batched into a single Raft log entry for reduced consensus overhead
- **Log Compaction** - Automatic in-memory log prefix truncation with snapshot transfer to slow followers
- **Proposal Backpressure** - Bounded proposal queue (256 slots); clients get an immediate `RESOURCE_EXHAUSTED` when the leader is overloaded and a `DEADLINE_EXCEEDED` if a commit stalls, rather than hanging indefinitely

## Requirements

| Dependency | Version | Installation |
|------------|---------|--------------|
| Rust | 1.56+ | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| protoc | 3.0+ | `apt install protobuf-compiler` / `brew install protobuf` |
| Docker | 20.0+ | `apt install docker.io` / `brew install --cask docker` |
