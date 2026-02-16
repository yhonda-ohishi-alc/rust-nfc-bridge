use std::fmt;

#[derive(Debug)]
pub enum BridgeError {
    Pcsc(pcsc::Error),
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),
    Io(std::io::Error),
    NoReaders,
    CardReadFailed(String),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pcsc(e) => write!(f, "PC/SC error: {}", e),
            Self::WebSocket(e) => write!(f, "WebSocket error: {}", e),
            Self::Io(e) => write!(f, "IO error: {}", e),
            Self::NoReaders => write!(f, "No NFC readers found"),
            Self::CardReadFailed(msg) => write!(f, "Card read failed: {}", msg),
        }
    }
}

impl std::error::Error for BridgeError {}

impl From<pcsc::Error> for BridgeError {
    fn from(e: pcsc::Error) -> Self {
        Self::Pcsc(e)
    }
}

impl From<tokio_tungstenite::tungstenite::Error> for BridgeError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self {
        Self::WebSocket(Box::new(e))
    }
}

impl From<std::io::Error> for BridgeError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
