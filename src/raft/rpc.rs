use chrono::{DateTime, TimeZone, Utc};

use crate::proto::{
    AppendEntriesRequest, AppendEntriesResponse, LogEntry as ProtoLogEntry, VoteRequest,
    VoteResponse,
};
use crate::raft::state::{Command, LogEntry, RaftState};
use crate::scheduler::JobStatus;
use uuid::Uuid;

/// Handle RequestVote RPC
pub fn handle_request_vote(
    state: &mut RaftState,
    req: &VoteRequest,
    my_id: u64,
) -> Result<VoteResponse, String> {
    // If request term is greater, update our term and become follower
    if req.term > state.current_term {
        state.become_follower(req.term);
    }

    let vote_granted = if req.term < state.current_term {
        // Reject if request term is less than our current term
        false
    } else if state.voted_for.is_some() && state.voted_for != Some(req.candidate_id) {
        // Already voted for someone else in this term
        false
    } else if !state.is_log_up_to_date(req.last_log_index, req.last_log_term) {
        // Candidate's log is not up-to-date
        false
    } else {
        // Grant vote
        state.voted_for = Some(req.candidate_id);
        true
    };

    tracing::debug!(
        node_id = my_id,
        candidate = req.candidate_id,
        term = req.term,
        granted = vote_granted,
        "RequestVote response"
    );

    Ok(VoteResponse {
        term: state.current_term,
        vote_granted,
    })
}

/// Handle AppendEntries RPC
pub fn handle_append_entries(
    state: &mut RaftState,
    req: &AppendEntriesRequest,
    my_id: u64,
) -> Result<AppendEntriesResponse, String> {
    // If request term is greater, update our term and become follower
    if req.term > state.current_term {
        state.become_follower(req.term);
    }

    // Reject if request term is less than our current term
    if req.term < state.current_term {
        return Ok(AppendEntriesResponse {
            term: state.current_term,
            success: false,
            match_index: state.last_log_index(),
        });
    }

    // Valid AppendEntries from leader - reset to follower if we're a candidate
    if state.role != crate::raft::RaftRole::Follower {
        state.become_follower(req.term);
    }
    state.leader_id = Some(req.leader_id);

    // Check if we have the prev_log entry
    if req.prev_log_index > 0 {
        match state.get_entry(req.prev_log_index) {
            None => {
                // We don't have the entry at prev_log_index
                return Ok(AppendEntriesResponse {
                    term: state.current_term,
                    success: false,
                    match_index: state.last_log_index(),
                });
            }
            Some(entry) => {
                if entry.term != req.prev_log_term {
                    // Term mismatch - truncate and reject
                    state.log.truncate((req.prev_log_index - 1) as usize);
                    return Ok(AppendEntriesResponse {
                        term: state.current_term,
                        success: false,
                        match_index: state.last_log_index(),
                    });
                }
            }
        }
    }

    // Append new entries (if any)
    if !req.entries.is_empty() {
        let new_entries: Vec<LogEntry> = req
            .entries
            .iter()
            .filter_map(|proto| match proto_to_log_entry(proto) {
                Some(entry) => Some(entry),
                None => {
                    tracing::warn!(
                        node_id = my_id,
                        term = proto.term,
                        index = proto.index,
                        "Skipping malformed log entry"
                    );
                    None
                }
            })
            .collect();

        let start_index = req.prev_log_index + 1;
        state.truncate_and_append(start_index, new_entries);

        tracing::debug!(
            node_id = my_id,
            entries_appended = req.entries.len(),
            new_last_index = state.last_log_index(),
            "Appended entries"
        );
    }

    // Update commit index
    if req.leader_commit > state.commit_index {
        state.commit_index = std::cmp::min(req.leader_commit, state.last_log_index());
    }

    Ok(AppendEntriesResponse {
        term: state.current_term,
        success: true,
        match_index: state.last_log_index(),
    })
}

/// Convert protobuf LogEntry to internal LogEntry
fn proto_to_log_entry(proto: &ProtoLogEntry) -> Option<LogEntry> {
    let command = match &proto.command {
        Some(cmd) => match &cmd.command_type {
            Some(crate::proto::command::CommandType::SubmitJob(submit)) => Command::SubmitJob {
                job_id: Uuid::parse_str(&submit.job_id).ok()?,
                command: submit.command.clone(),
                created_at: ms_to_datetime(submit.created_at_ms),
            },
            Some(crate::proto::command::CommandType::UpdateJobStatus(update)) => {
                Command::UpdateJobStatus {
                    job_id: Uuid::parse_str(&update.job_id).ok()?,
                    status: proto_status_to_internal(update.status()),
                    executed_by: update.executed_by,
                    exit_code: update.exit_code,
                    completed_at: update.completed_at_ms.map(ms_to_datetime),
                }
            }
            Some(crate::proto::command::CommandType::RegisterWorker(register)) => {
                Command::RegisterWorker {
                    worker_id: register.worker_id,
                }
            }
            None => Command::Noop,
        },
        None => Command::Noop,
    };

    Some(LogEntry {
        term: proto.term,
        index: proto.index,
        command,
    })
}

/// Convert internal LogEntry to protobuf LogEntry
pub fn log_entry_to_proto(entry: &LogEntry) -> ProtoLogEntry {
    use crate::proto::{
        command::CommandType, Command as ProtoCommand, RegisterWorkerCommand, SubmitJobCommand,
        UpdateJobStatusCommand,
    };

    let command = match &entry.command {
        Command::SubmitJob {
            job_id,
            command,
            created_at,
        } => Some(ProtoCommand {
            command_type: Some(CommandType::SubmitJob(SubmitJobCommand {
                job_id: job_id.to_string(),
                command: command.clone(),
                created_at_ms: created_at.timestamp_millis(),
            })),
        }),
        Command::UpdateJobStatus {
            job_id,
            status,
            executed_by,
            exit_code,
            completed_at,
        } => Some(ProtoCommand {
            command_type: Some(CommandType::UpdateJobStatus(UpdateJobStatusCommand {
                job_id: job_id.to_string(),
                status: internal_status_to_proto(status) as i32,
                executed_by: *executed_by,
                exit_code: *exit_code,
                completed_at_ms: completed_at.map(|dt| dt.timestamp_millis()),
            })),
        }),
        Command::RegisterWorker { worker_id } => Some(ProtoCommand {
            command_type: Some(CommandType::RegisterWorker(RegisterWorkerCommand {
                worker_id: *worker_id,
            })),
        }),
        Command::Noop => None,
    };

    ProtoLogEntry {
        term: entry.term,
        index: entry.index,
        command,
    }
}

/// Convert milliseconds since epoch to DateTime<Utc>
fn ms_to_datetime(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

fn proto_status_to_internal(status: crate::proto::JobStatus) -> JobStatus {
    match status {
        crate::proto::JobStatus::Pending => JobStatus::Pending,
        crate::proto::JobStatus::Running => JobStatus::Running,
        crate::proto::JobStatus::Completed => JobStatus::Completed,
        crate::proto::JobStatus::Failed => JobStatus::Failed,
        crate::proto::JobStatus::Unspecified => JobStatus::Pending,
    }
}

fn internal_status_to_proto(status: &JobStatus) -> crate::proto::JobStatus {
    match status {
        JobStatus::Pending => crate::proto::JobStatus::Pending,
        JobStatus::Running => crate::proto::JobStatus::Running,
        JobStatus::Completed => crate::proto::JobStatus::Completed,
        JobStatus::Failed => crate::proto::JobStatus::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{
        command::CommandType, Command as ProtoCommand, RegisterWorkerCommand, SubmitJobCommand,
        UpdateJobStatusCommand,
    };

    /// Test log_entry_to_proto for SubmitJob command.
    #[test]
    fn test_log_entry_to_proto_submit_job() {
        let job_id = Uuid::new_v4();
        let entry = LogEntry {
            term: 1,
            index: 5,
            command: Command::SubmitJob {
                job_id,
                command: "echo hello".to_string(),
                created_at: Utc::now(),
            },
        };

        let proto = log_entry_to_proto(&entry);

        assert_eq!(proto.term, 1);
        assert_eq!(proto.index, 5);
        assert!(proto.command.is_some());

        if let Some(cmd) = proto.command {
            if let Some(CommandType::SubmitJob(submit)) = cmd.command_type {
                assert_eq!(submit.job_id, job_id.to_string());
                assert_eq!(submit.command, "echo hello");
            } else {
                panic!("Expected SubmitJob command type");
            }
        }
    }

    /// Test log_entry_to_proto for UpdateJobStatus command.
    #[test]
    fn test_log_entry_to_proto_update_job_status() {
        let job_id = Uuid::new_v4();
        let entry = LogEntry {
            term: 2,
            index: 10,
            command: Command::UpdateJobStatus {
                job_id,
                status: JobStatus::Completed,
                executed_by: 3,
                exit_code: Some(0),
                completed_at: Some(Utc::now()),
            },
        };

        let proto = log_entry_to_proto(&entry);

        assert_eq!(proto.term, 2);
        assert_eq!(proto.index, 10);

        if let Some(cmd) = proto.command {
            if let Some(CommandType::UpdateJobStatus(update)) = cmd.command_type {
                assert_eq!(update.job_id, job_id.to_string());
                assert_eq!(update.status, crate::proto::JobStatus::Completed as i32);
                assert_eq!(update.executed_by, 3);
                assert_eq!(update.exit_code, Some(0));
            } else {
                panic!("Expected UpdateJobStatus command type");
            }
        }
    }

    /// Test log_entry_to_proto for UpdateJobStatus with None exit_code.
    #[test]
    fn test_log_entry_to_proto_update_job_status_none_exit_code() {
        let job_id = Uuid::new_v4();
        let entry = LogEntry {
            term: 1,
            index: 1,
            command: Command::UpdateJobStatus {
                job_id,
                status: JobStatus::Failed,
                executed_by: 1,
                exit_code: None,
                completed_at: Some(Utc::now()),
            },
        };

        let proto = log_entry_to_proto(&entry);

        if let Some(cmd) = proto.command {
            if let Some(CommandType::UpdateJobStatus(update)) = cmd.command_type {
                assert_eq!(update.exit_code, None);
            } else {
                panic!("Expected UpdateJobStatus command type");
            }
        }
    }

    /// Test log_entry_to_proto for RegisterWorker command.
    #[test]
    fn test_log_entry_to_proto_register_worker() {
        let entry = LogEntry {
            term: 1,
            index: 3,
            command: Command::RegisterWorker { worker_id: 42 },
        };

        let proto = log_entry_to_proto(&entry);

        if let Some(cmd) = proto.command {
            if let Some(CommandType::RegisterWorker(register)) = cmd.command_type {
                assert_eq!(register.worker_id, 42);
            } else {
                panic!("Expected RegisterWorker command type");
            }
        }
    }

    /// Test log_entry_to_proto for Noop command.
    #[test]
    fn test_log_entry_to_proto_noop() {
        let entry = LogEntry {
            term: 1,
            index: 1,
            command: Command::Noop,
        };

        let proto = log_entry_to_proto(&entry);
        assert!(proto.command.is_none());
    }

    /// Test proto_to_log_entry for SubmitJob command.
    #[test]
    fn test_proto_to_log_entry_submit_job() {
        let job_id = Uuid::new_v4();
        let proto = ProtoLogEntry {
            term: 1,
            index: 5,
            command: Some(ProtoCommand {
                command_type: Some(CommandType::SubmitJob(SubmitJobCommand {
                    job_id: job_id.to_string(),
                    command: "echo test".to_string(),
                    created_at_ms: 1000,
                })),
            }),
        };

        let entry = proto_to_log_entry(&proto).unwrap();

        assert_eq!(entry.term, 1);
        assert_eq!(entry.index, 5);
        if let Command::SubmitJob {
            job_id: parsed_id,
            command,
            ..
        } = entry.command
        {
            assert_eq!(parsed_id, job_id);
            assert_eq!(command, "echo test");
        } else {
            panic!("Expected SubmitJob command");
        }
    }

    /// Test proto_to_log_entry for UpdateJobStatus command.
    #[test]
    fn test_proto_to_log_entry_update_job_status() {
        let job_id = Uuid::new_v4();
        let proto = ProtoLogEntry {
            term: 2,
            index: 10,
            command: Some(ProtoCommand {
                command_type: Some(CommandType::UpdateJobStatus(UpdateJobStatusCommand {
                    job_id: job_id.to_string(),
                    status: crate::proto::JobStatus::Completed as i32,
                    executed_by: 3,
                    exit_code: Some(0),
                    completed_at_ms: Some(2000),
                })),
            }),
        };

        let entry = proto_to_log_entry(&proto).unwrap();

        if let Command::UpdateJobStatus {
            job_id: parsed_id,
            status,
            executed_by,
            exit_code,
            ..
        } = entry.command
        {
            assert_eq!(parsed_id, job_id);
            assert_eq!(status, JobStatus::Completed);
            assert_eq!(executed_by, 3);
            assert_eq!(exit_code, Some(0));
        } else {
            panic!("Expected UpdateJobStatus command");
        }
    }

    /// Test proto_to_log_entry for RegisterWorker command.
    #[test]
    fn test_proto_to_log_entry_register_worker() {
        let proto = ProtoLogEntry {
            term: 1,
            index: 3,
            command: Some(ProtoCommand {
                command_type: Some(CommandType::RegisterWorker(RegisterWorkerCommand {
                    worker_id: 42,
                })),
            }),
        };

        let entry = proto_to_log_entry(&proto).unwrap();

        if let Command::RegisterWorker { worker_id } = entry.command {
            assert_eq!(worker_id, 42);
        } else {
            panic!("Expected RegisterWorker command");
        }
    }

    /// Test proto_to_log_entry for Noop (no command).
    #[test]
    fn test_proto_to_log_entry_noop() {
        let proto = ProtoLogEntry {
            term: 1,
            index: 1,
            command: None,
        };

        let entry = proto_to_log_entry(&proto).unwrap();
        assert!(matches!(entry.command, Command::Noop));
    }

    /// Test roundtrip conversion for SubmitJob.
    #[test]
    fn test_roundtrip_submit_job() {
        let job_id = Uuid::new_v4();
        let original = LogEntry {
            term: 3,
            index: 15,
            command: Command::SubmitJob {
                job_id,
                command: "echo roundtrip".to_string(),
                created_at: Utc::now(),
            },
        };

        let proto = log_entry_to_proto(&original);
        let recovered = proto_to_log_entry(&proto).unwrap();

        assert_eq!(recovered.term, original.term);
        assert_eq!(recovered.index, original.index);
        if let (
            Command::SubmitJob {
                job_id: orig_id,
                command: orig_cmd,
                ..
            },
            Command::SubmitJob {
                job_id: rec_id,
                command: rec_cmd,
                ..
            },
        ) = (&original.command, &recovered.command)
        {
            assert_eq!(orig_id, rec_id);
            assert_eq!(orig_cmd, rec_cmd);
        } else {
            panic!("Commands don't match");
        }
    }

    /// Test roundtrip conversion for UpdateJobStatus.
    #[test]
    fn test_roundtrip_update_job_status() {
        let job_id = Uuid::new_v4();
        let original = LogEntry {
            term: 4,
            index: 20,
            command: Command::UpdateJobStatus {
                job_id,
                status: JobStatus::Failed,
                executed_by: 5,
                exit_code: Some(1),
                completed_at: Some(Utc::now()),
            },
        };

        let proto = log_entry_to_proto(&original);
        let recovered = proto_to_log_entry(&proto).unwrap();

        if let (
            Command::UpdateJobStatus {
                job_id: orig_id,
                status: orig_status,
                executed_by: orig_exec,
                exit_code: orig_exit,
                ..
            },
            Command::UpdateJobStatus {
                job_id: rec_id,
                status: rec_status,
                executed_by: rec_exec,
                exit_code: rec_exit,
                ..
            },
        ) = (&original.command, &recovered.command)
        {
            assert_eq!(orig_id, rec_id);
            assert_eq!(orig_status, rec_status);
            assert_eq!(orig_exec, rec_exec);
            assert_eq!(orig_exit, rec_exit);
        } else {
            panic!("Commands don't match");
        }
    }

    /// Test status conversion functions.
    #[test]
    fn test_status_conversion_roundtrip() {
        let statuses = [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Completed,
            JobStatus::Failed,
        ];

        for status in statuses {
            let proto = internal_status_to_proto(&status);
            let recovered = proto_status_to_internal(proto);
            assert_eq!(status, recovered);
        }
    }
}
