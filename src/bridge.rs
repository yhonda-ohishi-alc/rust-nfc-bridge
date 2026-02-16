use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::config::Config;
use crate::events::NfcEvent;
use crate::nfc;
use crate::ws;

/// Run the NFC bridge application.
/// Returns when the cancellation token is triggered.
pub async fn run(
    config: Config,
    shutdown: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting NFC Bridge: ws://{}", config.addr());

    let (event_tx, event_rx) = broadcast::channel::<NfcEvent>(64);

    // Log initial reader state
    match tokio::task::spawn_blocking(nfc::list_readers_sync).await? {
        Ok(readers) => {
            info!("Found NFC readers: {:?}", readers);
            let _ = event_tx.send(NfcEvent::Status {
                readers: readers.clone(),
                connected: !readers.is_empty(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            });
        }
        Err(e) => {
            info!(
                "No NFC readers found on startup: {}. Will keep retrying.",
                e
            );
        }
    }

    // Spawn NFC polling loop
    let poll_config = config.clone();
    let poll_tx = event_tx.clone();
    let poll_shutdown = shutdown.clone();
    let nfc_handle = tokio::spawn(async move {
        tokio::select! {
            _ = nfc::nfc_polling_loop(poll_config, poll_tx) => {}
            _ = poll_shutdown.cancelled() => {}
        }
    });

    // Spawn WebSocket server
    let ws_config = config.clone();
    let ws_shutdown = shutdown.clone();
    let ws_handle = tokio::spawn(async move {
        tokio::select! {
            result = ws::run_ws_server(&ws_config, event_rx) => {
                if let Err(e) = result {
                    tracing::error!("WebSocket server error: {}", e);
                }
            }
            _ = ws_shutdown.cancelled() => {}
        }
    });

    // Wait for shutdown signal
    shutdown.cancelled().await;
    info!("Shutting down NFC Bridge...");

    nfc_handle.abort();
    ws_handle.abort();

    info!("NFC Bridge stopped.");
    Ok(())
}
