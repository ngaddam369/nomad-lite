use nomad_lite::proto::{AppendEntriesRequest, VoteRequest};
use nomad_lite::raft::rpc::{handle_append_entries, handle_request_vote};
use nomad_lite::raft::state::{Command, LogEntry, RaftState};

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
