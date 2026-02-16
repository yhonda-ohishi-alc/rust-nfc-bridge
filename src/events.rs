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
