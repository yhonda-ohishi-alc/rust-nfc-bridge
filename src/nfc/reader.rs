use std::sync::Arc;
use std::time::{Duration, Instant};

use pcsc::*;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::error::BridgeError;
use crate::events::NfcEvent;
use crate::nfc::license;

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

/// Result of a single card poll.
pub enum CardReadResult {
    /// Simple UID read (fallback).
    Uid(String),
    /// Full license data read.
    License(license::LicenseData),
}

impl CardReadResult {
    fn debounce_key(&self) -> &str {
        match self {
            CardReadResult::Uid(uid) => uid,
            CardReadResult::License(data) => &data.card_id,
        }
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

/// Combined poll cycle: list readers + check card presence + optionally read.
/// Returns (readers, card_present, optional read result).
/// When `skip_read` is true, only checks card presence without connecting.
fn poll_cycle(
    ctx: &Context,
    skip_read: bool,
) -> Result<(Vec<String>, bool, Option<CardReadResult>), BridgeError> {
    let mut readers_buf = [0u8; 2048];
    let reader_names: Vec<_> = match ctx.list_readers(&mut readers_buf) {
        Ok(readers) => readers.collect(),
        Err(pcsc::Error::NoReadersAvailable) => return Ok((vec![], false, None)),
        Err(e) => return Err(BridgeError::Pcsc(e)),
    };

    let readers: Vec<String> = reader_names
        .iter()
        .map(|r| r.to_str().unwrap_or("unknown").to_string())
        .collect();

    if reader_names.is_empty() {
        return Ok((readers, false, None));
    }

    let reader_name = reader_names[0];
    let mut reader_states = vec![ReaderState::new(reader_name, State::UNAWARE)];

    match ctx.get_status_change(Duration::from_millis(200), &mut reader_states) {
        Ok(()) => {}
        Err(pcsc::Error::Timeout) => return Ok((readers, false, None)),
        Err(e) => return Err(BridgeError::Pcsc(e)),
    }

    let state = reader_states[0].event_state();
    if !state.contains(State::PRESENT) {
        return Ok((readers, false, None));
    }

    // Card is present
    if skip_read {
        return Ok((readers, true, None));
    }

    let atr_bytes = reader_states[0].atr().to_vec();
    let card = ctx.connect(reader_name, ShareMode::Shared, Protocols::ANY)?;

    let result = match license::read_card(&card, &atr_bytes) {
        Ok(data) => {
            debug!(
                "Card read: card_id={}, type={}",
                data.card_id,
                data.card_type.as_str()
            );
            Some(CardReadResult::License(data))
        }
        Err(e) => {
            warn!("License read failed ({}), falling back to UID", e);
            match read_uid(&card) {
                Ok(uid) => {
                    debug!("Fallback UID: {}", uid);
                    Some(CardReadResult::Uid(uid))
                }
                Err(_) => None,
            }
        }
    };

    // Leak the card handle to prevent SCardDisconnect from being called.
    // SCardDisconnect causes the NFC reader to reset, triggering Windows USB notification sound.
    std::mem::forget(card);

    Ok((readers, true, result))
}

/// Main NFC polling loop. Runs indefinitely, sending events via broadcast channel.
pub async fn nfc_polling_loop(config: Config, event_tx: broadcast::Sender<NfcEvent>) {
    let mut debouncer = CardDebouncer::new(Duration::from_millis(config.cooldown_ms));
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut last_no_readers_log = Instant::now() - Duration::from_secs(10);
    let mut previous_readers: Vec<String> = vec![];
    let mut ctx: Option<Arc<Context>> = None;
    let mut card_read_done = false;

    loop {
        if ctx.is_none() {
            match Context::establish(Scope::User) {
                Ok(c) => ctx = Some(Arc::new(c)),
                Err(e) => {
                    warn!("Failed to establish PCSC context: {}", e);
                    tokio::time::sleep(poll_interval).await;
                    continue;
                }
            }
        }

        let ctx_ref = ctx.as_ref().unwrap().clone();
        let skip = card_read_done;
        let cycle_result =
            tokio::task::spawn_blocking(move || poll_cycle(&ctx_ref, skip)).await;

        match cycle_result {
            Ok(Ok((readers, card_present, card_result))) => {
                // Card removed — allow next read
                if !card_present {
                    card_read_done = false;
                }

                // Hot-plug detection
                if readers != previous_readers {
                    info!(
                        "NFC readers changed: {:?} -> {:?}",
                        previous_readers, readers
                    );
                    let _ = event_tx.send(NfcEvent::Status {
                        readers: readers.clone(),
                        connected: !readers.is_empty(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                    });
                    previous_readers = readers;
                }

                // No readers warning (throttled)
                if previous_readers.is_empty()
                    && last_no_readers_log.elapsed() > Duration::from_secs(5)
                {
                    warn!("No NFC readers found, retrying...");
                    let _ = event_tx.send(NfcEvent::NfcError {
                        error: "no_readers".to_string(),
                    });
                    last_no_readers_log = Instant::now();
                }

                // Handle card read
                if let Some(result) = card_result {
                    card_read_done = true;
                    if debouncer.should_emit(result.debounce_key()) {
                        let event = match result {
                            CardReadResult::Uid(uid) => {
                                info!("NFC read (UID): {}", uid);
                                NfcEvent::NfcRead { employee_id: uid }
                            }
                            CardReadResult::License(data) => {
                                info!(
                                    "NFC read ({}): card_id={}, remain={:?}",
                                    data.card_type.as_str(),
                                    data.card_id,
                                    data.remain_count
                                );
                                NfcEvent::NfcLicenseRead {
                                    card_id: data.card_id,
                                    card_type: data.card_type.as_str().to_string(),
                                    atr: data.atr,
                                    expiry_date: data.expiry_date,
                                    remain_count: data.remain_count,
                                    felica_uid: data.felica_uid,
                                }
                            }
                        };
                        let _ = event_tx.send(event);
                    }
                }
            }
            Ok(Err(e)) => {
                warn!("NFC poll error: {}, will re-establish context", e);
                ctx = None;
                card_read_done = false;
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
