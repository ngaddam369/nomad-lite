use tokio::signal::unix::{signal, SignalKind};
use tokio_util::sync::CancellationToken;

/// Install a shutdown handler that listens for SIGTERM and SIGINT.
///
/// Returns a `CancellationToken` that is cancelled when either signal is received.
/// All subsystems should monitor this token and drain gracefully.
pub fn install_shutdown_handler() -> CancellationToken {
    let token = CancellationToken::new();
    let token_clone = token.clone();

    tokio::spawn(async move {
        let mut sigterm =
            signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM, initiating graceful shutdown");
            }
            _ = sigint.recv() => {
                tracing::info!("Received SIGINT, initiating graceful shutdown");
            }
        }

        token_clone.cancel();
    });

    token
}
