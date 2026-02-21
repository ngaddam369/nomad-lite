# API Reference

## REST API (Dashboard)

The web dashboard exposes a REST API for job and cluster management.

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

## gRPC API

### SchedulerService (client-facing)

| Method | Description | Leader Only |
|--------|-------------|-------------|
| `SubmitJob(command)` | Submit a job | Yes |
| `GetJobStatus(job_id)` | Get job status | No |
| `ListJobs()` | List jobs (paginated) | No |
| `StreamJobs()` | Stream jobs | No |
| `GetClusterStatus()` | Cluster info | Forwarded to leader |
| `GetRaftLogEntries()` | View Raft log entries | Forwarded to leader |
| `TransferLeadership(target)` | Transfer leadership | Yes |
| `DrainNode()` | Drain node for maintenance | No |

#### SubmitJob error codes

| gRPC status | Meaning | Client action |
|---|---|---|
| `OK` | Job accepted and committed | — |
| `FAILED_PRECONDITION` | Node is not the leader | Redirect to the node ID in the message |
| `RESOURCE_EXHAUSTED` | Leader proposal queue is full (>256 pending) | Retry with exponential backoff |
| `DEADLINE_EXCEEDED` | Raft did not commit the entry within 5 seconds | Retry; may indicate a degraded cluster |
| `UNAVAILABLE` | Node is draining, or the Raft loop has stopped | Retry on a different node |
| `INVALID_ARGUMENT` | Empty command string | Fix the request |

### InternalService (node-to-node, not client-facing)

| Method | Description |
|--------|-------------|
| `GetJobOutput(job_id)` | Fetch job output from the node that executed it |

### RaftService (node-to-node, consensus protocol)

| Method | Description |
|--------|-------------|
| `AppendEntries` | Log replication and heartbeats |
| `RequestVote` | Leader election voting |
| `InstallSnapshot` | Transfer compacted state to slow followers |
