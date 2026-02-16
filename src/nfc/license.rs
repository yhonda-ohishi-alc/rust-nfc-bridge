use pcsc::Card;
use tokio::sync::broadcast;
use tracing::info;

use crate::error::BridgeError;
use crate::events::NfcEvent;

// --- APDU command constants (ported from menkyo_go_ref/internal/nfc/license_reader.go) ---

/// Initialize NFC card reader session.
const CMD_START: &[u8] = &[0xFF, 0xC2, 0x00, 0x00, 0x01, 0x81];

/// Begin transaction.
const CMD_START_TRANS: &[u8] = &[0xFF, 0xC2, 0x00, 0x00, 0x02, 0x84, 0x00];

/// Check for car inspection certificate (車検証チェック).
const CMD_CHECK_SHAKEN: &[u8] = &[0xFF, 0xCA, 0x01, 0x00, 0x00];

/// Get FeliCa IDm.
const CMD_GET_FELICA_IDM: &[u8] = &[0xFF, 0xCA, 0x00, 0x00, 0x00];

/// Select Master File (ISO7816-4).
const CMD_SELECT_MF: &[u8] = &[0x00, 0xA4, 0x00, 0x00];

/// Check remaining read count (PIN verify without data).
const CMD_CHECK_REMAIN: &[u8] = &[0x00, 0x20, 0x00, 0x81];

/// Select expiry date file (EF 2F01 under MF).
const CMD_SELECT_EXPIRE_MF: &[u8] = &[0x00, 0xA4, 0x02, 0x0C, 0x02, 0x2F, 0x01];

/// Read expiry date data (17 bytes).
const CMD_READ_EXPIRE_DF: &[u8] = &[0x00, 0xB0, 0x00, 0x00, 0x11];

/// End session.
const CMD_SELECT_END: &[u8] = &[0xFF, 0xC2, 0x00, 0x00, 0x02, 0x82, 0x00];

/// Driver's license ATR prefix (7 bytes).
const DRIVER_LICENSE_ATR_PREFIX: &[u8] = &[0x3B, 0x88, 0x80, 0x01, 0x00, 0x00, 0x00];

/// Car inspection response signature (6 bytes).
const SHAKEN_SIGNATURE: &[u8] = &[0x06, 0x78, 0x77, 0x81, 0x02, 0x80];

/// Card types detected from NFC.
#[derive(Debug, Clone, PartialEq)]
pub enum CardType {
    DriverLicense,
    CarInspection,
    Other,
}

impl CardType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CardType::DriverLicense => "driver_license",
            CardType::CarInspection => "car_inspection",
            CardType::Other => "other",
        }
    }
}

/// Data extracted from a card via NFC.
#[derive(Debug, Clone)]
pub struct LicenseData {
    pub card_id: String,
    pub card_type: CardType,
    pub atr: String,
    pub expiry_date: Option<String>,
    pub remain_count: Option<u8>,
    pub felica_uid: Option<String>,
}

/// Send an APDU command and return (response_data, sw1, sw2).
fn transmit_apdu(card: &Card, apdu: &[u8]) -> Result<(Vec<u8>, u8, u8), BridgeError> {
    let mut response_buf = [0u8; 258];
    let response = card
        .transmit(apdu, &mut response_buf)
        .map_err(|e| BridgeError::CardReadFailed(format!("transmit failed: {}", e)))?;

    if response.len() < 2 {
        return Err(BridgeError::CardReadFailed("response too short".into()));
    }

    let sw1 = response[response.len() - 2];
    let sw2 = response[response.len() - 1];
    let data = response[..response.len() - 2].to_vec();

    Ok((data, sw1, sw2))
}

/// Convert byte slice to uppercase hex string.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02X}", b)).collect()
}

/// Detect card type from ATR prefix and APDU probing.
fn detect_card_type(card: &Card, atr: &[u8]) -> CardType {
    // Check car inspection certificate first
    if let Ok((resp, _sw1, _sw2)) = transmit_apdu(card, CMD_CHECK_SHAKEN) {
        if resp == SHAKEN_SIGNATURE {
            return CardType::CarInspection;
        }
    }

    // Check driver's license via ATR prefix
    if atr.len() >= DRIVER_LICENSE_ATR_PREFIX.len()
        && atr[..DRIVER_LICENSE_ATR_PREFIX.len()] == *DRIVER_LICENSE_ATR_PREFIX
    {
        return CardType::DriverLicense;
    }

    CardType::Other
}

/// Read driver's license specific data (MF select, remain count, expiry date).
fn read_driver_license_data(card: &Card) -> Result<(Option<String>, Option<u8>), BridgeError> {
    // SELECT MF
    transmit_apdu(card, CMD_SELECT_MF)?;

    // CHECK REMAIN — SW2 lower nibble = remaining count
    let remain_count = match transmit_apdu(card, CMD_CHECK_REMAIN) {
        Ok((_data, _sw1, sw2)) => Some(sw2 & 0x0F),
        Err(_) => None,
    };

    // SELECT expiry date file
    transmit_apdu(card, CMD_SELECT_EXPIRE_MF)?;

    // READ expiry date
    let expiry_date = match transmit_apdu(card, CMD_READ_EXPIRE_DF) {
        Ok((data, 0x90, 0x00)) => Some(to_hex(&data)),
        _ => None,
    };

    Ok((expiry_date, remain_count))
}

/// Send a debug log to the browser console via WebSocket.
fn debug_log(tx: &broadcast::Sender<NfcEvent>, msg: &str) {
    info!("{}", msg);
    let _ = tx.send(NfcEvent::NfcDebug {
        message: msg.to_string(),
    });
}

/// Read card data from a connected card.
/// `atr_bytes` is the raw ATR obtained from ReaderState.
/// `tx` is used to send real-time debug logs to the browser console.
pub fn read_card(
    card: &Card,
    atr_bytes: &[u8],
    tx: &broadcast::Sender<NfcEvent>,
) -> Result<LicenseData, BridgeError> {
    let atr_hex = to_hex(atr_bytes);
    debug_log(tx, &format!("[license] ATR: {}", atr_hex));

    // Step 1: Get FeliCa IDm (before START command)
    debug_log(tx, "[license] Step 1: GET_FELICA_IDM...");
    let felica_uid = match transmit_apdu(card, CMD_GET_FELICA_IDM) {
        Ok((data, sw1, sw2)) if sw1 == 0x90 && sw2 == 0x00 && data.len() >= 4 => {
            let len = std::cmp::min(data.len(), 8);
            let uid = to_hex(&data[..len]);
            debug_log(
                tx,
                &format!("[license] FeliCa IDm: {} ({}bytes)", uid, data.len()),
            );
            Some(uid)
        }
        Ok((_data, sw1, sw2)) => {
            debug_log(
                tx,
                &format!(
                    "[license] FeliCa IDm: not available (SW={:02X}{:02X})",
                    sw1, sw2
                ),
            );
            None
        }
        Err(e) => {
            debug_log(tx, &format!("[license] FeliCa IDm: error ({})", e));
            None
        }
    };

    // Step 2: Initialize session
    debug_log(tx, "[license] Step 2: START...");
    transmit_apdu(card, CMD_START)
        .map_err(|e| BridgeError::CardReadFailed(format!("START failed: {}", e)))?;
    debug_log(tx, "[license] Step 2: START done");

    debug_log(tx, "[license] Step 3: START_TRANS...");
    transmit_apdu(card, CMD_START_TRANS)
        .map_err(|e| BridgeError::CardReadFailed(format!("START_TRANS failed: {}", e)))?;
    debug_log(tx, "[license] Step 3: START_TRANS done");

    // Step 4: Detect card type
    debug_log(tx, "[license] Step 4: detect_card_type...");
    let card_type = detect_card_type(card, atr_bytes);
    debug_log(tx, &format!("[license] Card type: {}", card_type.as_str()));

    // Step 5: Read license-specific data if applicable
    let (expiry_date, remain_count) = if card_type == CardType::DriverLicense {
        debug_log(tx, "[license] Step 5: read_driver_license_data...");
        match read_driver_license_data(card) {
            Ok((expiry, remain)) => {
                debug_log(
                    tx,
                    &format!("[license] Expiry: {:?}, Remain: {:?}", expiry, remain),
                );
                (expiry, remain)
            }
            Err(e) => {
                debug_log(tx, &format!("[license] Failed to read license data: {}", e));
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Step 6: Generate card_id
    let card_id = if card_type == CardType::DriverLicense {
        expiry_date
            .clone()
            .unwrap_or_else(|| felica_uid.clone().unwrap_or_default())
    } else {
        felica_uid.clone().unwrap_or_default()
    };

    debug_log(tx, &format!("[license] card_id: {}", card_id));

    // Step 7: End session (best effort)
    debug_log(tx, "[license] Step 7: SELECT_END...");
    let _ = transmit_apdu(card, CMD_SELECT_END);
    debug_log(tx, "[license] Step 7: SELECT_END done");

    Ok(LicenseData {
        card_id,
        card_type,
        atr: atr_hex,
        expiry_date,
        remain_count,
        felica_uid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_type_as_str() {
        assert_eq!(CardType::DriverLicense.as_str(), "driver_license");
        assert_eq!(CardType::CarInspection.as_str(), "car_inspection");
        assert_eq!(CardType::Other.as_str(), "other");
    }

    #[test]
    fn to_hex_conversion() {
        assert_eq!(to_hex(&[0x3B, 0x88, 0x80]), "3B8880");
        assert_eq!(to_hex(&[0x00, 0xFF]), "00FF");
        assert_eq!(to_hex(&[]), "");
    }
}
