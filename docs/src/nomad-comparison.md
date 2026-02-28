# Nomad vs nomad-lite

nomad-lite draws direct inspiration from [HashiCorp Nomad](https://www.nomadproject.io/),
a production-grade workload orchestrator. This page documents where the two systems
overlap and where they diverge.

nomad-lite is **not** a drop-in replacement for Nomad. It is a purpose-built implementation
of the core distributed scheduling concepts — Raft consensus, distributed job assignment,
worker liveness, graceful failover — written from scratch in Rust as a learning and
experimentation platform. Understanding where Nomad sets the bar makes it easier to reason
about which parts of that bar nomad-lite has already cleared and which remain ahead.

---

## Scheduler Types

Nomad ships three built-in schedulers, each with a fundamentally different execution model:

| Scheduler | Nomad | nomad-lite |
|---|---|---|
| **batch** | Run-to-completion jobs; retries on failure | ✅ Core model |
| **service** | Long-running daemons; keeps N instances alive, restarts on crash | Not supported |
| **system** | Runs exactly one instance on every node (log shippers, exporters) | Not supported |

nomad-lite is a **pure batch scheduler**. Every job is expected to start, do work, and
exit. Long-running services and system-wide daemons are outside its current scope.

---

## Task Drivers (How Jobs Run)

Nomad abstracts the execution runtime through pluggable task drivers:

| Driver | Nomad | nomad-lite |
|---|---|---|
| `docker` | Full image + entrypoint + env control | Partial — fixed Alpine image, `sh -c` only |
| `exec` / `raw_exec` | Direct process execution, no container | Not supported |
| `java` | JVM workloads with classpath management | Not supported |
| `qemu` | Full virtual machine execution | Not supported |
| `podman`, `containerd` | OCI-compatible runtimes via plugins | Not supported |

nomad-lite runs all jobs as `docker run --rm alpine:latest sh -c <command>`. The image is
configurable at the node level but uniform across all jobs — submitters cannot choose a
per-job image or entrypoint.

---

## Scheduling Features

| Feature | Nomad | nomad-lite |
|---|---|---|
| **Placement algorithm** | Bin-packing (CPU + memory aware) | Least-loaded (running job count) |
| **Resource constraints** | CPU, memory, GPU, disk declared per job | Not supported |
| **Node constraints** | Attribute expressions (`kernel.name == linux`) | Not supported |
| **Affinities** | Soft preferences for placement | Not supported |
| **Spread** | Distribute across failure domains (AZ, rack) | Not supported |
| **Job priorities** | Integer 1–100; high priority preempts low | Not supported |
| **Preemption** | Evict lower-priority running jobs to place high-priority ones | Not supported |

nomad-lite's assigner selects the worker with the fewest running jobs. This is a
reasonable approximation of load balancing but ignores actual resource consumption —
a node running one heavy job looks the same as one running one trivial job.

---

## Job Lifecycle

| Feature | Nomad | nomad-lite |
|---|---|---|
| **Job submission** | HCL job spec file | Single string command (gRPC / CLI) |
| **Job timeouts** | `kill_timeout`, `max_kill_timeout` per task | Node-level 30 s wall-clock timeout (kills container + marks Failed); per-job configurable timeout not supported |
| **Retry policy** | `restart` stanza: attempts, delay, mode | Not supported |
| **Reschedule on node loss** | `reschedule` stanza with backoff | Not supported |
| **Job cancellation** | `nomad job stop` | ✅ `nomad-lite job cancel <job-id>` |
| **Job priorities** | 1–100 integer field | Not supported |
| **Parameterized jobs** | `parameterized` stanza; dispatch via CLI/API | Not supported |
| **Periodic jobs** | Cron expression in job spec | Not supported |
| **Job dependencies (DAG)** | Not native; requires external tooling | Not supported |
| **Rolling updates** | `update` stanza with canary, max_parallel | Not supported |
| **Job versioning** | Tracks job spec history, auto-reverts on failure | Not supported |

---

## Consensus and State

| Feature | Nomad | nomad-lite |
|---|---|---|
| **Consensus protocol** | Raft (via HashiCorp Raft library) | Custom Raft implementation in Rust |
| **State persistence** | Durable BoltDB-backed Raft log | ✅ Optional RocksDB-backed log + snapshot via `--data-dir`; in-memory if omitted |
| **Log compaction** | Raft snapshots to BoltDB | In-memory prefix truncation + snapshot; persisted to RocksDB when `--data-dir` is set |
| **Multi-region** | Federation across regions with replication | Single cluster only |
| **Leader election** | Randomized timeouts | Randomized timeouts (150–300 ms) |
| **Leadership transfer** | `nomad operator raft transfer-leadership` | ✅ `nomad-lite cluster transfer-leader` |
| **Node drain** | `nomad node drain` | ✅ `nomad-lite cluster drain` |

nomad-lite now supports optional **state persistence** via `--data-dir`. When enabled, every
Raft log entry, term change, voted-for value, and snapshot is written to a local RocksDB store
before being acknowledged. A crashed node can rejoin, replay its log, and resume without losing
any committed state. Without `--data-dir` the node runs in-memory only.

---

## Security

| Feature | Nomad | nomad-lite |
|---|---|---|
| **Transport encryption** | mTLS for all RPC | ✅ mTLS implemented |
| **ACL system** | Token-based with policies and roles | Not supported |
| **Vault integration** | Secrets injection at job start | Not supported |
| **Namespaces** | Multi-tenant isolation | Not supported |
| **Sentinel policies** | Fine-grained job submission governance | Not supported |

nomad-lite implements the transport layer (mTLS) but has no authorization model. Any
client that presents a valid certificate can submit jobs, drain nodes, or transfer
leadership. In a controlled environment this is acceptable; in a shared environment it is
not.

---

## Observability

| Feature | Nomad | nomad-lite |
|---|---|---|
| **Metrics** | Prometheus endpoint (`/v1/metrics`) | Not supported |
| **Distributed tracing** | OpenTelemetry support | Not supported |
| **Health endpoints** | `/v1/agent/health` (live + ready) | ✅ `/health/live` + `/health/ready` |
| **Audit logging** | Immutable audit log (Enterprise) | Not supported |
| **Web UI** | Full job and cluster management UI | ✅ Basic dashboard (status + job list) |
| **Log streaming** | `nomad alloc logs -f` | Not supported |

---

## Client API

| Feature | Nomad | nomad-lite |
|---|---|---|
| **Protocol** | HTTP/JSON REST + gRPC | gRPC (primary) + HTTP (dashboard only) |
| **Job submission** | HCL / JSON job spec | `string command` field |
| **Streaming** | Event stream, log tailing | ✅ `StreamJobs` gRPC streaming |
| **Pagination** | Cursor-based | ✅ Cursor-based `ListJobs` |
| **Batch submission** | Multiple allocations per job | Not supported |
| **Leader redirect** | `X-Nomad-Index` + redirect hints | ✅ CLI auto-redirects to leader |

---

## Feature Summary

```
                    ┌─────────────────────────────────────────┐
                    │         nomad-lite feature coverage     │
                    └─────────────────────────────────────────┘

 Consensus layer
   ✅  Leader election (randomized timeouts)
   ✅  Log replication (AppendEntries)
   ✅  Log compaction + snapshots
   ✅  Leadership transfer
   ✅  InstallSnapshot for lagging followers
   ✅  Dedicated heartbeat channel (no HOL blocking)
   ✅  Proposal backpressure (bounded queue, immediate RESOURCE_EXHAUSTED)

 Cluster operations
   ✅  Node drain (stop accepting, finish in-flight, transfer leadership)
   ✅  Graceful shutdown (SIGTERM/SIGINT with 30 s drain window)
   ✅  mTLS for all cluster and client communication
   ✅  Peer liveness tracking

 Job scheduling
   ✅  Distributed job assignment via Raft (all nodes agree on assignments)
   ✅  Least-loaded worker selection
   ✅  Worker heartbeat liveness (5 s window)
   ✅  Batch status update replication
   ✅  Job output stored on executing node, fetched on demand

 Client-facing
   ✅  SubmitJob / CancelJob / GetJobStatus / ListJobs (paginated) / StreamJobs
   ✅  GetClusterStatus
   ✅  GetRaftLogEntries (debug)
   ✅  CLI with automatic leader redirect
   ✅  Web dashboard
   ✅  /health/live + /health/ready endpoints

 Persistence
   ✅  State persistence (RocksDB-backed Raft log + snapshot via --data-dir)

 Not supported / Out of scope
   ⬜  Job timeouts, retries, priorities
   ⬜  Reschedule on node failure
   ⬜  Typed job payloads (HttpCallback, WasmModule, DockerImage)
   ⬜  Parameterized job templates
   ⬜  Periodic / cron jobs
   ⬜  Resource-aware placement
   ⬜  Job dependency graphs (DAG)
   ⬜  Shared output storage
   ⬜  Prometheus metrics / OpenTelemetry tracing
   ⬜  Authorization (ACL tokens)
```
