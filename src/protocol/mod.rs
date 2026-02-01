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
    #[serde(default)]
    pub nickname: Option<String>,
    /// Whether this is a direct message (vs global broadcast)
    #[serde(default)]
    pub direct: bool,
}

impl PlainMessage {
    pub fn new(sender: String, content: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content,
            system: false,
            nickname: None,
            direct: false,
        }
    }

    pub fn direct(sender: String, content: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content,
            system: false,
            nickname: None,
            direct: true,
        }
    }

    pub fn system(sender: String, content: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content,
            system: true,
            nickname: None,
            direct: false,
        }
    }

    pub fn nickname(sender: String, nickname: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content: String::new(),
            system: true,
            nickname: Some(nickname),
            direct: false,
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
