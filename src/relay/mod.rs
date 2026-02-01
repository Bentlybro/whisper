use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_tungstenite::{accept_async, tungstenite::Message as WsMessage};

use crate::protocol::Message;

type PeerMap = Arc<RwLock<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>;

/// Zero-knowledge relay server
/// - Stores nothing to disk
/// - No logging of message content
/// - Only forwards encrypted blobs
/// - Session IDs are ephemeral and in-memory only
pub struct RelayServer {
    addr: String,
    peers: PeerMap,
}

impl RelayServer {
    pub fn new(addr: String) -> Self {
        Self {
            addr,
            peers: Arc::new(RwLock::new(HashMap::new())),
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
            println!("🔌 New connection (identity hidden)");
            
            let peers = self.peers.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, peers).await {
                    eprintln!("❌ Connection error: {}", e);
                }
            });
        }
    }
}

async fn handle_connection(stream: TcpStream, peers: PeerMap) -> Result<()> {
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
                    Message::KeyExchange { .. } | Message::Encrypted { .. } => {
                        // Forward encrypted messages to all peers (blind forwarding)
                        // In a real implementation, we'd use session routing
                        // For MVP, we broadcast to all connected peers
                        let peers_read = peers.read().await;
                        for (sid, peer_tx) in peers_read.iter() {
                            if Some(sid) != session_id.as_ref() {
                                let _ = peer_tx.send(data.clone());
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
        println!("🔌 Session disconnected");
    }

    send_task.abort();
    Ok(())
}

pub async fn start_relay(addr: String) -> Result<()> {
    let server = RelayServer::new(addr);
    server.run().await
}
