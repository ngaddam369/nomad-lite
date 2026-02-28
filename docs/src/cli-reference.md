# CLI Reference

The `nomad-lite` binary provides both server and client functionality.

```
nomad-lite
├── server                        # Start a server node
├── job                           # Job management
│   ├── submit <COMMAND>         # Submit a new job
│   ├── status <JOB_ID>          # Get job status
│   ├── cancel <JOB_ID>          # Cancel a pending or running job
│   └── list                     # List all jobs
├── cluster                       # Cluster management
│   ├── status                   # Get cluster info
│   ├── transfer-leader          # Transfer leadership to another node
│   └── drain                    # Drain node for maintenance
└── log                           # Raft log inspection
    └── list                     # View committed log entries
```

## Server Options

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
| `--data-dir` | - | Path to RocksDB data directory for persistence (optional; omit for in-memory) |
| `--advertise-addr` | `127.0.0.1:<port>` | Address reported to peers and shown in cluster status; defaults to the listen port on loopback; override when nodes must be reachable at a specific hostname or external IP (e.g., `192.168.1.10:50051`) |

## Client Options

| Flag | Default | Description |
|------|---------|-------------|
| `-a, --addr` | `http://127.0.0.1:50051` | Server address |
| `-o, --output` | `table` | Output format: `table` or `json` |
| `--ca-cert` | - | CA certificate for TLS |
| `--cert` | - | Client certificate for mTLS |
| `--key` | - | Client private key for mTLS |

## `job submit` Options

| Flag | Default | Description |
|------|---------|-------------|
| `--image IMAGE` | server default (`alpine:latest`) | Docker image to run this job in; overrides the server-wide `--image` setting for this job only |

## Command Examples

**Submit a job:**

```bash
nomad-lite job submit "echo hello"
# Job submitted successfully!
# Job ID: ef319e40-c888-490d-8349-e9c05f78cf5a

# Override the Docker image for this specific job
nomad-lite job submit --image python:3.12-alpine "python3 -c 'print(42)'"
# Job submitted successfully!
# Job ID: 3b7a1c22-...
```

**Get job status:**

```bash
nomad-lite job status ef319e40-c888-490d-8349-e9c05f78cf5a
# Job ID:          ef319e40-c888-490d-8349-e9c05f78cf5a
# Status:          COMPLETED
# Exit Code:       0
# Assigned Worker: 1
# Executed By:     1
# Output:
#   hello
```

**Cancel a job:**

```bash
nomad-lite job cancel ef319e40-c888-490d-8349-e9c05f78cf5a
# job ef319e40-c888-490d-8349-e9c05f78cf5a cancelled
```

Cancelling a job that is already terminal returns an error:

```bash
nomad-lite job cancel ef319e40-c888-490d-8349-e9c05f78cf5a
# Error: job is already completed
```

**List all jobs:**

```bash
nomad-lite job list
# JOB ID                                 STATUS       WORKER   COMMAND
# ------------------------------------------------------------------------------
# ef319e40-c888-490d-8349-e9c05f78cf5a   COMPLETED    1        echo hello
#
# Showing 1 of 1 jobs
```

**List jobs with pagination:**

```bash
nomad-lite job list --page-size 50 --all  # Fetch all pages
nomad-lite job list --stream              # Use streaming API
```

**Filter jobs:**

```bash
# Filter by status
nomad-lite job list --status pending
nomad-lite job list --status completed

# Filter by worker node
nomad-lite job list --worker 2

# Filter by command substring (case-insensitive)
nomad-lite job list --command-substr echo

# Filter by creation time range (Unix ms timestamps)
nomad-lite job list --created-after-ms 1700000000000 --created-before-ms 1710000000000

# Combine filters
nomad-lite job list --status pending --command-substr echo --worker 1
```

Filter flags for `job list`:

| Flag | Description |
|------|-------------|
| `--status STATUS` | Only jobs with this status: `pending`, `running`, `completed`, `failed`, `cancelled` |
| `--worker WORKER_ID` | Only jobs whose `assigned_worker` or `executed_by` matches this node ID |
| `--command-substr SUBSTR` | Case-insensitive substring match on the job command |
| `--created-after-ms MS` | Only jobs created at or after this Unix timestamp (milliseconds) |
| `--created-before-ms MS` | Only jobs created at or before this Unix timestamp (milliseconds) |

> **Note:** Filters are not supported with `--stream` (only `--status` applies to the streaming API).

**Get cluster status:**

```bash
nomad-lite cluster status
# Cluster Status
# ========================================
# Term:   5
# Leader: Node 1
#
# Nodes:
# ID       ADDRESS                   STATUS
# ---------------------------------------------
# 1        0.0.0.0:50051             [+] alive
# 2        127.0.0.1:50052           [+] alive
# 3        127.0.0.1:50053           [+] alive
```

**Transfer leadership:**

```bash
# Transfer to a specific node
nomad-lite cluster -a http://127.0.0.1:50051 transfer-leader --to 2
# Leadership transferred successfully!
# New leader: Node 2

# Auto-select best candidate
nomad-lite cluster -a http://127.0.0.1:50051 transfer-leader
# Leadership transferred successfully!
# New leader: Node 3
```

**Drain a node for maintenance:**

```bash
# Drain the node: stops accepting jobs, waits for running jobs, transfers leadership
nomad-lite cluster -a http://127.0.0.1:50051 drain
# Draining node...
# Node drained successfully.
# Node drained successfully

# Verify leadership moved
nomad-lite cluster -a http://127.0.0.1:50052 status
```

**View Raft log entries:**

```bash
nomad-lite log list
# Raft Log Entries
# ================================================================================
# Commit Index: 6  |  Last Log Index: 6
#
# INDEX  TERM   COMMITTED  TYPE                 DETAILS
# --------------------------------------------------------------------------------
# 1      1      yes        Noop
# 2      1      yes        SubmitJob            job_id=bd764021-..., cmd=echo job1
# 3      1      yes        SubmitJob            job_id=1cce681f-..., cmd=echo job2
# 4      1      yes        SubmitJob            job_id=26694755-..., cmd=echo job3
# 5      1      yes        BatchUpdateJobStatus 3 updates
# 6      1      yes        UpdateJobStatus      job_id=17cc39b2-..., status=Completed
#
# Showing 6 entries
```

**View log entries after compaction:**

When log compaction has occurred, earlier entries are replaced by a snapshot:

```bash
nomad-lite log list
# Raft Log Entries
# ================================================================================
# First Available: 1001  |  Commit Index: 1050  |  Last Log Index: 1050
# (Entries 1-1000 were compacted into a snapshot)
#
# INDEX  TERM   COMMITTED  TYPE                 DETAILS
# ...
```

**View log entries with pagination:**

```bash
nomad-lite log list --start-index 1 --limit 50  # Start from index 1, max 50 entries
nomad-lite log list --start-index 10            # Start from index 10
```

**JSON output:**

```bash
nomad-lite job -o json list
nomad-lite cluster -o json status
nomad-lite log -o json list
```

## Automatic Leader Redirect

For local clusters (non-Docker), the CLI automatically redirects to the leader if you connect to a follower. This works for both `submit` and `cancel`:

```bash
# Connect to follower node (port 50052), CLI auto-redirects to leader
nomad-lite job -a http://127.0.0.1:50052 submit "echo hello"
# Redirecting to leader at http://127.0.0.1:50051...
# Job submitted successfully!

nomad-lite job -a http://127.0.0.1:50052 cancel <job-id>
# Redirecting to leader at http://127.0.0.1:50051...
# job <job-id> cancelled
```
