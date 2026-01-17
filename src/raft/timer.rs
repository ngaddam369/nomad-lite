use rand::Rng;
use std::time::Duration;
use tokio::time::{interval, Interval};

/// Generates a random election timeout within the configured range
pub fn random_election_timeout(min_ms: u64, max_ms: u64) -> Duration {
    let mut rng = rand::thread_rng();
    let timeout_ms = rng.gen_range(min_ms..=max_ms);
    Duration::from_millis(timeout_ms)
}

/// Creates a heartbeat interval
pub fn heartbeat_interval(interval_ms: u64) -> Interval {
    interval(Duration::from_millis(interval_ms))
}
