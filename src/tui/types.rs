use std::path::PathBuf;

use crate::protocol::FileOffer;

pub const FILE_CHUNK_SIZE: usize = 16384; // 16KB chunks for file transfer

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
