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
    /// Join a group room on the relay (relay tracks room membership)
    GroupJoin {
        session_id: String,
        group_id: String,
    },
    /// Leave a group room on the relay
    GroupLeave {
        session_id: String,
        group_id: String,
    },
    /// Encrypted message for a group — relay forwards to all room members except sender
    GroupEncrypted {
        from: String,
        group_id: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
}

/// File offer metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileOffer {
    pub file_id: String,      // Random ID for this transfer
    pub filename: String,     // Original filename only (no path!)
    pub size: u64,            // Total size in bytes
    pub checksum: String,     // Blake3 hash of full file
    pub total_chunks: u32,    // Number of chunks
}

/// File chunk data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChunk {
    pub file_id: String,      // Matches the offer
    pub index: u32,           // Chunk index (0-based)
    pub data: Vec<u8>,        // Chunk data (base64 in serde)
}

/// Group invite data (sent inside a DM PlainMessage)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupInvite {
    pub group_id: String,
    pub group_name: String,
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
    /// Whether this is a DM open request (peer should open a DM tab)
    #[serde(default)]
    pub dm_request: bool,
    /// File offer metadata
    #[serde(default)]
    pub file_offer: Option<FileOffer>,
    /// File chunk data
    #[serde(default)]
    pub file_chunk: Option<FileChunk>,
    /// File response (true = accept, false = reject)
    #[serde(default)]
    pub file_response: Option<bool>,
    /// Group ID — identifies which group this message belongs to
    #[serde(default)]
    pub group_id: Option<String>,
    /// Group invite data (sent via DM to invite someone)
    #[serde(default)]
    pub group_invite: Option<GroupInvite>,
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
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: None,
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
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: None,
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
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: None,
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
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: None,
        }
    }

    pub fn dm_request(sender: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content: String::new(),
            system: true,
            nickname: None,
            direct: true,
            dm_request: true,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: None,
        }
    }

    pub fn file_offer(sender: String, offer: FileOffer, direct: bool) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content: String::new(),
            system: false,
            nickname: None,
            direct,
            dm_request: false,
            file_offer: Some(offer),
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: None,
        }
    }

    pub fn file_chunk(sender: String, chunk: FileChunk, direct: bool) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content: String::new(),
            system: false,
            nickname: None,
            direct,
            dm_request: false,
            file_offer: None,
            file_chunk: Some(chunk),
            file_response: None,
            group_id: None,
            group_invite: None,
        }
    }

    pub fn file_response(sender: String, file_id: String, accept: bool, direct: bool) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content: file_id,
            system: false,
            nickname: None,
            direct,
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: Some(accept),
            group_id: None,
            group_invite: None,
        }
    }

    /// A group chat message
    pub fn group(sender: String, content: String, group_id: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content,
            system: false,
            nickname: None,
            direct: false,
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: Some(group_id),
            group_invite: None,
        }
    }

    /// A group invite sent via DM
    pub fn group_invite_msg(sender: String, invite: GroupInvite) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            content: String::new(),
            system: true,
            nickname: None,
            direct: true,
            dm_request: false,
            file_offer: None,
            file_chunk: None,
            file_response: None,
            group_id: None,
            group_invite: Some(invite),
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
