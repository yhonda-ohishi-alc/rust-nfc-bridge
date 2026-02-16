use serde::Serialize;

/// Events sent from the NFC bridge to browser clients over WebSocket.
/// Uses `#[serde(tag = "type")]` for flat JSON discriminant,
/// matching the pattern in fc1200-wasm/src/events.rs (Fc1200Event).
///
/// JSON output must match web types in web/app/types/index.ts:
///   NfcReadEvent:  { type: "nfc_read", employee_id: "..." }
///   NfcErrorEvent: { type: "nfc_error", error: "..." }
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum NfcEvent {
    /// Card UID successfully read.
    #[serde(rename = "nfc_read")]
    NfcRead { employee_id: String },

    /// Driver's license (or other typed card) successfully read.
    #[serde(rename = "nfc_license_read")]
    NfcLicenseRead {
        card_id: String,
        card_type: String,
        atr: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expiry_date: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        remain_count: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        felica_uid: Option<String>,
    },

    /// Debug log for APDU step tracing (sent to browser console).
    #[serde(rename = "nfc_debug")]
    NfcDebug { message: String },

    /// Error during NFC operation.
    #[serde(rename = "nfc_error")]
    NfcError { error: String },

    /// Status broadcast (reader list and connection state).
    #[serde(rename = "status")]
    Status {
        readers: Vec<String>,
        connected: bool,
        version: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_nfc_read() {
        let event = NfcEvent::NfcRead {
            employee_id: "AABBCCDD".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "nfc_read");
        assert_eq!(json["employee_id"], "AABBCCDD");
    }

    #[test]
    fn serialize_nfc_error() {
        let event = NfcEvent::NfcError {
            error: "read_failed".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "nfc_error");
        assert_eq!(json["error"], "read_failed");
    }

    #[test]
    fn serialize_status() {
        let event = NfcEvent::Status {
            readers: vec!["ACS ACR122U".to_string()],
            connected: true,
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["readers"][0], "ACS ACR122U");
        assert_eq!(json["connected"], true);
        assert_eq!(json["version"], "0.1.0");
    }

    #[test]
    fn serialize_nfc_license_read() {
        let event = NfcEvent::NfcLicenseRead {
            card_id: "AABBCCDD11223344".to_string(),
            card_type: "driver_license".to_string(),
            atr: "3B888001000000XX".to_string(),
            expiry_date: Some("AABBCCDD112233445566778899AABBCCDD".to_string()),
            remain_count: Some(3),
            felica_uid: Some("0102030405060708".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "nfc_license_read");
        assert_eq!(json["card_id"], "AABBCCDD11223344");
        assert_eq!(json["card_type"], "driver_license");
        assert_eq!(json["remain_count"], 3);
    }

    #[test]
    fn serialize_nfc_license_read_skips_none() {
        let event = NfcEvent::NfcLicenseRead {
            card_id: "0102030405060708".to_string(),
            card_type: "other".to_string(),
            atr: "3B8F80018000".to_string(),
            expiry_date: None,
            remain_count: None,
            felica_uid: Some("0102030405060708".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "nfc_license_read");
        assert!(json.get("expiry_date").is_none());
        assert!(json.get("remain_count").is_none());
        assert_eq!(json["felica_uid"], "0102030405060708");
    }

    #[test]
    fn serialize_status_no_readers() {
        let event = NfcEvent::Status {
            readers: vec![],
            connected: false,
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "status");
        assert!(json["readers"].as_array().unwrap().is_empty());
        assert_eq!(json["connected"], false);
        assert_eq!(json["version"], "0.1.0");
    }
}
