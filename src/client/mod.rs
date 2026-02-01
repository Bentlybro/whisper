use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
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
        let (ws_stream, _) = connect_async(&self.relay_url)
            .await
            .context("Failed to connect to relay")?;

        let (mut ws_sender, mut ws_receiver) = ws_stream.split();

        // Send initial connect message
        let connect_msg = Message::Connect {
            session_id: self.session_id.clone(),
        };
        let data = bincode::serialize(&connect_msg)?;
        ws_sender.send(WsMessage::Binary(data)).await?;

        // Send key exchange immediately so any peer that connects can establish E2EE
        let key_exchange_msg = Message::KeyExchange {
            from: self.session_id.clone(),
            public_key: self.identity.public_key_bytes(),
        };
        let ke_data = bincode::serialize(&key_exchange_msg)?;
        ws_sender.send(WsMessage::Binary(ke_data)).await?;

        // Channels for communication with TUI
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<OutgoingMessage>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<PlainMessage>();
        let (status_tx, status_rx) = mpsc::unbounded_channel::<String>();
        let (peer_update_tx, peer_update_rx) = mpsc::unbounded_channel::<HashMap<String, PeerInfo>>();

        let identity = self.identity.clone_for_thread();
        let session_id = self.session_id.clone();
        let public_key_bytes = self.identity.public_key_bytes();
        let my_nickname = self.nickname.clone();
        
        // Track all peers
        let peers = std::sync::Arc::new(tokio::sync::RwLock::new(HashMap::<String, PeerInfo>::new()));

        // Spawn receiver task
        let peers_recv = peers.clone();
        let status_tx_recv = status_tx.clone();
        let session_id_recv = session_id.clone();
        let public_key_bytes_recv = public_key_bytes.clone();
        let my_nickname_recv = my_nickname.clone();
        let (ke_reply_tx, mut ke_reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (nickname_tx, mut nickname_rx) = mpsc::unbounded_channel::<(String, Vec<u8>)>();
        
        tokio::spawn(async move {
            while let Some(msg) = ws_receiver.next().await {
                if let Ok(WsMessage::Binary(data)) = msg {
                    if let Ok(message) = bincode::deserialize::<Message>(&data) {
                        match message {
                            Message::Ack => {
                                let _ = status_tx_recv.send("Connected to relay".to_string());
                            }
                            Message::KeyExchange { from, public_key } => {
                                if from == session_id_recv {
                                    continue; // Ignore our own key exchange
                                }
                                
                                // Perform key exchange
                                match identity.key_exchange(&public_key) {
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
                                            
                                            // Send our nickname after a short delay to ensure
                                            // key exchange reply arrives first at the peer
                                            if let Some(ref nick) = my_nickname_recv {
                                                let nick = nick.clone();
                                                let session_id_nick = session_id_recv.clone();
                                                let secret_nick = secret.clone();
                                                let nickname_tx_clone = nickname_tx.clone();
                                                let from_clone = from.clone();
                                                tokio::spawn(async move {
                                                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
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
                                                    drop(peers_map); // Release read lock
                                                    let mut peers_map = peers_recv.write().await;
                                                    if let Some(peer) = peers_map.get_mut(&from) {
                                                        peer.nickname = plain_msg.nickname.clone();
                                                        let _ = peer_update_tx.send(peers_map.clone());
                                                    }
                                                } else {
                                                    let _ = incoming_tx.send(plain_msg);
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            // Silently drop failed decryptions (message wasn't for us)
                                        }
                                    }
                                } else {
                                    // No shared secret for this peer yet, silently drop
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        // Spawn sender task
        let peers_send = peers.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Handle key exchange replies
                    Some(ke_data) = ke_reply_rx.recv() => {
                        let _ = ws_sender.send(WsMessage::Binary(ke_data)).await;
                    }
                    // Handle nickname messages
                    Some((_target, data)) = nickname_rx.recv() => {
                        let _ = ws_sender.send(WsMessage::Binary(data)).await;
                    }
                    // Handle outgoing chat messages
                    Some(outgoing) = msg_rx.recv() => {
                        match outgoing {
                            OutgoingMessage::Direct { target_id, message } => {
                                // Send DM - encrypt with specific peer's key
                                let peers_map = peers_send.read().await;
                                if let Some(peer_info) = peers_map.get(&target_id) {
                                    let serialized = bincode::serialize(&message).unwrap();
                                    match encrypt_message(&peer_info.shared_secret, &serialized) {
                                        Ok((nonce, ciphertext)) => {
                                            let encrypted_msg = Message::Encrypted {
                                                from: session_id.clone(),
                                                nonce,
                                                ciphertext,
                                            };
                                            let data = bincode::serialize(&encrypted_msg).unwrap();
                                            let _ = ws_sender.send(WsMessage::Binary(data)).await;
                                        }
                                        Err(e) => {
                                            let _ = status_tx.send(format!("❌ Encryption failed: {}", e));
                                        }
                                    }
                                } else {
                                    let _ = status_tx.send(format!("❌ No session with peer {}", &target_id[..12.min(target_id.len())]));
                                }
                            }
                            OutgoingMessage::Global(message) => {
                                // Send global message - encrypt once per peer
                                let peers_map = peers_send.read().await;
                                if peers_map.is_empty() {
                                    let _ = status_tx.send("⚠️  No peers connected".to_string());
                                } else {
                                    for (peer_id, peer_info) in peers_map.iter() {
                                        let serialized = bincode::serialize(&message).unwrap();
                                        match encrypt_message(&peer_info.shared_secret, &serialized) {
                                            Ok((nonce, ciphertext)) => {
                                                let encrypted_msg = Message::Encrypted {
                                                    from: session_id.clone(),
                                                    nonce,
                                                    ciphertext,
                                                };
                                                let data = bincode::serialize(&encrypted_msg).unwrap();
                                                let _ = ws_sender.send(WsMessage::Binary(data)).await;
                                            }
                                            Err(e) => {
                                                let _ = status_tx.send(format!("❌ Encryption failed for {}: {}", &peer_id[..12], e));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    else => break,
                }
            }
        });

        Ok((msg_tx, incoming_rx, status_rx, peer_update_rx))
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
