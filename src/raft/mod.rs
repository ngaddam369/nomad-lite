//! Raft consensus implementation for distributed coordination.
//!
//! This module implements the Raft consensus algorithm, providing:
//! - **Leader election**: Automatic leader election with randomized timeouts
//! - **Log replication**: Reliable replication of commands across the cluster
//! - **Safety guarantees**: Only one leader per term, committed entries persist
//!
//! # Architecture
//!
//! - [`RaftNode`]: Main coordinator that runs the consensus protocol
//! - [`RaftState`]: Persistent and volatile state (term, log, commit index)
//! - [`RaftRole`]: Node roles (Follower, Candidate, Leader)
//! - [`Command`]: Replicated commands (job submissions, status updates)
//!
//! # Usage
//!
//! ```ignore
//! let (raft_node, raft_rx) = RaftNode::new(config, None);
//! raft_node.connect_to_peers().await;
//! raft_node.run(raft_rx, shutdown_token).await;
//! ```

pub mod node;
pub mod rpc;
pub mod state;
pub mod timer;

pub use node::RaftNode;
pub use state::{Command, LogEntry, RaftRole, RaftState, Snapshot, SnapshotJob};
