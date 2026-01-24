use std::collections::HashMap;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::scheduler::JobStatus;

/// Raft node role
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaftRole {
    Follower,
    Candidate,
    Leader,
}

impl std::fmt::Display for RaftRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RaftRole::Follower => write!(f, "follower"),
            RaftRole::Candidate => write!(f, "candidate"),
            RaftRole::Leader => write!(f, "leader"),
        }
    }
}

/// Commands that can be replicated through Raft
#[derive(Debug, Clone)]
pub enum Command {
    /// Submit a new job
    SubmitJob {
        job_id: Uuid,
        command: String,
        created_at: DateTime<Utc>,
    },
    /// Update job status (metadata only - output stored locally on executing node)
    UpdateJobStatus {
        job_id: Uuid,
        status: JobStatus,
        executed_by: u64,
        exit_code: Option<i32>,
        completed_at: Option<DateTime<Utc>>,
    },
    /// Register a worker
    RegisterWorker { worker_id: u64 },
    /// No-op command (used for leader commit)
    Noop,
}

/// A single entry in the Raft log
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub term: u64,
    pub index: u64,
    pub command: Command,
}

/// Persistent state on all servers (would be persisted to disk in production).
///
/// # Raft Safety Invariants
///
/// This implementation maintains the following safety guarantees:
///
/// ## Election Safety
/// At most one leader can be elected in a given term. Enforced by:
/// - Each node votes for at most one candidate per term (`voted_for`)
/// - Candidate must receive majority of votes to become leader
///
/// ## Leader Append-Only
/// A leader never overwrites or deletes entries in its log. Enforced by:
/// - Leaders only append new entries via `append_entry()`
/// - Log truncation only occurs on followers during replication conflicts
///
/// ## Log Matching
/// If two logs contain an entry with the same index and term, then the logs
/// are identical in all entries up through that index. Enforced by:
/// - `AppendEntries` consistency check (prev_log_index, prev_log_term)
/// - Conflicting entries are truncated before appending
///
/// ## Leader Completeness
/// If a log entry is committed in a given term, that entry will be present
/// in the logs of all leaders for higher terms. Enforced by:
/// - Vote restriction: candidates must have up-to-date logs (`is_log_up_to_date`)
/// - Leaders only commit entries from their current term
///
/// ## State Machine Safety
/// If a server has applied a log entry at a given index, no other server will
/// ever apply a different entry for that index. Enforced by:
/// - Entries are only applied after being committed (`last_applied <= commit_index`)
/// - Committed entries are never overwritten (Leader Completeness)
#[derive(Debug)]
pub struct RaftState {
    // Persistent state
    pub current_term: u64,
    pub voted_for: Option<u64>,
    pub log: Vec<LogEntry>,

    // Volatile state on all servers
    pub commit_index: u64,
    pub last_applied: u64,

    // Volatile state on leaders (reinitialized after election)
    pub next_index: HashMap<u64, u64>,
    pub match_index: HashMap<u64, u64>,

    // Current role
    pub role: RaftRole,

    // Known leader (if any)
    pub leader_id: Option<u64>,

    // Votes received in current election (for candidates)
    pub votes_received: u64,
}

impl RaftState {
    pub fn new() -> Self {
        Self {
            current_term: 0,
            voted_for: None,
            log: Vec::new(),
            commit_index: 0,
            last_applied: 0,
            next_index: HashMap::new(),
            match_index: HashMap::new(),
            role: RaftRole::Follower,
            leader_id: None,
            votes_received: 0,
        }
    }

    /// Get the last log index
    pub fn last_log_index(&self) -> u64 {
        self.log.last().map(|e| e.index).unwrap_or(0)
    }

    /// Get the last log term
    pub fn last_log_term(&self) -> u64 {
        self.log.last().map(|e| e.term).unwrap_or(0)
    }

    /// Get log entry at index (1-indexed)
    pub fn get_entry(&self, index: u64) -> Option<&LogEntry> {
        if index == 0 {
            return None;
        }
        self.log.get((index - 1) as usize)
    }

    /// Get entries starting from index (inclusive)
    pub fn get_entries_from(&self, start_index: u64) -> Vec<LogEntry> {
        if start_index == 0 {
            return self.log.clone();
        }
        let start = (start_index - 1) as usize;
        if start >= self.log.len() {
            return Vec::new();
        }
        self.log[start..].to_vec()
    }

    /// Append a new entry to the log
    pub fn append_entry(&mut self, command: Command) -> &LogEntry {
        let index = self.last_log_index() + 1;
        let entry = LogEntry {
            term: self.current_term,
            index,
            command,
        };
        self.log.push(entry);
        self.log.last().unwrap()
    }

    /// Truncate log from index (inclusive) and append new entries
    pub fn truncate_and_append(&mut self, from_index: u64, entries: Vec<LogEntry>) {
        if from_index > 0 {
            let truncate_at = (from_index - 1) as usize;
            if truncate_at < self.log.len() {
                self.log.truncate(truncate_at);
            }
        } else {
            self.log.clear();
        }
        self.log.extend(entries);
    }

    /// Check if candidate's log is at least as up-to-date as ours
    pub fn is_log_up_to_date(&self, last_log_index: u64, last_log_term: u64) -> bool {
        let our_last_term = self.last_log_term();
        let our_last_index = self.last_log_index();

        // Candidate's log is up-to-date if:
        // 1. Their last term is greater, OR
        // 2. Terms are equal and their index is >= ours
        last_log_term > our_last_term
            || (last_log_term == our_last_term && last_log_index >= our_last_index)
    }

    /// Transition to follower state
    pub fn become_follower(&mut self, term: u64) {
        self.role = RaftRole::Follower;
        self.current_term = term;
        self.voted_for = None;
        self.votes_received = 0;
    }

    /// Transition to candidate state
    pub fn become_candidate(&mut self, my_id: u64) {
        self.role = RaftRole::Candidate;
        self.current_term += 1;
        self.voted_for = Some(my_id);
        self.votes_received = 1; // Vote for self
        self.leader_id = None;
    }

    /// Transition to leader state
    pub fn become_leader(&mut self, my_id: u64, peer_ids: &[u64]) {
        self.role = RaftRole::Leader;
        self.leader_id = Some(my_id);

        // Initialize next_index and match_index for all peers
        let last_log_index = self.last_log_index();
        self.next_index.clear();
        self.match_index.clear();
        for &peer_id in peer_ids {
            self.next_index.insert(peer_id, last_log_index + 1);
            self.match_index.insert(peer_id, 0);
        }
    }
}

impl Default for RaftState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_is_follower() {
        let state = RaftState::new();
        assert_eq!(state.role, RaftRole::Follower);
        assert_eq!(state.current_term, 0);
        assert_eq!(state.voted_for, None);
        assert!(state.log.is_empty());
    }

    #[test]
    fn test_become_candidate() {
        let mut state = RaftState::new();
        state.become_candidate(1);

        assert_eq!(state.role, RaftRole::Candidate);
        assert_eq!(state.current_term, 1);
        assert_eq!(state.voted_for, Some(1));
        assert_eq!(state.votes_received, 1); // Self-vote
        assert_eq!(state.leader_id, None);
    }

    #[test]
    fn test_become_leader() {
        let mut state = RaftState::new();
        state.become_candidate(1);
        state.become_leader(1, &[2, 3]);

        assert_eq!(state.role, RaftRole::Leader);
        assert_eq!(state.leader_id, Some(1));
        assert_eq!(state.next_index.get(&2), Some(&1));
        assert_eq!(state.next_index.get(&3), Some(&1));
        assert_eq!(state.match_index.get(&2), Some(&0));
        assert_eq!(state.match_index.get(&3), Some(&0));
    }

    #[test]
    fn test_become_follower() {
        let mut state = RaftState::new();
        state.become_candidate(1);
        state.become_follower(5);

        assert_eq!(state.role, RaftRole::Follower);
        assert_eq!(state.current_term, 5);
        assert_eq!(state.voted_for, None);
        assert_eq!(state.votes_received, 0);
    }

    #[test]
    fn test_append_entry() {
        let mut state = RaftState::new();
        state.current_term = 1;

        let entry = state.append_entry(Command::Noop);
        assert_eq!(entry.term, 1);
        assert_eq!(entry.index, 1);

        state.current_term = 2;
        let entry2 = state.append_entry(Command::Noop);
        assert_eq!(entry2.term, 2);
        assert_eq!(entry2.index, 2);

        assert_eq!(state.last_log_index(), 2);
        assert_eq!(state.last_log_term(), 2);
    }

    #[test]
    fn test_get_entry() {
        let mut state = RaftState::new();
        state.current_term = 1;
        state.append_entry(Command::Noop);
        state.current_term = 2;
        state.append_entry(Command::Noop);

        assert!(state.get_entry(0).is_none());
        assert_eq!(state.get_entry(1).unwrap().term, 1);
        assert_eq!(state.get_entry(2).unwrap().term, 2);
        assert!(state.get_entry(3).is_none());
    }

    #[test]
    fn test_get_entries_from() {
        let mut state = RaftState::new();
        state.current_term = 1;
        state.append_entry(Command::Noop);
        state.current_term = 2;
        state.append_entry(Command::Noop);
        state.current_term = 3;
        state.append_entry(Command::Noop);

        let entries = state.get_entries_from(2);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].index, 2);
        assert_eq!(entries[1].index, 3);

        let all_entries = state.get_entries_from(0);
        assert_eq!(all_entries.len(), 3);

        let no_entries = state.get_entries_from(10);
        assert!(no_entries.is_empty());
    }

    #[test]
    fn test_truncate_and_append() {
        let mut state = RaftState::new();
        state.current_term = 1;
        state.append_entry(Command::Noop);
        state.append_entry(Command::Noop);
        state.append_entry(Command::Noop);

        // Truncate from index 2 and append new entries
        let new_entries = vec![
            LogEntry {
                term: 2,
                index: 2,
                command: Command::Noop,
            },
            LogEntry {
                term: 2,
                index: 3,
                command: Command::Noop,
            },
        ];
        state.truncate_and_append(2, new_entries);

        assert_eq!(state.log.len(), 3);
        assert_eq!(state.log[0].term, 1);
        assert_eq!(state.log[1].term, 2);
        assert_eq!(state.log[2].term, 2);
    }

    #[test]
    fn test_is_log_up_to_date() {
        let mut state = RaftState::new();

        // Empty log - any log is up-to-date
        assert!(state.is_log_up_to_date(0, 0));
        assert!(state.is_log_up_to_date(1, 1));

        // Add some entries
        state.current_term = 1;
        state.append_entry(Command::Noop);
        state.current_term = 2;
        state.append_entry(Command::Noop);

        // Our log: [(term=1, idx=1), (term=2, idx=2)]
        // last_term=2, last_index=2

        // Higher term is always up-to-date
        assert!(state.is_log_up_to_date(1, 3));

        // Same term, same or higher index is up-to-date
        assert!(state.is_log_up_to_date(2, 2));
        assert!(state.is_log_up_to_date(3, 2));

        // Lower term is never up-to-date
        assert!(!state.is_log_up_to_date(5, 1));

        // Same term, lower index is not up-to-date
        assert!(!state.is_log_up_to_date(1, 2));
    }

    #[test]
    fn test_state_transitions() {
        let mut state = RaftState::new();

        // Start as follower
        assert_eq!(state.role, RaftRole::Follower);

        // Become candidate (simulating election timeout)
        state.become_candidate(1);
        assert_eq!(state.role, RaftRole::Candidate);
        assert_eq!(state.current_term, 1);

        // Win election
        state.votes_received = 2; // Self + one other
        state.become_leader(1, &[2, 3]);
        assert_eq!(state.role, RaftRole::Leader);

        // Discover higher term
        state.become_follower(5);
        assert_eq!(state.role, RaftRole::Follower);
        assert_eq!(state.current_term, 5);
    }
}
