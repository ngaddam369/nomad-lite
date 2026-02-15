# Architecture

## Cluster Overview

```mermaid
graph LR
    subgraph Clients
        CLI[CLI]
        REST[REST]
        GRPC[gRPC]
    end

    subgraph Cluster[3-Node Raft Cluster]
        N1[Node 1<br/>gRPC :50051<br/>Dashboard :8081]
        N2[Node 2<br/>gRPC :50052<br/>Dashboard :8082]
        N3[Node 3<br/>gRPC :50053<br/>Dashboard :8083]

        N1 <-->|Raft RPCs| N2
        N1 <-->|Raft RPCs| N3
        N2 <-->|Raft RPCs| N3
    end

    subgraph Docker[Docker Containers]
        D1[Container]
        D2[Container]
        D3[Container]
    end

    CLI -->|Submit to leader| N1
    REST -->|HTTP API| N1
    GRPC -->|gRPC API| N1

    N1 -->|docker run| D1
    N2 -->|docker run| D2
    N3 -->|docker run| D3

    classDef leader fill:#90EE90,stroke:#006400,stroke-width:2px
    classDef follower fill:#FFE4B5,stroke:#FF8C00,stroke-width:2px
    class N1 leader
    class N2,N3 follower
```

## Node Internal Architecture

```mermaid
graph TB
    subgraph External[External APIs]
        GRPC[gRPC Server<br/>SubmitJob, GetStatus, ListJobs]
        DASH[Dashboard<br/>REST API + Web UI]
    end

    subgraph Core[Core Components]
        RAFT[Raft Module<br/>Leader Election<br/>Log Replication]
        LOG[Raft Log<br/>In-Memory]
    end

    subgraph Loops[Background Loops]
        SCHED[Scheduler Loop<br/>Event-driven<br/>Applies commits<br/>Assigns jobs on leader]
        WORKER[Worker Loop<br/>Event-driven + 2s heartbeat<br/>Executes assigned jobs]
    end

    QUEUE[Job Queue<br/>State Machine]
    DOCKER[Docker Container<br/>Sandboxed Execution]

    %% External to Core
    GRPC -->|Commands| RAFT
    DASH -->|Commands| RAFT
    DASH -->|Query| QUEUE

    %% Core
    RAFT <--> LOG

    %% Loops subscribe/interact
    SCHED -.->|Subscribe commits| RAFT
    SCHED -->|Apply entries & Assign jobs| QUEUE
    WORKER -->|Notified & Update| QUEUE
    WORKER -->|Execute| DOCKER

    classDef api fill:#87CEEB,stroke:#4682B4
    classDef core fill:#90EE90,stroke:#006400
    classDef loop fill:#FFE4B5,stroke:#FF8C00
    classDef state fill:#E6E6FA,stroke:#9370DB

    class GRPC,DASH api
    class RAFT,LOG core
    class SCHED,WORKER loop
    class QUEUE,DOCKER state
```

**Key Points:**

- Every node runs all components (gRPC, Dashboard, Raft, Scheduler, Worker)
- Only the leader's Scheduler Loop assigns jobs; followers just apply committed entries
- Workers are notified immediately when jobs are assigned - no polling or RPC needed for job dispatch
- Job output is stored only on the executing node (fetched via `InternalService` RPC when queried)

## Data Flow

```mermaid
sequenceDiagram
    participant Client as CLI Client
    participant N1 as Node 1 (Leader)<br/>gRPC Server
    participant Raft1 as Node 1<br/>Raft Module
    participant N2 as Node 2 (Follower)<br/>Raft Module
    participant N3 as Node 3 (Follower)<br/>Raft Module
    participant Log1 as Node 1<br/>Raft Log
    participant Queue1 as Node 1<br/>Job Queue
    participant Sched as Scheduler Loop<br/>(Leader Only)
    participant Worker as Worker Loop<br/>(Node 2)
    participant Docker as Docker Container

    Note over Client,Docker: Job Submission Flow (Write Operation)

    Client->>N1: 1. SubmitJob("echo hello")
    N1->>N1: 2. Create Job object<br/>job_id = uuid
    N1->>Raft1: 3. Propose SubmitJob command
    Raft1->>Log1: 4. Append to local log<br/>(index=N, term=T)

    par Replicate to Followers
        Raft1->>N2: 5a. AppendEntries RPC<br/>(log entry)
        Raft1->>N3: 5b. AppendEntries RPC<br/>(log entry)
    end

    N2-->>Raft1: 6a. ACK (success)
    N3-->>Raft1: 6b. ACK (success)

    Note over Raft1: Majority reached (2/3)

    Raft1-->>N1: 7. Commit confirmed
    N1->>Queue1: 8. Add job to queue<br/>Status: PENDING
    N1-->>Client: 9. Response: job_id

    Note over Client,Docker: Job Assignment Flow (Leader Scheduler Loop - event-driven)

    Sched->>Raft1: 10. Subscribe to commits
    Raft1-->>Sched: 11. Commit notification
    Sched->>Queue1: 12. Apply committed entry<br/>(idempotent add)

    Sched->>Queue1: 13. Check pending jobs
    Queue1-->>Sched: 14. Job found (PENDING)
    Sched->>Sched: 15. Select least-loaded worker<br/>(Node 2)
    Sched->>Queue1: 16. Assign job to worker 2<br/>Status: RUNNING

    Note over Client,Docker: Job Execution Flow (Worker Loop - notified on assignment)

    Worker->>Queue1: 17. Notified, check assigned jobs
    Queue1-->>Worker: 18. Job assigned to me<br/>(RUNNING status)

    Worker->>Docker: 19. docker run alpine:latest<br/>--network=none --read-only<br/>--memory=256m --cpus=0.5<br/>echo hello

    Docker-->>Worker: 20. Output: "hello\n"<br/>Exit code: 0

    Worker->>Queue1: 21. Update local job result<br/>Status: COMPLETED<br/>Output stored locally

    Note over Worker,Raft1: If this node is leader, replicate status

    Worker->>Raft1: 22. Propose UpdateJobStatus
    Raft1->>N2: 23. AppendEntries (status update)
    Raft1->>N3: 23. AppendEntries (status update)

    Note over Client,Docker: Job Status Query Flow (Read Operation)

    Client->>N1: 24. GetJobStatus(job_id)
    N1->>Queue1: 25. Query job queue
    Queue1-->>N1: 26. Job metadata<br/>(executed_by: Node 2)
    N1->>N2: 27. GetJobOutput RPC<br/>(fetch from executor)
    N2-->>N1: 28. Output: "hello"
    N1-->>Client: 29. Response: COMPLETED<br/>Output: "hello"

    Note over Client,Docker: Leader Election Flow (Failure Scenario)

    Note over Raft1: Leader crashes!

    Note over N2,N3: Election timeout (150-300ms)<br/>No heartbeat received

    N2->>N2: Election timeout triggered
    N2->>N2: Increment term, become candidate
    N2->>N3: RequestVote RPC (term=T+1)
    N3-->>N2: VoteGranted

    Note over N2: Won election with majority

    N2->>N2: Become Leader
    Note over N2: Scheduler loop now active<br/>(assigns jobs)

    Note over N2,N3: Node 2 now leads cluster
```
