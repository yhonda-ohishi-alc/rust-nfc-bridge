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
/// Returns (readers, card_present, optional read result, optional card handle).
/// When `skip_read` is true, only checks card presence without connecting.
/// Card handle is returned to allow disconnect on card removal event.
#[allow(clippy::type_complexity)]
fn poll_cycle(
    ctx: &Context,
    skip_read: bool,
) -> Result<(Vec<String>, bool, Option<CardReadResult>, Option<pcsc::Card>), BridgeError> {
    let mut readers_buf = [0u8; 2048];
    let reader_names: Vec<_> = match ctx.list_readers(&mut readers_buf) {
        Ok(readers) => readers.collect(),
        Err(pcsc::Error::NoReadersAvailable) => return Ok((vec![], false, None, None)),
        Err(e) => return Err(BridgeError::Pcsc(e)),
    };

    let readers: Vec<String> = reader_names
        .iter()
        .map(|r| r.to_str().unwrap_or("unknown").to_string())
        .collect();

    if reader_names.is_empty() {
        return Ok((readers, false, None, None));
    }

    let reader_name = reader_names[0];
    let mut reader_states = vec![ReaderState::new(reader_name, State::UNAWARE)];

    match ctx.get_status_change(Duration::from_millis(200), &mut reader_states) {
        Ok(()) => {}
        Err(pcsc::Error::Timeout) => return Ok((readers, false, None, None)),
        Err(e) => return Err(BridgeError::Pcsc(e)),
    }

    let state = reader_states[0].event_state();
    if !state.contains(State::PRESENT) {
        return Ok((readers, false, None, None));
    }

    // Card is present
    if skip_read {
        return Ok((readers, true, None, None));
    }

    let atr_bytes = reader_states[0].atr().to_vec();
    let mut card = ctx.connect(reader_name, ShareMode::Shared, Protocols::ANY)?;

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

    // ============================================================================
    // USB 切断音問題の試行錯誤履歴 (Firmware Ver.1.08)
    // ============================================================================
    // ❌ 試行1: CMD_SELECT_END + Disposition::LeaveCard
    //    → USB切断音が鳴る
    //
    // ❌ 試行2: CMD_SELECT_END + Disposition::UnpowerCard (Python pyscard default)
    //    → USB切断音が鳴る
    //
    // ❌ 試行3: CMD_SELECT_ENDスキップ + Disposition::UnpowerCard
    //    → USB切断音が鳴る
    //
    // ❌ 試行4: CMD_SELECT_ENDスキップ + mem::forget() (disconnect呼ばない)
    //    → USB切断音がまだ鳴る！
    //
    // ❌ 試行5: CMD_SELECT_END復活 + mem::forget()
    //    → USB切断音がまだ鳴る
    //
    // ❌ 試行6: CMD_RF_OFF のみ、CMD_SELECT_ENDスキップ + mem::forget()
    //    → USB切断音がまだ鳴る
    //
    // ❌ 試行7-8: SCardControl(Auto Polling無効化/Buzzer無効化) + mem::forget()
    //    → カード検出自体ができなくなった
    //
    // ❌ 試行9: CMD_SELECT_END復活 + Disposition::LeaveCard (正常disconnect)
    //    → USB切断音がまだ鳴る
    //
    // ❌ 試行10: printobserver.pyアプローチ（disconnect後にfalse返してカード削除扱い）
    //    → USB切断音がまだ鳴る
    //
    // ❌ 試行11: カード読み取り直後に reconnect を試みる
    //    → USB切断音がまだ鳴る
    //
    // ❌ 試行12: 完全にクリーンアップをスキップ（mem::forget() でリーク）
    //    → USB切断音がまだ鳴る
    //
    // ❌ 試行13: printobserver.py の disconnect タイミングを完全再現
    //    カード読み取り直後は disconnect せず、カード削除検出時のみ disconnect
    //    → USB切断音がまだ鳴る（printobserver.py でも同様に鳴ることが判明）
    //
    // 📝 試行14 (現在): SCardReconnect を使用
    //    printobserver.py 解析結果:
    //    - カード追加イベントで CMD_SELECT_END 送信 + disconnect (Line 152, 157)
    //    - カード削除イベントで再度 disconnect (Line 164)
    //    - CardMonitor が別スレッドで監視（非同期処理）
    //
    //    修正内容:
    //    - カード読み取り直後は disconnect を呼ばず、カードハンドルを返す
    //    - カード削除検出時（次のポーリングサイクルでカード不在）のみ disconnect
    //    - CMD_SELECT_END は送信する（printobserver.py と同じ）
    //
    //    仮説: カード読み取り**直後**の disconnect が USB 切断音の原因
    //          カードが**物理的に離れた後**に disconnect すれば鳴らないはず
    // ============================================================================

    // 試行14: SCardReconnect を使用
    // disconnect の代わりに reconnect を使うことで、USB 切断を防ぐ
    // reconnect は接続を維持したままカードをリセットする
    // 結果: USB切断音はまだ鳴る（reconnect でも同じ）
    info!("[reader] Card read completed, waiting before reconnect");

    // Wait 100ms before reconnect to avoid immediate reconnection issue
    std::thread::sleep(std::time::Duration::from_millis(100));

    info!("[reader] Attempting reconnect instead of disconnect");

    match card.reconnect(ShareMode::Shared, Protocols::ANY, Disposition::ResetCard) {
        Ok(_) => {
            info!("[reader] Successfully reconnected to card");
            // reconnect 成功後、カードを drop して disconnect
            std::mem::drop(card);
            info!("[reader] Card dropped after reconnect");
        }
        Err(e) => {
            warn!("[reader] Failed to reconnect: {}, dropping card", e);
            std::mem::drop(card);
        }
    }

    // カードハンドルは返さない（既に drop 済み）
    Ok((readers, true, result, None))
}

/// Main NFC polling loop. Runs indefinitely, sending events via broadcast channel.
pub async fn nfc_polling_loop(config: Config, event_tx: broadcast::Sender<NfcEvent>) {
    let mut debouncer = CardDebouncer::new(Duration::from_millis(config.cooldown_ms));
    let poll_interval = Duration::from_millis(config.poll_interval_ms);
    let mut last_no_readers_log = Instant::now() - Duration::from_secs(10);
    let mut previous_readers: Vec<String> = vec![];
    let mut ctx: Option<Arc<Context>> = None;
    let mut card_read_done = false;
    let mut active_card: Option<pcsc::Card> = None; // Keep card handle for removal event

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
        let cycle_result = tokio::task::spawn_blocking(move || poll_cycle(&ctx_ref, skip)).await;

        match cycle_result {
            Ok(Ok((readers, card_present, card_result, card_handle))) => {
                // Card removed event - disconnect if we have an active card
                if !card_present && active_card.is_some() {
                    info!("[reader] Card removal detected, disconnecting...");
                    if let Some(card) = active_card.take() {
                        match card.disconnect(Disposition::LeaveCard) {
                            Ok(_) => info!("[reader] Card disconnected on removal event"),
                            Err((_, e)) => warn!("[reader] Failed to disconnect on removal: {}", e),
                        }
                    }
                    card_read_done = false;
                } else if !card_present {
                    card_read_done = false;
                }

                // Store card handle if we just read a card
                if card_handle.is_some() {
                    active_card = card_handle;
                    card_read_done = true; // Prevent re-reading the same card
                } else if card_result.is_some() {
                    // Card was read successfully but handle is None (Trial 14: reconnect+drop)
                    // Still need to mark as done to prevent re-reading the same card
                    card_read_done = true;
                }

                // Hot-plug detection
                if readers != previous_readers {
                    info!(
                        "NFC readers changed: {:?} -> {:?}",
                        previous_readers, readers
                    );

                    // Reset reader settings when a reader is detected
                    if !readers.is_empty() && previous_readers.is_empty() {
                        let ctx_clone = ctx.as_ref().unwrap().clone();
                        let reader_name = std::ffi::CString::new(readers[0].as_str()).unwrap();
                        tokio::task::spawn_blocking(move || {
                            match ctx_clone.connect(&reader_name, ShareMode::Direct, Protocols::UNDEFINED) {
                                Ok(card) => {
                                    let control_code = pcsc::ctl_code(3500);
                                    let enable_auto_polling = [
                                        0xFF, 0x00, 0x40, // Escape Command: Set Polling Parameter
                                        0x01, 0x01, 0x01, // Enable Auto Polling
                                    ];
                                    let mut recv_buf = [0u8; 256];
                                    match card.control(control_code, &enable_auto_polling, &mut recv_buf) {
                                        Ok(_) => info!("[reader] Reader settings reset: Auto Polling enabled"),
                                        Err(e) => warn!("[reader] Failed to reset reader settings: {}", e),
                                    }
                                    let _ = card.disconnect(Disposition::LeaveCard);
                                }
                                Err(e) => warn!("[reader] Failed to connect for settings reset: {}", e),
                            }
                        });
                    }

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
