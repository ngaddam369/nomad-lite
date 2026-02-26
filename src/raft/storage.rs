use std::path::Path;

use rocksdb::{Direction, IteratorMode, DB};

use crate::raft::state::{LogEntry, Snapshot};

/// Embedded RocksDB-backed persistence for Raft hard state and the log.
///
/// Key schema (all in the default column family):
///
/// | Key                           | Value                        |
/// |-------------------------------|------------------------------|
/// | `b"term"`                     | u64 big-endian               |
/// | `b"voted_for"`                | JSON `Option<u64>`           |
/// | `b"log_offset"`               | u64 big-endian               |
/// | `b"snapshot"`                 | JSON `Snapshot`              |
/// | `b"log\x00{index:08-be}"`     | JSON `LogEntry`              |
///
/// Big-endian log keys ensure lexicographic order == numeric order, enabling
/// efficient range scans and prefix-bounded iteration.
pub struct RaftStorage {
    db: DB,
}

/// State recovered from a persisted RocksDB directory.
pub struct PersistedState {
    pub current_term: u64,
    pub voted_for: Option<u64>,
    pub log_offset: u64,
    pub snapshot: Option<Snapshot>,
    pub log: Vec<LogEntry>,
}

/// Build the RocksDB key for a log entry at the given 1-based index.
fn log_key(index: u64) -> [u8; 12] {
    let mut key = [0u8; 12];
    key[..4].copy_from_slice(b"log\x00");
    key[4..].copy_from_slice(&index.to_be_bytes());
    key
}

const LOG_PREFIX: &[u8] = b"log\x00";

impl RaftStorage {
    /// Open (or create) the RocksDB at `data_dir`. Panics on failure — a node
    /// that cannot open its storage cannot provide durability guarantees.
    pub fn open(data_dir: &Path) -> Self {
        let db = DB::open_default(data_dir).unwrap_or_else(|e| {
            tracing::error!(path = %data_dir.display(), error = %e, "Failed to open RocksDB");
            panic!("RocksDB open failure: {}", e);
        });
        Self { db }
    }

    /// Persist the durable Raft hard state: current term, voted-for candidate,
    /// and the compaction offset. Called after every term/vote change.
    pub fn save_hard_state(&self, term: u64, voted_for: Option<u64>, log_offset: u64) {
        let voted_for_bytes = serde_json::to_vec(&voted_for).unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to serialize voted_for");
            panic!("Storage serialization failure: {}", e);
        });

        self.db
            .put(b"term", term.to_be_bytes())
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to save term");
                panic!("RocksDB write failure: {}", e);
            });
        self.db
            .put(b"voted_for", voted_for_bytes)
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to save voted_for");
                panic!("RocksDB write failure: {}", e);
            });
        self.db
            .put(b"log_offset", log_offset.to_be_bytes())
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to save log_offset");
                panic!("RocksDB write failure: {}", e);
            });
    }

    /// Append a single log entry. Called after each successful `log.push`.
    pub fn append_entry(&self, entry: &LogEntry) {
        let key = log_key(entry.index);
        let value = serde_json::to_vec(entry).unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to serialize log entry");
            panic!("Storage serialization failure: {}", e);
        });
        self.db.put(key, value).unwrap_or_else(|e| {
            tracing::error!(index = entry.index, error = %e, "Failed to save log entry");
            panic!("RocksDB write failure: {}", e);
        });
    }

    /// Delete all persisted log entries with index >= `from_index`. Called
    /// before appending on a term mismatch (follower log truncation).
    pub fn truncate_from(&self, from_index: u64) {
        let start_key = log_key(from_index);
        let keys_to_delete: Vec<Vec<u8>> = self
            .db
            .iterator(IteratorMode::From(&start_key, Direction::Forward))
            .map_while(|item| {
                let (k, _) = item.unwrap_or_else(|e| {
                    tracing::error!(error = %e, "Failed to iterate storage during truncation");
                    panic!("RocksDB iteration failure: {}", e);
                });
                if k.starts_with(LOG_PREFIX) {
                    Some(k.to_vec())
                } else {
                    None
                }
            })
            .collect();

        for key in keys_to_delete {
            self.db.delete(&key).unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to delete log entry during truncation");
                panic!("RocksDB delete failure: {}", e);
            });
        }
    }

    /// Persist a snapshot and delete all log entries covered by it
    /// (indices 1 through `log_offset`, inclusive). Also updates the stored
    /// `log_offset` so `load()` returns the correct value on restart.
    pub fn save_snapshot(&self, snapshot: &Snapshot, log_offset: u64) {
        let snapshot_bytes = serde_json::to_vec(snapshot).unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to serialize snapshot");
            panic!("Storage serialization failure: {}", e);
        });

        self.db
            .put(b"snapshot", snapshot_bytes)
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to save snapshot");
                panic!("RocksDB write failure: {}", e);
            });
        self.db
            .put(b"log_offset", log_offset.to_be_bytes())
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to save log_offset");
                panic!("RocksDB write failure: {}", e);
            });

        // Delete compacted log entries (indices 1..=log_offset)
        if log_offset > 0 {
            let start_key = log_key(1);
            // exclusive upper bound: first entry NOT to delete
            let end_key = log_key(log_offset + 1);

            let keys_to_delete: Vec<Vec<u8>> = self
                .db
                .iterator(IteratorMode::From(&start_key, Direction::Forward))
                .map_while(|item| {
                    let (k, _) = item.unwrap_or_else(|e| {
                        tracing::error!(
                            error = %e,
                            "Failed to iterate storage during snapshot compaction"
                        );
                        panic!("RocksDB iteration failure: {}", e);
                    });
                    if k.as_ref() < end_key.as_slice() && k.starts_with(LOG_PREFIX) {
                        Some(k.to_vec())
                    } else {
                        None
                    }
                })
                .collect();

            for key in keys_to_delete {
                self.db.delete(&key).unwrap_or_else(|e| {
                    tracing::error!(error = %e, "Failed to delete compacted log entry");
                    panic!("RocksDB delete failure: {}", e);
                });
            }
        }
    }

    /// Load persisted state. Returns `None` on a fresh (empty) database.
    /// Panics on corruption (malformed data) — crash-fail-safe.
    pub fn load(&self) -> Option<PersistedState> {
        // Use the presence of the "term" key as the freshness sentinel.
        let term_bytes = self.db.get(b"term").unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to read term from storage");
            panic!("RocksDB read failure: {}", e);
        })?; // None → fresh DB

        let term = u64::from_be_bytes(
            term_bytes
                .as_slice()
                .try_into()
                .unwrap_or_else(|_| panic!("Storage corruption: invalid term byte length")),
        );

        let voted_for: Option<u64> = match self.db.get(b"voted_for").unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to read voted_for from storage");
            panic!("RocksDB read failure: {}", e);
        }) {
            Some(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to deserialize voted_for");
                panic!("Storage deserialization failure: {}", e);
            }),
            None => None,
        };

        let log_offset = match self.db.get(b"log_offset").unwrap_or_else(|e| {
            tracing::error!(error = %e, "Failed to read log_offset from storage");
            panic!("RocksDB read failure: {}", e);
        }) {
            Some(bytes) => u64::from_be_bytes(
                bytes
                    .as_slice()
                    .try_into()
                    .unwrap_or_else(|_| panic!("Storage corruption: invalid log_offset")),
            ),
            None => 0,
        };

        let snapshot: Option<Snapshot> = self
            .db
            .get(b"snapshot")
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "Failed to read snapshot from storage");
                panic!("RocksDB read failure: {}", e);
            })
            .map(|bytes| {
                serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                    tracing::error!(error = %e, "Failed to deserialize snapshot");
                    panic!("Storage deserialization failure: {}", e);
                })
            });

        // Scan log entries in index order (big-endian keys sort correctly)
        let start_key = log_key(log_offset + 1);
        let log: Vec<LogEntry> = self
            .db
            .iterator(IteratorMode::From(&start_key, Direction::Forward))
            .map_while(|item| {
                let (k, v) = item.unwrap_or_else(|e| {
                    tracing::error!(error = %e, "Failed to iterate log entries during load");
                    panic!("RocksDB iteration failure: {}", e);
                });
                if !k.starts_with(LOG_PREFIX) {
                    return None;
                }
                Some(serde_json::from_slice(&v).unwrap_or_else(|e| {
                    tracing::error!(error = %e, "Failed to deserialize log entry");
                    panic!("Storage deserialization failure: {}", e);
                }))
            })
            .collect();

        Some(PersistedState {
            current_term: term,
            voted_for,
            log_offset,
            snapshot,
            log,
        })
    }
}
