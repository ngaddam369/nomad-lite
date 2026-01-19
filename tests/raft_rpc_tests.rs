use nomad_lite::config::NodeConfig;
use nomad_lite::proto::{AppendEntriesRequest, VoteRequest};
use nomad_lite::raft::node::RaftNode;
use nomad_lite::raft::rpc::{handle_append_entries, handle_request_vote};
use nomad_lite::raft::state::{Command, LogEntry, RaftState};
use std::time::Duration;

#[test]
fn test_request_vote_grant_vote() {
    let mut state = RaftState::new();
    state.current_term = 1;

    let req = VoteRequest {
        term: 2,
        candidate_id: 2,
        last_log_index: 0,
        last_log_term: 0,
    };

    let resp = handle_request_vote(&mut state, &req, 1);

    assert!(resp.vote_granted);
    assert_eq!(resp.term, 2);
    assert_eq!(state.voted_for, Some(2));
}

#[test]
fn test_request_vote_reject_stale_term() {
    let mut state = RaftState::new();
    state.current_term = 5;

    let req = VoteRequest {
        term: 3, // Lower than current term
        candidate_id: 2,
        last_log_index: 0,
        last_log_term: 0,
    };

    let resp = handle_request_vote(&mut state, &req, 1);

    assert!(!resp.vote_granted);
    assert_eq!(resp.term, 5);
}

#[test]
fn test_request_vote_reject_already_voted() {
    let mut state = RaftState::new();
    state.current_term = 2;
    state.voted_for = Some(3); // Already voted for node 3

    let req = VoteRequest {
        term: 2,
        candidate_id: 2, // Different candidate
        last_log_index: 0,
        last_log_term: 0,
    };

    let resp = handle_request_vote(&mut state, &req, 1);

    assert!(!resp.vote_granted);
}

#[test]
fn test_request_vote_reject_outdated_log() {
    let mut state = RaftState::new();
    state.current_term = 2;
    state.log.push(LogEntry {
        term: 2,
        index: 1,
        command: Command::Noop,
    });

    let req = VoteRequest {
        term: 3,
        candidate_id: 2,
        last_log_index: 0, // Candidate has no logs
        last_log_term: 0,
    };

    let resp = handle_request_vote(&mut state, &req, 1);

    assert!(!resp.vote_granted);
}

#[test]
fn test_append_entries_heartbeat() {
    let mut state = RaftState::new();
    state.current_term = 1;

    let req = AppendEntriesRequest {
        term: 1,
        leader_id: 2,
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![],
        leader_commit: 0,
    };

    let resp = handle_append_entries(&mut state, &req, 1);

    assert!(resp.success);
    assert_eq!(resp.term, 1);
    assert_eq!(state.leader_id, Some(2));
}

#[test]
fn test_append_entries_reject_stale_term() {
    let mut state = RaftState::new();
    state.current_term = 5;

    let req = AppendEntriesRequest {
        term: 3, // Lower than current term
        leader_id: 2,
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![],
        leader_commit: 0,
    };

    let resp = handle_append_entries(&mut state, &req, 1);

    assert!(!resp.success);
    assert_eq!(resp.term, 5);
}

#[test]
fn test_append_entries_update_commit_index() {
    let mut state = RaftState::new();
    state.current_term = 1;
    state.log.push(LogEntry {
        term: 1,
        index: 1,
        command: Command::Noop,
    });

    let req = AppendEntriesRequest {
        term: 1,
        leader_id: 2,
        prev_log_index: 1,
        prev_log_term: 1,
        entries: vec![],
        leader_commit: 1,
    };

    let resp = handle_append_entries(&mut state, &req, 1);

    assert!(resp.success);
    assert_eq!(state.commit_index, 1);
}

#[test]
fn test_append_entries_missing_prev_log() {
    let mut state = RaftState::new();
    state.current_term = 1;
    // Empty log

    let req = AppendEntriesRequest {
        term: 1,
        leader_id: 2,
        prev_log_index: 5, // We don't have entry at index 5
        prev_log_term: 1,
        entries: vec![],
        leader_commit: 0,
    };

    let resp = handle_append_entries(&mut state, &req, 1);

    assert!(!resp.success);
}

#[test]
fn test_append_entries_higher_term_becomes_follower() {
    let mut state = RaftState::new();
    state.current_term = 1;
    state.become_candidate(1); // Node is a candidate

    let req = AppendEntriesRequest {
        term: 5, // Higher term
        leader_id: 2,
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![],
        leader_commit: 0,
    };

    let resp = handle_append_entries(&mut state, &req, 1);

    assert!(resp.success);
    assert_eq!(state.current_term, 5);
    assert_eq!(state.role, nomad_lite::raft::RaftRole::Follower);
}

#[test]
fn test_subscribe_commits_returns_receiver() {
    let config = NodeConfig::default();
    let (raft_node, _rx) = RaftNode::new(config);

    // Should be able to subscribe multiple times
    let _commit_rx1 = raft_node.subscribe_commits();
    let _commit_rx2 = raft_node.subscribe_commits();
}

#[tokio::test]
async fn test_commit_notification_on_follower_append_entries() {
    let config = NodeConfig::default();
    let (raft_node, _rx) = RaftNode::new(config);

    // Add an entry to the log so we can commit it
    {
        let mut state = raft_node.state.write().await;
        state.current_term = 1;
        state.log.push(LogEntry {
            term: 1,
            index: 1,
            command: Command::Noop,
        });
    }

    let mut commit_rx = raft_node.subscribe_commits();

    // Send AppendEntries with leader_commit = 1
    let req = AppendEntriesRequest {
        term: 1,
        leader_id: 2,
        prev_log_index: 1,
        prev_log_term: 1,
        entries: vec![],
        leader_commit: 1,
    };

    let resp = raft_node.handle_append_entries(req).await;
    assert!(resp.success);

    // Should receive notification
    let result = tokio::time::timeout(Duration::from_millis(100), commit_rx.changed()).await;
    assert!(result.is_ok(), "Should receive commit notification");
    assert_eq!(*commit_rx.borrow(), 1);
}

#[tokio::test]
async fn test_no_notification_when_commit_index_unchanged() {
    let config = NodeConfig::default();
    let (raft_node, _rx) = RaftNode::new(config);

    let mut commit_rx = raft_node.subscribe_commits();

    // Mark the current value as seen
    let _ = commit_rx.borrow_and_update();

    // Send AppendEntries with leader_commit = 0 (no change)
    let req = AppendEntriesRequest {
        term: 1,
        leader_id: 2,
        prev_log_index: 0,
        prev_log_term: 0,
        entries: vec![],
        leader_commit: 0,
    };

    let resp = raft_node.handle_append_entries(req).await;
    assert!(resp.success);

    // Should timeout waiting for notification (none sent)
    let result = tokio::time::timeout(Duration::from_millis(50), commit_rx.changed()).await;
    assert!(result.is_err(), "Should not receive notification when commit_index unchanged");
}
