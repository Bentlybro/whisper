use serde::{Deserialize, Serialize};

/// Message types sent over the wire
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    /// Initial handshake with relay
    Connect { session_id: String },
    /// Peer discovery
    Discover { target_session: String },
    /// Key exchange message (contains identity public key + ephemeral DH ratchet key)
    KeyExchange {
        from: String,
        public_key: Vec<u8>,
        /// Initial ephemeral DH public key for the Double Ratchet.
        /// Lets the peer set dh_remote immediately so subsequent DH ratchet
        /// steps (where the sender generates a NEW key) are detectable.
        #[serde(default)]
        dh_ratchet_key: Vec<u8>,
    },
    /// Encrypted message payload
    Encrypted {
        from: String,
        /// Target session ID — relay forwards only to this peer
        /// Empty string = broadcast to all (legacy/KeyExchange compat)
        #[serde(default)]
        target: String,
        /// Serialized RatchetHeader (DH public key + chain metadata)
        #[serde(default)]
        header: Vec<u8>,
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
        /// Serialized RatchetHeader (DH public key + chain metadata)
        #[serde(default)]
        header: Vec<u8>,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// Encrypted audio frame — relay forwards to all peers (only target can decrypt)
    AudioFrame {
        from: String,
        nonce: Vec<u8>,
        ciphertext: Vec<u8>,
    },
    /// Lightweight typing indicator — NOT encrypted, doesn't touch the ratchet
    Typing {
        from: String,
        /// Target peer (empty = broadcast)
        target: String,
        is_typing: bool,
    },
    /// Lightweight read receipt — NOT encrypted, doesn't touch the ratchet
    ReadReceipt {
        from: String,
        target: String,
        message_id: String,
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    /// Voice call request (true = requesting a call)
    #[serde(default)]
    pub call_request: Option<bool>,
    /// Voice call accept/reject (true = accept, false = reject)
    #[serde(default)]
    pub call_accept: Option<bool>,
    /// Voice call hangup
    #[serde(default)]
    pub call_hangup: Option<bool>,
    /// Unique message ID for tracking read receipts
    #[serde(default)]
    pub message_id: Option<String>,
    /// Typing indicator (true = started typing, false = stopped)
    #[serde(default)]
    pub typing: Option<bool>,
    /// Read receipt — contains the message_id that was read
    #[serde(default)]
    pub read_receipt: Option<String>,
}

impl PlainMessage {
    /// Base message with sender and timestamp set, all other fields default
    fn base(sender: String) -> Self {
        Self {
            timestamp: chrono::Utc::now().timestamp(),
            sender,
            ..Default::default()
        }
    }

    pub fn new(sender: String, content: String) -> Self {
        Self { content, ..Self::base(sender) }
    }

    pub fn direct(sender: String, content: String) -> Self {
        Self { content, direct: true, ..Self::base(sender) }
    }

    pub fn system(sender: String, content: String) -> Self {
        Self { content, system: true, ..Self::base(sender) }
    }

    pub fn nickname(sender: String, nickname: String) -> Self {
        Self { system: true, nickname: Some(nickname), ..Self::base(sender) }
    }

    pub fn dm_request(sender: String) -> Self {
        Self { system: true, direct: true, dm_request: true, ..Self::base(sender) }
    }

    pub fn file_offer(sender: String, offer: FileOffer, direct: bool) -> Self {
        Self { direct, file_offer: Some(offer), ..Self::base(sender) }
    }

    pub fn file_chunk(sender: String, chunk: FileChunk, direct: bool) -> Self {
        Self { direct, file_chunk: Some(chunk), ..Self::base(sender) }
    }

    pub fn file_response(sender: String, file_id: String, accept: bool, direct: bool) -> Self {
        Self { content: file_id, direct, file_response: Some(accept), ..Self::base(sender) }
    }

    /// A group chat message
    pub fn group(sender: String, content: String, group_id: String) -> Self {
        Self { content, group_id: Some(group_id), ..Self::base(sender) }
    }

    /// A group invite sent via DM
    pub fn group_invite_msg(sender: String, invite: GroupInvite) -> Self {
        Self { system: true, direct: true, group_invite: Some(invite), ..Self::base(sender) }
    }

    /// Voice call request
    pub fn call_request(sender: String) -> Self {
        Self { system: true, direct: true, call_request: Some(true), ..Self::base(sender) }
    }

    /// Voice call accept/reject
    pub fn call_accept(sender: String, accept: bool) -> Self {
        Self { system: true, direct: true, call_accept: Some(accept), ..Self::base(sender) }
    }

    /// Voice call hangup
    pub fn call_hangup(sender: String) -> Self {
        Self { system: true, direct: true, call_hangup: Some(true), ..Self::base(sender) }
    }

    /// Typing indicator
    pub fn typing(sender: String, is_typing: bool, direct: bool) -> Self {
        Self { system: true, direct, typing: Some(is_typing), ..Self::base(sender) }
    }

    /// Read receipt for a specific message
    pub fn read_receipt(sender: String, message_id: String, direct: bool) -> Self {
        Self { system: true, direct, read_receipt: Some(message_id), ..Self::base(sender) }
    }

    /// Generate a unique message ID
    pub fn generate_id() -> String {
        use rand::Rng;
        let random_bytes: Vec<u8> = (0..8).map(|_| rand::thread_rng().gen()).collect();
        hex::encode(random_bytes)
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
