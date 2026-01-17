use crate::proto::{
    AppendEntriesRequest, AppendEntriesResponse, LogEntry as ProtoLogEntry, VoteRequest,
    VoteResponse,
};
use crate::raft::state::{Command, LogEntry, RaftState};
use crate::scheduler::JobStatus;
use uuid::Uuid;

/// Handle RequestVote RPC
pub fn handle_request_vote(state: &mut RaftState, req: &VoteRequest, my_id: u64) -> VoteResponse {
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

    VoteResponse {
        term: state.current_term,
        vote_granted,
    }
}

/// Handle AppendEntries RPC
pub fn handle_append_entries(
    state: &mut RaftState,
    req: &AppendEntriesRequest,
    my_id: u64,
) -> AppendEntriesResponse {
    // If request term is greater, update our term and become follower
    if req.term > state.current_term {
        state.become_follower(req.term);
    }

    // Reject if request term is less than our current term
    if req.term < state.current_term {
        return AppendEntriesResponse {
            term: state.current_term,
            success: false,
            match_index: state.last_log_index(),
        };
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
                return AppendEntriesResponse {
                    term: state.current_term,
                    success: false,
                    match_index: state.last_log_index(),
                };
            }
            Some(entry) => {
                if entry.term != req.prev_log_term {
                    // Term mismatch - truncate and reject
                    state.log.truncate((req.prev_log_index - 1) as usize);
                    return AppendEntriesResponse {
                        term: state.current_term,
                        success: false,
                        match_index: state.last_log_index(),
                    };
                }
            }
        }
    }

    // Append new entries (if any)
    if !req.entries.is_empty() {
        let new_entries: Vec<LogEntry> = req
            .entries
            .iter()
            .filter_map(|e| proto_to_log_entry(e))
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

    AppendEntriesResponse {
        term: state.current_term,
        success: true,
        match_index: state.last_log_index(),
    }
}

/// Convert protobuf LogEntry to internal LogEntry
fn proto_to_log_entry(proto: &ProtoLogEntry) -> Option<LogEntry> {
    let command = match &proto.command {
        Some(cmd) => match &cmd.command_type {
            Some(crate::proto::command::CommandType::SubmitJob(submit)) => Command::SubmitJob {
                job_id: Uuid::parse_str(&submit.job_id).ok()?,
                command: submit.command.clone(),
            },
            Some(crate::proto::command::CommandType::UpdateJobStatus(update)) => {
                Command::UpdateJobStatus {
                    job_id: Uuid::parse_str(&update.job_id).ok()?,
                    status: proto_status_to_internal(update.status()),
                    output: if update.output.is_empty() {
                        None
                    } else {
                        Some(update.output.clone())
                    },
                    error: if update.error.is_empty() {
                        None
                    } else {
                        Some(update.error.clone())
                    },
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
        Command::SubmitJob { job_id, command } => Some(ProtoCommand {
            command_type: Some(CommandType::SubmitJob(SubmitJobCommand {
                job_id: job_id.to_string(),
                command: command.clone(),
            })),
        }),
        Command::UpdateJobStatus {
            job_id,
            status,
            output,
            error,
        } => Some(ProtoCommand {
            command_type: Some(CommandType::UpdateJobStatus(UpdateJobStatusCommand {
                job_id: job_id.to_string(),
                status: internal_status_to_proto(status) as i32,
                output: output.clone().unwrap_or_default(),
                error: error.clone().unwrap_or_default(),
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
