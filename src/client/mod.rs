use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream};

use crate::crypto::{decrypt_message, encrypt_message, Identity};
use crate::protocol::{Message, PlainMessage};

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub shared_secret: Vec<u8>,
    pub nickname: Option<String>,
}

pub enum OutgoingMessage {
    Global(PlainMessage),
    Direct { target_id: String, message: PlainMessage },
    /// Send a group message — fan out to each member using pairwise encryption
    Group {
        group_id: String,
        member_ids: Vec<String>,
        message: PlainMessage,
    },
    /// Tell the relay to join a group room
    JoinRoom { group_id: String },
    /// Tell the relay to leave a group room
    LeaveRoom { group_id: String },
}

pub struct ChatClient {
    identity: Identity,
    relay_url: String,
    session_id: String,
    nickname: Option<String>,
}

impl ChatClient {
    pub fn new(identity: Identity, relay_url: String, nickname: Option<String>) -> Self {
        let session_id = generate_session_id();
        Self {
            identity,
            relay_url,
            session_id,
            nickname,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn identity_id(&self) -> String {
        self.identity.public_key_b64()
    }

    pub fn nickname(&self) -> Option<&str> {
        self.nickname.as_deref()
    }

    pub fn set_nickname(&mut self, nickname: String) {
        self.nickname = Some(nickname);
    }

    pub async fn connect(&mut self) -> Result<(
        mpsc::UnboundedSender<OutgoingMessage>,
        mpsc::UnboundedReceiver<PlainMessage>,
        mpsc::UnboundedReceiver<String>, // Status messages
        mpsc::UnboundedReceiver<HashMap<String, PeerInfo>>, // Peer updates
    )> {
        // Channels for communication with TUI (persist across reconnects)
        let (msg_tx, msg_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<PlainMessage>();
        let (status_tx, status_rx) = mpsc::unbounded_channel::<String>();
        let (peer_update_tx, peer_update_rx) = mpsc::unbounded_channel::<HashMap<String, PeerInfo>>();

        let identity = self.identity.clone_for_thread();
        let session_id = self.session_id.clone();
        let public_key_bytes = self.identity.public_key_bytes();
        let my_nickname = self.nickname.clone();
        let relay_url = self.relay_url.clone();
        
        // Track all peers (persists across reconnects)
        let peers = std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::<String, PeerInfo>::new()));
        
        // Wrap receiver in Arc<Mutex> so it can be shared across reconnection attempts
        let msg_rx = std::sync::Arc::new(tokio::sync::Mutex::new(msg_rx));

        // Spawn reconnection loop
        let peers_reconnect = peers.clone();
        let status_tx_reconnect = status_tx.clone();
        tokio::spawn(async move {
            let mut reconnect_delay = 1u64;
            let mut attempt = 0u32;
            
            loop {
                // Attempt connection
                match Self::establish_connection(
                    &relay_url,
                    &session_id,
                    &public_key_bytes,
                    &identity,
                    &my_nickname,
                    peers_reconnect.clone(),
                    msg_rx.clone(),
                    incoming_tx.clone(),
                    status_tx_reconnect.clone(),
                    peer_update_tx.clone(),
                    attempt,
                ).await {
                    Ok(_) => {
                        // Connection ended gracefully, reset backoff
                        reconnect_delay = 1;
                        attempt = 0;
                    }
                    Err(_e) => {
                        attempt += 1;
                        let _ = status_tx_reconnect.send(format!(
                            "Connection lost, reconnecting (attempt {})...",
                            attempt
                        ));
                        
                        // Exponential backoff: 1s, 2s, 4s, 8s, max 30s
                        sleep(Duration::from_secs(reconnect_delay)).await;
                        reconnect_delay = (reconnect_delay * 2).min(30);
                    }
                }
            }
        });

        Ok((msg_tx, incoming_rx, status_rx, peer_update_rx))
    }

    async fn establish_connection(
        relay_url: &str,
        session_id: &str,
        public_key_bytes: &[u8],
        identity: &Identity,
        my_nickname: &Option<String>,
        peers: std::sync::Arc<tokio::sync::RwLock<HashMap<String, PeerInfo>>>,
        outgoing_rx: std::sync::Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<OutgoingMessage>>>,
        incoming_tx: mpsc::UnboundedSender<PlainMessage>,
        status_tx: mpsc::UnboundedSender<String>,
        peer_update_tx: mpsc::UnboundedSender<HashMap<String, PeerInfo>>,
        attempt: u32,
    ) -> Result<()> {
        // Connect to relay
        let (ws_stream, _) = connect_async(relay_url)
            .await
            .context("Failed to connect to relay")?;

        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        // Send connect message with same session_id (for session resumption)
        let connect_msg = Message::Connect {
            session_id: session_id.to_string(),
        };
        let data = bincode::serialize(&connect_msg)?;
        ws_sender.send(WsMessage::Binary(data)).await?;

        // Send key exchange to re-establish E2EE with all peers
        let key_exchange_msg = Message::KeyExchange {
            from: session_id.to_string(),
            public_key: public_key_bytes.to_vec(),
        };
        let ke_data = bincode::serialize(&key_exchange_msg)?;
        ws_sender.send(WsMessage::Binary(ke_data)).await?;

        // Channels for internal communication
        let (ke_reply_tx, mut ke_reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (nickname_tx, mut nickname_rx) = mpsc::unbounded_channel::<(String, Vec<u8>)>();
        let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<()>();

        // Channels for signaling connection failure
        let (failure_tx, mut failure_rx) = mpsc::unbounded_channel::<String>();

        // Spawn receiver task
        let peers_recv = peers.clone();
        let status_tx_recv = status_tx.clone();
        let session_id_recv = session_id.to_string();
        let public_key_bytes_recv = public_key_bytes.to_vec();
        let my_nickname_recv = my_nickname.clone();
        let identity_recv = identity.clone_for_thread();
        let pong_tx_clone = pong_tx.clone();
        let failure_tx_recv = failure_tx.clone();
        
        tokio::spawn(async move {
            while let Some(msg) = ws_receiver.next().await {
                match msg {
                    Ok(WsMessage::Binary(data)) => {
                        if let Ok(message) = bincode::deserialize::<Message>(&data) {
                            match message {
                                Message::Ack => {
                                    if attempt == 0 {
                                        let _ = status_tx_recv.send("Connected to relay".to_string());
                                    } else {
                                        let _ = status_tx_recv.send("Reconnected".to_string());
                                    }
                                }
                                Message::KeyExchange { from, public_key } => {
                                    if from == session_id_recv {
                                        continue; // Ignore our own key exchange
                                    }
                                    
                                    // Perform key exchange
                                    match identity_recv.key_exchange(&public_key) {
                                        Ok(secret) => {
                                            let mut peers_map = peers_recv.write().await;
                                            let is_new_peer = !peers_map.contains_key(&from);
                                            
                                            peers_map.insert(from.clone(), PeerInfo {
                                                shared_secret: secret.clone(),
                                                nickname: None,
                                            });
                                            
                                            let _ = status_tx_recv.send(format!("🔐 Encrypted session established with {}", &from[..12]));
                                            
                                            // Send peer update
                                            let _ = peer_update_tx.send(peers_map.clone());
                                            
                                            // Show join notification
                                            if is_new_peer {
                                                let join_msg = PlainMessage::system(
                                                    from.clone(),
                                                    format!("{} has joined", &from[..12]),
                                                );
                                                let _ = incoming_tx.send(join_msg);
                                            }

                                            // Send our public key back so the peer can also complete the exchange
                                            if is_new_peer {
                                                let reply = Message::KeyExchange {
                                                    from: session_id_recv.clone(),
                                                    public_key: public_key_bytes_recv.clone(),
                                                };
                                                if let Ok(reply_data) = bincode::serialize(&reply) {
                                                    let _ = ke_reply_tx.send(reply_data);
                                                }
                                                
                                                // Send our nickname after key exchange
                                                if let Some(ref nick) = my_nickname_recv {
                                                    let nick = nick.clone();
                                                    let session_id_nick = session_id_recv.clone();
                                                    let secret_nick = secret.clone();
                                                    let nickname_tx_clone = nickname_tx.clone();
                                                    let from_clone = from.clone();
                                                    tokio::spawn(async move {
                                                        tokio::time::sleep(Duration::from_millis(500)).await;
                                                        let nickname_msg = PlainMessage::nickname(
                                                            session_id_nick.clone(),
                                                            nick,
                                                        );
                                                        if let Ok(serialized) = bincode::serialize(&nickname_msg) {
                                                            if let Ok((nonce, ciphertext)) = encrypt_message(&secret_nick, &serialized) {
                                                                let encrypted_msg = Message::Encrypted {
                                                                    from: session_id_nick,
                                                                    nonce,
                                                                    ciphertext,
                                                                };
                                                                if let Ok(data) = bincode::serialize(&encrypted_msg) {
                                                                    let _ = nickname_tx_clone.send((from_clone, data));
                                                                }
                                                            }
                                                        }
                                                    });
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            let _ = status_tx_recv.send(format!("❌ Key exchange failed: {}", e));
                                        }
                                    }
                                }
                                Message::Encrypted { from, nonce, ciphertext } => {
                                    if from == session_id_recv {
                                        continue; // Ignore our own messages
                                    }
                                    
                                    let peers_map = peers_recv.read().await;
                                    if let Some(peer_info) = peers_map.get(&from) {
                                        match decrypt_message(&peer_info.shared_secret, &nonce, &ciphertext) {
                                            Ok(plaintext) => {
                                                if let Ok(plain_msg) = bincode::deserialize::<PlainMessage>(&plaintext) {
                                                    // Handle nickname updates
                                                    if plain_msg.system && plain_msg.nickname.is_some() {
                                                        let new_nick = plain_msg.nickname.clone().unwrap();
                                                        drop(peers_map);
                                                        let mut peers_map = peers_recv.write().await;
                                                        let old_nick = peers_map.get(&from).and_then(|p| p.nickname.clone());
                                                        if let Some(peer) = peers_map.get_mut(&from) {
                                                            peer.nickname = Some(new_nick.clone());
                                                            let _ = peer_update_tx.send(peers_map.clone());
                                                        }
                                                        drop(peers_map);
                                                        let display = old_nick.unwrap_or_else(|| from[..12.min(from.len())].to_string());
                                                        let notify = PlainMessage::system(
                                                            from.clone(),
                                                            format!("{} is now known as {}", display, new_nick),
                                                        );
                                                        let _ = incoming_tx.send(notify);
                                                    } else {
                                                        let _ = incoming_tx.send(plain_msg);
                                                    }
                                                }
                                            }
                                            Err(_) => {
                                                // Silently drop failed decryptions
                                            }
                                        }
                                    }
                                }
                                Message::GroupEncrypted { from, group_id, nonce, ciphertext } => {
                                    if from == session_id_recv {
                                        continue; // Ignore our own messages
                                    }
                                    
                                    // Try to decrypt with sender's pairwise key
                                    let peers_map = peers_recv.read().await;
                                    if let Some(peer_info) = peers_map.get(&from) {
                                        match decrypt_message(&peer_info.shared_secret, &nonce, &ciphertext) {
                                            Ok(plaintext) => {
                                                if let Ok(mut plain_msg) = bincode::deserialize::<PlainMessage>(&plaintext) {
                                                    // Ensure group_id is set (in case it wasn't in the inner message)
                                                    plain_msg.group_id = Some(group_id);
                                                    let _ = incoming_tx.send(plain_msg);
                                                }
                                            }
                                            Err(_) => {
                                                // This message was encrypted for a different group member — ignore
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(WsMessage::Pong(_)) => {
                        // Pong received
                        let _ = pong_tx_clone.send(());
                    }
                    Ok(WsMessage::Close(_)) | Err(_) => {
                        let _ = failure_tx_recv.send("Connection closed".to_string());
                        break;
                    }
                    _ => {}
                }
            }
            let _ = failure_tx_recv.send("Receiver stream ended".to_string());
        });

        // Spawn sender task
        let peers_send = peers.clone();
        let session_id_send = session_id.to_string();
        let status_tx_send = status_tx.clone();
        let failure_tx_send = failure_tx.clone();
        let outgoing_rx_clone = outgoing_rx.clone();
        
        tokio::spawn(async move {
            // Send ping every 30 seconds, expect pong within 10 seconds
            let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
            let mut pending_pong = false;
            let mut pong_deadline = tokio::time::Instant::now();
            
            loop {
                // Check if pong deadline exceeded
                if pending_pong && tokio::time::Instant::now() > pong_deadline {
                    let _ = failure_tx_send.send("Pong timeout".to_string());
                    break;
                }
                
                // Lock the receiver before select!
                let mut outgoing_locked = outgoing_rx_clone.lock().await;
                
                tokio::select! {
                    _ = ping_interval.tick() => {
                        // Send WebSocket Ping
                        if ws_sender.send(WsMessage::Ping(vec![])).await.is_err() {
                            let _ = failure_tx_send.send("Failed to send ping".to_string());
                            break;
                        }
                        pending_pong = true;
                        pong_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                    }
                    Some(_) = pong_rx.recv() => {
                        // Pong received
                        pending_pong = false;
                    }
                    Some(ke_data) = ke_reply_rx.recv() => {
                        if ws_sender.send(WsMessage::Binary(ke_data)).await.is_err() {
                            let _ = failure_tx_send.send("Send failed".to_string());
                            break;
                        }
                    }
                    Some((_target, data)) = nickname_rx.recv() => {
                        if ws_sender.send(WsMessage::Binary(data)).await.is_err() {
                            let _ = failure_tx_send.send("Send failed".to_string());
                            break;
                        }
                    }
                    outgoing = outgoing_locked.recv() => {
                        if let Some(outgoing) = outgoing {
                            match outgoing {
                                OutgoingMessage::Direct { target_id, message } => {
                                    let peers_map = peers_send.read().await;
                                    if let Some(peer_info) = peers_map.get(&target_id) {
                                        let serialized = bincode::serialize(&message).unwrap();
                                        match encrypt_message(&peer_info.shared_secret, &serialized) {
                                            Ok((nonce, ciphertext)) => {
                                                let encrypted_msg = Message::Encrypted {
                                                    from: session_id_send.clone(),
                                                    nonce,
                                                    ciphertext,
                                                };
                                                let data = bincode::serialize(&encrypted_msg).unwrap();
                                                if ws_sender.send(WsMessage::Binary(data)).await.is_err() {
                                                    let _ = failure_tx_send.send("Send failed".to_string());
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                let _ = status_tx_send.send(format!("❌ Encryption failed: {}", e));
                                            }
                                        }
                                    } else {
                                        let _ = status_tx_send.send(format!("❌ No session with peer {}", &target_id[..12.min(target_id.len())]));
                                    }
                                }
                                OutgoingMessage::Global(message) => {
                                    let peers_map = peers_send.read().await;
                                    if peers_map.is_empty() {
                                        let _ = status_tx_send.send("⚠️  No peers connected".to_string());
                                    } else {
                                        for (peer_id, peer_info) in peers_map.iter() {
                                            let serialized = bincode::serialize(&message).unwrap();
                                            match encrypt_message(&peer_info.shared_secret, &serialized) {
                                                Ok((nonce, ciphertext)) => {
                                                    let encrypted_msg = Message::Encrypted {
                                                        from: session_id_send.clone(),
                                                        nonce,
                                                        ciphertext,
                                                    };
                                                    let data = bincode::serialize(&encrypted_msg).unwrap();
                                                    if ws_sender.send(WsMessage::Binary(data)).await.is_err() {
                                                        let _ = failure_tx_send.send("Send failed".to_string());
                                                        break;
                                                    }
                                                }
                                                Err(e) => {
                                                    let _ = status_tx_send.send(format!("❌ Encryption failed for {}: {}", &peer_id[..12], e));
                                                }
                                            }
                                        }
                                    }
                                }
                                OutgoingMessage::Group { group_id, member_ids, message } => {
                                    // Fan-out: encrypt once per member using pairwise keys,
                                    // send as GroupEncrypted so relay routes via room
                                    let peers_map = peers_send.read().await;
                                    let mut sent = 0;
                                    for member_id in &member_ids {
                                        if let Some(peer_info) = peers_map.get(member_id) {
                                            let serialized = bincode::serialize(&message).unwrap();
                                            match encrypt_message(&peer_info.shared_secret, &serialized) {
                                                Ok((nonce, ciphertext)) => {
                                                    let encrypted_msg = Message::GroupEncrypted {
                                                        from: session_id_send.clone(),
                                                        group_id: group_id.clone(),
                                                        nonce,
                                                        ciphertext,
                                                    };
                                                    let data = bincode::serialize(&encrypted_msg).unwrap();
                                                    if ws_sender.send(WsMessage::Binary(data)).await.is_err() {
                                                        let _ = failure_tx_send.send("Send failed".to_string());
                                                        break;
                                                    }
                                                    sent += 1;
                                                }
                                                Err(e) => {
                                                    let _ = status_tx_send.send(format!("❌ Group encrypt failed for {}: {}", &member_id[..12], e));
                                                }
                                            }
                                        }
                                    }
                                    if sent == 0 && !member_ids.is_empty() {
                                        let _ = status_tx_send.send("⚠️  No group members online".to_string());
                                    }
                                }
                                OutgoingMessage::JoinRoom { group_id } => {
                                    let join_msg = Message::GroupJoin {
                                        session_id: session_id_send.clone(),
                                        group_id,
                                    };
                                    let data = bincode::serialize(&join_msg).unwrap();
                                    if ws_sender.send(WsMessage::Binary(data)).await.is_err() {
                                        let _ = failure_tx_send.send("Send failed".to_string());
                                        break;
                                    }
                                }
                                OutgoingMessage::LeaveRoom { group_id } => {
                                    let leave_msg = Message::GroupLeave {
                                        session_id: session_id_send.clone(),
                                        group_id,
                                    };
                                    let data = bincode::serialize(&leave_msg).unwrap();
                                    if ws_sender.send(WsMessage::Binary(data)).await.is_err() {
                                        let _ = failure_tx_send.send("Send failed".to_string());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        // Wait for connection failure signal
        if let Some(reason) = failure_rx.recv().await {
            Err(anyhow::anyhow!("Connection lost: {}", reason))
        } else {
            Err(anyhow::anyhow!("Connection lost"))
        }
    }

    pub async fn initiate_handshake(
        &mut self,
        ws_sender: &mut futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, WsMessage>,
    ) -> Result<()> {
        let key_exchange = Message::KeyExchange {
            from: self.session_id.clone(),
            public_key: self.identity.public_key_bytes(),
        };
        let data = bincode::serialize(&key_exchange)?;
        ws_sender.send(WsMessage::Binary(data)).await?;
        Ok(())
    }
}

impl Identity {
    // Helper for cloning in async context
    fn clone_for_thread(&self) -> Self {
        // Since we can't directly clone StaticSecret, we serialize and deserialize
        let serialized = bincode::serialize(self).unwrap();
        bincode::deserialize(&serialized).unwrap()
    }
}

fn generate_session_id() -> String {
    use rand::Rng;
    let random_bytes: Vec<u8> = (0..16).map(|_| rand::thread_rng().gen()).collect();
    hex::encode(random_bytes)
}
