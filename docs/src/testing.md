# Testing

```bash
cargo test                # Run all tests
cargo test --lib          # Unit tests only
cargo test --test <name>  # Specific test suite
```

## Test Suites

| Suite | Description |
|-------|-------------|
| `lib` (unit) | Config, Raft state machine, RPC serialization |
| `scheduler_tests` | Job queue, worker assignment, heartbeats |
| `raft_rpc_tests` | AppendEntries, RequestVote, term handling |
| `integration_tests` | Multi-node election, replication, consistency |
| `failover_tests` | Leader crash, re-election, quorum loss |
| `partition_tests` | Network partitions, split-brain prevention, healing |
| `chaos_tests` | Rapid leader churn, network flapping, cascading failures, full isolation recovery |
| `tls_tests` | mTLS certificate loading, encrypted cluster communication |
| `executor_tests` | Docker sandbox command execution |
| `internal_service_tests` | Internal node-to-node API, job output fetching |
| `dashboard_tests` | REST API endpoints |
| `leadership_transfer_tests` | Voluntary leadership transfer, auto-select, non-leader rejection |
| `drain_tests` | Node draining, job rejection during drain, leadership handoff |
| `compaction_tests` | Log compaction trigger, snapshot transfer to slow followers, state consistency |
