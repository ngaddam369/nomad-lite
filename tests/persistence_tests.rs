use std::sync::Arc;

use tempfile::TempDir;

use nomad_lite::config::NodeConfig;
use nomad_lite::raft::state::{Command, LogEntry, Snapshot};
use nomad_lite::raft::{PersistedState, RaftStorage};

// ── helpers ──────────────────────────────────────────────────────────────────

fn noop_entry(term: u64, index: u64) -> LogEntry {
    LogEntry {
        term,
        index,
        command: Command::Noop,
    }
}

fn empty_snapshot(last_included_index: u64, last_included_term: u64) -> Snapshot {
    Snapshot {
        last_included_index,
        last_included_term,
        jobs: vec![],
        workers: vec![],
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// A brand-new RocksDB directory should return `None` from `load()`.
#[test]
fn test_fresh_storage_returns_none() {
    let dir = TempDir::new().unwrap();
    let storage = RaftStorage::open(dir.path());
    assert!(storage.load().is_none(), "fresh storage must return None");
}

/// Hard state (term, voted_for, log_offset) survives a DB reopen.
#[test]
fn test_persist_and_reload_hard_state() {
    let dir = TempDir::new().unwrap();

    // Write
    {
        let storage = RaftStorage::open(dir.path());
        storage.save_hard_state(5, Some(3), 0);
    }

    // Read back
    {
        let storage = RaftStorage::open(dir.path());
        let p: PersistedState = storage.load().expect("should have persisted state");
        assert_eq!(p.current_term, 5);
        assert_eq!(p.voted_for, Some(3));
        assert_eq!(p.log_offset, 0);
        assert!(p.snapshot.is_none());
        assert!(p.log.is_empty());
    }
}

/// Three log entries survive a DB reopen in index order.
#[test]
fn test_persist_and_reload_log_entries() {
    let dir = TempDir::new().unwrap();

    {
        let storage = RaftStorage::open(dir.path());
        storage.save_hard_state(1, None, 0);
        storage.append_entry(&noop_entry(1, 1));
        storage.append_entry(&noop_entry(1, 2));
        storage.append_entry(&noop_entry(2, 3));
    }

    {
        let storage = RaftStorage::open(dir.path());
        let p = storage.load().expect("should have state");
        assert_eq!(p.log.len(), 3);
        assert_eq!(p.log[0].index, 1);
        assert_eq!(p.log[0].term, 1);
        assert_eq!(p.log[1].index, 2);
        assert_eq!(p.log[2].index, 3);
        assert_eq!(p.log[2].term, 2);
    }
}

/// After writing 5 entries and truncating from index 3, only entries 1-2 remain.
#[test]
fn test_truncate_log() {
    let dir = TempDir::new().unwrap();

    {
        let storage = RaftStorage::open(dir.path());
        storage.save_hard_state(1, None, 0);
        for i in 1u64..=5 {
            storage.append_entry(&noop_entry(1, i));
        }
        storage.truncate_from(3);
    }

    {
        let storage = RaftStorage::open(dir.path());
        let p = storage.load().expect("should have state");
        assert_eq!(p.log.len(), 2, "only entries 1-2 should remain");
        assert_eq!(p.log[0].index, 1);
        assert_eq!(p.log[1].index, 2);
    }
}

/// Saving a snapshot at offset 3 deletes log entries 1-3 from the DB;
/// entries 4-5 survive.
#[test]
fn test_save_snapshot_deletes_compacted_log() {
    let dir = TempDir::new().unwrap();

    {
        let storage = RaftStorage::open(dir.path());
        storage.save_hard_state(1, None, 0);
        for i in 1u64..=5 {
            storage.append_entry(&noop_entry(1, i));
        }
        let snapshot = empty_snapshot(3, 1);
        storage.save_snapshot(&snapshot, 3);
    }

    {
        let storage = RaftStorage::open(dir.path());
        let p = storage.load().expect("should have state");
        assert!(p.snapshot.is_some(), "snapshot should be persisted");
        assert_eq!(p.log_offset, 3);
        assert_eq!(p.log.len(), 2, "only entries 4-5 should remain");
        assert_eq!(p.log[0].index, 4);
        assert_eq!(p.log[1].index, 5);
    }
}

/// After a compaction (snapshot saved + old entries deleted), reopening the storage
/// reconstructs the correct log_offset, snapshot, and post-compaction log entries.
#[test]
fn test_restart_after_compaction() {
    let dir = TempDir::new().unwrap();

    // First run: write 5 entries, then compact at index 3
    {
        let storage = RaftStorage::open(dir.path());
        storage.save_hard_state(1, None, 0);
        for i in 1u64..=5 {
            storage.append_entry(&noop_entry(1, i));
        }
        // Compact: snapshot covers 1-3, deletes those entries from RocksDB
        let snapshot = empty_snapshot(3, 1);
        storage.save_snapshot(&snapshot, 3);
    }

    // Reopen and verify post-compaction state is reconstructed correctly
    {
        let storage = RaftStorage::open(dir.path());
        let p = storage
            .load()
            .expect("should have persisted state after compaction");

        assert_eq!(p.log_offset, 3, "log_offset must equal snapshot boundary");

        let snap = p
            .snapshot
            .expect("snapshot must be present after compaction");
        assert_eq!(snap.last_included_index, 3);
        assert_eq!(snap.last_included_term, 1);

        assert_eq!(p.log.len(), 2, "only entries 4 and 5 should survive");
        assert_eq!(p.log[0].index, 4);
        assert_eq!(p.log[1].index, 5);
    }
}

/// A `RaftNode` created with persisted state restores the saved term.
#[tokio::test]
async fn test_node_restores_term_after_restart() {
    use nomad_lite::raft::node::RaftNode;

    let dir = TempDir::new().unwrap();

    // First run: persist term 7, voted_for node 2.
    {
        let storage = RaftStorage::open(dir.path());
        storage.save_hard_state(7, Some(2), 0);
    }

    // Second run: load and feed into RaftNode.
    let storage = Arc::new(RaftStorage::open(dir.path()));
    let persisted = storage.load().expect("should have persisted state");
    assert_eq!(persisted.current_term, 7);
    assert_eq!(persisted.voted_for, Some(2));

    let config = NodeConfig::default();
    let (raft_node, _rx) = RaftNode::new_with_storage(config, None, Some(storage), Some(persisted));

    let state = raft_node.state.read().await;
    assert_eq!(state.current_term, 7, "term must be restored from storage");
    assert_eq!(state.voted_for, Some(2), "voted_for must be restored");
}
