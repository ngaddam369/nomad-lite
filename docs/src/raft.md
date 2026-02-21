# Raft Consensus

## Timing

- **Election timeout:** 150-300ms (randomized)
- **Heartbeat interval:** 50ms

## Log Replication

1. Client sends command to leader
2. Leader appends to log and replicates via `AppendEntries`
3. Majority acknowledgment → committed
4. Applied to state machine

### Proposal backpressure

The leader's proposal channel holds at most **256 pending commands**. If the channel is full, `SubmitJob` returns `RESOURCE_EXHAUSTED` immediately — the caller should retry with exponential backoff. Additionally, if the Raft loop accepts the command but quorum is lost before the entry commits, `SubmitJob` returns `DEADLINE_EXCEEDED` after **5 seconds**. Both guarantees ensure clients always get a bounded response rather than hanging indefinitely.

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
