use std::path::PathBuf;

use crate::protocol::FileOffer;

pub const FILE_CHUNK_SIZE: usize = 16384; // 16KB chunks for file transfer

/// Read receipt status for a message
#[derive(Clone, Debug, PartialEq)]
pub enum ReadStatus {
    Sent,      // ✓  — message sent/delivered
    Read,      // ✓✓ — peer has seen it
}

/// Command entry for autocomplete
#[derive(Clone, Debug)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
}

/// Autocomplete popup state
#[derive(Clone, Debug)]
pub struct AutocompleteState {
    pub commands: Vec<CommandEntry>,
    pub filtered: Vec<usize>,  // indices into commands
    pub selected: usize,       // index into filtered
    pub filter: String,        // current filter text (after /)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Global,
    DirectMessage(String), // peer_id
    Group(String),         // group_id
}

#[derive(Clone, Debug)]
pub struct GroupInfo {
    pub name: String,
    pub members: Vec<String>, // session_ids of members (excluding self)
}

#[derive(Clone, Debug)]
pub struct PendingFileOffer {
    pub offer: FileOffer,
    pub from_peer: String,
    pub tab: Tab,
}

#[derive(Clone, Debug)]
pub struct ActiveTransfer {
    pub offer: FileOffer,
    pub chunks_received: Vec<Option<Vec<u8>>>,
    pub save_path: PathBuf,
    pub chunks_done: u32,
}

#[derive(Clone, Debug)]
pub struct OutgoingTransfer {
    pub offer: FileOffer,
    pub file_data: Vec<u8>,
    pub target_peer: String,
    pub chunks_sent: u32,
    pub is_direct: bool,
}

#[derive(Clone, Debug)]
pub enum CallType {
    Direct(String),             // peer_id
    Group { group_id: String }, // group calls use group membership
}

#[derive(Clone, Debug)]
pub struct CallState {
    pub call_type: CallType,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub muted: bool,
}
