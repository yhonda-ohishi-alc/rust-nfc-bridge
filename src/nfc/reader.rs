use std::time::{Duration, Instant};

use pcsc::*;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::BridgeError;
use crate::events::NfcEvent;

/// GET DATA APDU command to read card UID.
/// Works for both MIFARE (4/7 bytes) and FeliCa (8 bytes IDm).
const GET_UID_APDU: &[u8] = &[0xFF, 0xCA, 0x00, 0x00, 0x00];

/// APDU success status word.
const SW_SUCCESS: [u8; 2] = [0x90, 0x00];

/// Tracks recently seen cards for debouncing.
pub struct CardDebouncer {
    last_uid: Option<String>,
    last_seen: Option<Instant>,
    cooldown: Duration,
}

impl CardDebouncer {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            last_uid: None,
            last_seen: None,
            cooldown,
        }
    }

    /// Returns true if this UID should be emitted (not a duplicate within cooldown).
    pub fn should_emit(&mut self, uid: &str) -> bool {
        let now = Instant::now();
        if let (Some(last_uid), Some(last_seen)) = (&self.last_uid, self.last_seen) {
            if last_uid == uid && now.duration_since(last_seen) < self.cooldown {
                return false;
            }
        }
        self.last_uid = Some(uid.to_string());
        self.last_seen = Some(now);
        true
    }
}

/// List available NFC readers (blocking, call from sync context or spawn_blocking).
pub fn list_readers_sync() -> Result<Vec<String>, BridgeError> {
    let ctx = Context::establish(Scope::User)?;
    let mut readers_buf = [0u8; 2048];
    let readers = ctx.list_readers(&mut readers_buf)?;
    Ok(readers
        .map(|r| r.to_str().unwrap_or("unknown").to_string())
        .collect())
}

/// Read card UID using GET DATA APDU command.
fn read_uid(card: &Card) -> Result<String, BridgeError> {
    let mut response_buf = [0u8; 256];
    let response = card
        .transmit(GET_UID_APDU, &mut response_buf)
        .map_err(|e| BridgeError::CardReadFailed(format!("transmit failed: {}", e)))?;

    if response.len() < 2 {
        return Err(BridgeError::CardReadFailed("response too short".into()));
    }

    let sw1 = response[response.len() - 2];
    let sw2 = response[response.len() - 1];

    if [sw1, sw2] != SW_SUCCESS {
        return Err(BridgeError::CardReadFailed(format!(
            "APDU error: SW={:02X}{:02X}",
            sw1, sw2
        )));
    }

    // UID is everything before SW1/SW2
    let uid_bytes = &response[..response.len() - 2];
    let uid_hex = uid_bytes
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<String>();

    Ok(uid_hex)
}

/// Poll for a card on the first available reader and read its UID.
/// This is a blocking function — must be called via spawn_blocking.
fn poll_once() -> Result<Option<String>, BridgeError> {
    let ctx = Context::establish(Scope::User)?;
    let mut readers_buf = [0u8; 2048];
    let reader_names: Vec<_> = ctx
        .list_readers(&mut readers_buf)?
        .collect();

    if reader_names.is_empty() {
        return Err(BridgeError::NoReaders);
    }

    let reader_name = reader_names[0];

    let mut reader_states = vec![ReaderState::new(reader_name, State::UNAWARE)];

    // Wait up to 200ms for a card event
    match ctx.get_status_change(Duration::from_millis(200), &mut reader_states) {
        Ok(()) => {}
        Err(pcsc::Error::Timeout) => return Ok(None),
        Err(e) => return Err(BridgeError::Pcsc(e)),
    }

    let state = reader_states[0].event_state();
    if !state.contains(State::PRESENT) {
        return Ok(None);
    }

    // Card is present — connect and read UID
    let card = ctx.connect(
        reader_name,
        ShareMode::Shared,
        Protocols::ANY,
    )?;

    let uid = read_uid(&card)?;
    debug!("Card UID: {}", uid);
    Ok(Some(uid))
}

/// Main NFC polling loop. Runs indefinitely, sending events via broadcast channel.
pub async fn nfc_polling_loop(config: Config, event_tx: broadcast::Sender<NfcEvent>) {
    let mut debouncer = CardDebouncer::new(Duration::from_millis(config.cooldown_ms));
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut last_no_readers_log = Instant::now() - Duration::from_secs(10);

    loop {
        let poll_result = tokio::task::spawn_blocking(poll_once).await;

        match poll_result {
            Ok(Ok(Some(uid))) => {
                if debouncer.should_emit(&uid) {
                    info!("NFC read: {}", uid);
                    let _ = event_tx.send(NfcEvent::NfcRead {
                        employee_id: uid,
                    });
                }
            }
            Ok(Ok(None)) => {
                // No card present, continue polling
            }
            Ok(Err(BridgeError::NoReaders)) => {
                // Only log periodically to avoid spam
                if last_no_readers_log.elapsed() > Duration::from_secs(5) {
                    warn!("No NFC readers found, retrying...");
                    let _ = event_tx.send(NfcEvent::NfcError {
                        error: "no_readers".to_string(),
                    });
                    last_no_readers_log = Instant::now();
                }
            }
            Ok(Err(e)) => {
                warn!("NFC poll error: {}", e);
                let _ = event_tx.send(NfcEvent::NfcError {
                    error: e.to_string(),
                });
            }
            Err(e) => {
                warn!("NFC polling task panicked: {}", e);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debouncer_allows_first_card() {
        let mut debouncer = CardDebouncer::new(Duration::from_secs(3));
        assert!(debouncer.should_emit("AABBCCDD"));
    }

    #[test]
    fn debouncer_blocks_same_card_within_cooldown() {
        let mut debouncer = CardDebouncer::new(Duration::from_secs(3));
        assert!(debouncer.should_emit("AABBCCDD"));
        assert!(!debouncer.should_emit("AABBCCDD"));
    }

    #[test]
    fn debouncer_allows_different_card() {
        let mut debouncer = CardDebouncer::new(Duration::from_secs(3));
        assert!(debouncer.should_emit("AABBCCDD"));
        assert!(debouncer.should_emit("11223344"));
    }

    #[test]
    fn debouncer_allows_same_card_after_cooldown() {
        let mut debouncer = CardDebouncer::new(Duration::from_millis(10));
        assert!(debouncer.should_emit("AABBCCDD"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(debouncer.should_emit("AABBCCDD"));
    }
}
