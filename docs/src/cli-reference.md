# CLI Reference

The `nomad-lite` binary provides both server and client functionality.

```
nomad-lite
├── server                        # Start a server node
├── job                           # Job management
│   ├── submit <COMMAND>         # Submit a new job
│   ├── status <JOB_ID>          # Get job status
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

## Client Options

| Flag | Default | Description |
|------|---------|-------------|
| `-a, --addr` | `http://127.0.0.1:50051` | Server address |
| `-o, --output` | `table` | Output format: `table` or `json` |
| `--ca-cert` | - | CA certificate for TLS |
| `--cert` | - | Client certificate for mTLS |
| `--key` | - | Client private key for mTLS |

## Command Examples

**Submit a job:**

```bash
nomad-lite job submit "echo hello"
# Job submitted successfully!
# Job ID: ef319e40-c888-490d-8349-e9c05f78cf5a
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

For local clusters (non-Docker), the CLI automatically redirects to the leader if you connect to a follower:

```bash
# Connect to follower node (port 50052), CLI auto-redirects to leader
nomad-lite job -a http://127.0.0.1:50052 submit "echo hello"
# Redirecting to leader at 127.0.0.1:50051...
# Job submitted successfully!
```
