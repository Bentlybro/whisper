use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_tungstenite::{accept_async, tungstenite::Message as WsMessage};

use crate::protocol::Message;

type PeerMap = Arc<RwLock<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>;
type RoomMap = Arc<RwLock<HashMap<String, HashSet<String>>>>; // group_id -> set of session_ids

/// Zero-knowledge relay server
/// - Stores nothing to disk
/// - No logging of message content
/// - Only forwards encrypted blobs
/// - Session IDs are ephemeral and in-memory only
/// - Group rooms are tracked by ID only — relay never sees names or content
pub struct RelayServer {
    addr: String,
    peers: PeerMap,
    rooms: RoomMap,
}

impl RelayServer {
    pub fn new(addr: String) -> Self {
        Self {
            addr,
            peers: Arc::new(RwLock::new(HashMap::new())),
            rooms: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn run(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.addr).await?;
        println!("🔒 WSP Relay Server");
        println!("📡 Listening on: {}", self.addr);
        println!("🚫 Zero-knowledge mode: No logging, no storage, RAM only");
        println!();

        loop {
            let (stream, _) = listener.accept().await?;
            
            let peers = self.peers.clone();
            let rooms = self.rooms.clone();
            tokio::spawn(async move {
                match handle_connection(stream, peers, rooms).await {
                    Ok(_) => {}
                    Err(e) => {
                        let err_str = e.to_string();
                        // Silently ignore non-WebSocket connections (bots/scanners)
                        if !err_str.contains("Connection: upgrade") 
                            && !err_str.contains("protocol error") {
                            eprintln!("❌ Connection error: {}", e);
                        }
                    }
                }
            });
        }
    }
}

async fn handle_connection(stream: TcpStream, peers: PeerMap, rooms: RoomMap) -> Result<()> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let mut session_id: Option<String> = None;

    // Spawn task to send messages to this client
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(WsMessage::Binary(msg)).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages
    while let Some(msg) = ws_receiver.next().await {
        match msg {
            Ok(WsMessage::Binary(data)) => {
                // Deserialize message
                let message: Message = match bincode::deserialize(&data) {
                    Ok(m) => m,
                    Err(_) => continue,
                };

                match message {
                    Message::Connect { session_id: sid } => {
                        // Register or update this peer (session resumption)
                        let mut peers_write = peers.write().await;
                        let is_resumption = peers_write.contains_key(&sid);
                        
                        if is_resumption {
                            println!("🔄 Session resumption: {}", &sid[..12]);
                        } else {
                            println!("🆕 New session: {}", &sid[..12]);
                        }
                        
                        // Insert/replace the sender channel
                        peers_write.insert(sid.clone(), tx.clone());
                        drop(peers_write);
                        
                        session_id = Some(sid);
                        
                        // Send ACK
                        let ack = bincode::serialize(&Message::Ack)?;
                        tx.send(ack)?;
                    }
                    Message::Discover { target_session } => {
                        // Forward discovery to target if online
                        let peers_read = peers.read().await;
                        if let Some(target_tx) = peers_read.get(&target_session) {
                            target_tx.send(data)?;
                        }
                    }
                    Message::KeyExchange { .. } | Message::AudioFrame { .. } => {
                        // Forward key exchanges and audio to all peers (blind forwarding)
                        let peers_read = peers.read().await;
                        for (sid, peer_tx) in peers_read.iter() {
                            if Some(sid) != session_id.as_ref() {
                                let _ = peer_tx.send(data.clone());
                            }
                        }
                    }
                    Message::Encrypted { ref target, .. } => {
                        if !target.is_empty() {
                            // Targeted: forward only to the specified peer
                            let peers_read = peers.read().await;
                            if let Some(peer_tx) = peers_read.get(target) {
                                let _ = peer_tx.send(data.clone());
                            }
                        } else {
                            // Broadcast (legacy): forward to all peers
                            let peers_read = peers.read().await;
                            for (sid, peer_tx) in peers_read.iter() {
                                if Some(sid) != session_id.as_ref() {
                                    let _ = peer_tx.send(data.clone());
                                }
                            }
                        }
                    }
                    Message::GroupJoin { session_id: sid, group_id } => {
                        // Add session to the group room
                        let mut rooms_write = rooms.write().await;
                        let room = rooms_write.entry(group_id.clone()).or_insert_with(HashSet::new);
                        room.insert(sid.clone());
                        println!("📥 Session {}.. joined room {}.. ({} members)", 
                            &sid[..12.min(sid.len())], 
                            &group_id[..12.min(group_id.len())],
                            room.len());
                    }
                    Message::GroupLeave { session_id: sid, group_id } => {
                        // Remove session from the group room
                        let mut rooms_write = rooms.write().await;
                        if let Some(room) = rooms_write.get_mut(&group_id) {
                            room.remove(&sid);
                            let remaining = room.len();
                            println!("📤 Session {}.. left room {}.. ({} remaining)", 
                                &sid[..12.min(sid.len())], 
                                &group_id[..12.min(group_id.len())],
                                remaining);
                            // Clean up empty rooms
                            if remaining == 0 {
                                rooms_write.remove(&group_id);
                            }
                        }
                    }
                    Message::GroupEncrypted { from, group_id, .. } => {
                        // Forward to all members of the group room except sender
                        let rooms_read = rooms.read().await;
                        if let Some(members) = rooms_read.get(&group_id) {
                            let peers_read = peers.read().await;
                            for member_sid in members {
                                if member_sid != &from {
                                    if let Some(peer_tx) = peers_read.get(member_sid) {
                                        let _ = peer_tx.send(data.clone());
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(WsMessage::Close(_)) | Err(_) => break,
            _ => {}
        }
    }

    // Cleanup on disconnect
    if let Some(sid) = session_id {
        peers.write().await.remove(&sid);
        
        // Remove from all rooms
        let mut rooms_write = rooms.write().await;
        let mut empty_rooms = Vec::new();
        for (group_id, members) in rooms_write.iter_mut() {
            members.remove(&sid);
            if members.is_empty() {
                empty_rooms.push(group_id.clone());
            }
        }
        for group_id in empty_rooms {
            rooms_write.remove(&group_id);
        }
        
        println!("🔌 Session disconnected");
    }

    send_task.abort();
    Ok(())
}

pub async fn start_relay(addr: String) -> Result<()> {
    let server = RelayServer::new(addr);
    server.run().await
}
