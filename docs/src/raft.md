# Raft Consensus

## Timing

- **Election timeout:** 150-300ms (randomized)
- **Heartbeat interval:** 50ms
- **Peer liveness window:** 3s — a node is shown as `is_alive: false` in `GetClusterStatus` if no successful AppendEntries/heartbeat has been received from it within this window

## Log Replication

1. Client sends command to leader
2. Leader appends to log and replicates via `AppendEntries`
3. Majority acknowledgment → committed
4. Applied to state machine

### Proposal backpressure

The leader's proposal channel holds at most **256 pending commands**. If the channel is full, `SubmitJob` returns `RESOURCE_EXHAUSTED` immediately — the caller should retry with exponential backoff. Additionally, if the Raft loop accepts the command but quorum is lost before the entry commits, `SubmitJob` returns `DEADLINE_EXCEEDED` after **5 seconds**. Both guarantees ensure clients always get a bounded response rather than hanging indefinitely.

## State Machine Commands

Each log entry carries one of the following commands applied to the job queue on every node:

| Command | Description |
|---|---|
| `SubmitJob` | Adds a new job in Pending state |
| `AssignJob` | Assigns a pending job to a specific worker, setting it to Running on all nodes |
| `RegisterWorker` | Records a worker in the cluster-wide registry |
| `UpdateJobStatus` / `BatchUpdateJobStatus` | Marks one or more jobs Completed or Failed |

`AssignJob` is proposed by the leader's scheduler loop after it selects the least-loaded live worker. Because this command travels through the Raft log, every follower applies the same assignment atomically — workers discover their jobs by querying the local job queue rather than a node-local in-memory map.

## Log Compaction

When the in-memory log exceeds 1000 entries, the committed prefix is replaced with a snapshot of the current state (job queue + worker registrations). This bounds memory usage regardless of throughput. Followers that fall too far behind receive the snapshot via `InstallSnapshot` RPC instead of replaying individual entries.

## Safety Guarantees

- Election safety: One leader per term
- Leader append-only: Never overwrites log
- Log matching: Same index/term = identical
- Leader completeness: Committed entries persist

## Cluster Sizing

| Nodes | Majority | Fault Tolerance |
|-------|----------|-----------------|
| 3 | 2 | 1 failure |
| 5 | 3 | 2 failures |
| 7 | 4 | 3 failures |

Use odd numbers—even numbers add overhead without improving fault tolerance.
