use serde::{Deserialize, Serialize};

/// Message types sent over the wire
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// Initial handshake with relay
    Connect { session_id: String },
    /// Peer discovery
    Discover { target_session: String },
    /// Key exchange message (contains public key)
    KeyExchange { from: String, public_key: Vec<u8> },
    /// Encrypted message payload
    Encrypted {
        from: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// Acknowledgment
    Ack,
    /// Error
    Error { message: String },
}

/// Plaintext message format (before encryption)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlainMessage {
    pub timestamp: i64,
    pub sender: String,
    pub content: String,
    #[serde(default)]
    pub system: bool,
}

impl PlainMessage {
    pub fn new(sender: String, content: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content,
            system: false,
        }
    }

    pub fn system(sender: String, content: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content,
            system: true,
        }
    }
}

/// Session state
#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub peer_id: Option<String>,
    pub shared_secret: Option<Vec<u8>>,
}

impl Session {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            peer_id: None,
            shared_secret: None,
        }
    }
}
