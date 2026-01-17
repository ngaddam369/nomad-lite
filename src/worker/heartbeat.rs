use std::time::Duration;
use tokio::sync::mpsc;

/// Heartbeat sender that periodically sends heartbeat signals
pub struct HeartbeatSender {
    interval: Duration,
}

impl HeartbeatSender {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval: Duration::from_millis(interval_ms),
        }
    }

    /// Run the heartbeat sender, sending to the provided channel
    pub async fn run(&self, tx: mpsc::Sender<()>) {
        let mut interval = tokio::time::interval(self.interval);

        loop {
            interval.tick().await;
            if tx.send(()).await.is_err() {
                // Receiver dropped, stop sending
                break;
            }
        }
    }
}
