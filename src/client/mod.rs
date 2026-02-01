use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream};

use crate::crypto::{decrypt_message, encrypt_message, Identity};
use crate::protocol::{Message, PlainMessage};

pub struct ChatClient {
    identity: Identity,
    relay_url: String,
    session_id: String,
    shared_secret: Option<Vec<u8>>,
    peer_id: Option<String>,
}

impl ChatClient {
    pub fn new(identity: Identity, relay_url: String) -> Self {
        let session_id = generate_session_id();
        Self {
            identity,
            relay_url,
            session_id,
            shared_secret: None,
            peer_id: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn identity_id(&self) -> String {
        self.identity.public_key_b64()
    }

    pub fn peer_id(&self) -> Option<&str> {
        self.peer_id.as_deref()
    }

    pub async fn connect(&mut self) -> Result<(
        mpsc::UnboundedSender<PlainMessage>,
        mpsc::UnboundedReceiver<PlainMessage>,
        mpsc::UnboundedReceiver<String>, // Status messages
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
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<PlainMessage>();
        let (incoming_tx, incoming_rx) = mpsc::unbounded_channel::<PlainMessage>();
        let (status_tx, status_rx) = mpsc::unbounded_channel::<String>();

        let identity = self.identity.clone_for_thread();
        let session_id = self.session_id.clone();
        let public_key_bytes = self.identity.public_key_bytes();
        let shared_secret = std::sync::Arc::new(tokio::sync::RwLock::new(self.shared_secret.clone()));
        let peer_id = std::sync::Arc::new(tokio::sync::RwLock::new(self.peer_id.clone()));

        // Spawn receiver task
        let shared_secret_recv = shared_secret.clone();
        let peer_id_recv = peer_id.clone();
        let status_tx_recv = status_tx.clone();
        let session_id_recv = session_id.clone();
        let public_key_bytes_recv = public_key_bytes.clone();
        let (ke_reply_tx, mut ke_reply_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(msg) = ws_receiver.next().await {
                if let Ok(WsMessage::Binary(data)) = msg {
                    if let Ok(message) = bincode::deserialize::<Message>(&data) {
                        match message {
                            Message::Ack => {
                                let _ = status_tx_recv.send("Connected to relay".to_string());
                            }
                            Message::KeyExchange { from, public_key } => {
                                // Perform key exchange
                                match identity.key_exchange(&public_key) {
                                    Ok(secret) => {
                                        let already_had_secret = shared_secret_recv.read().await.is_some();
                                        *shared_secret_recv.write().await = Some(secret);
                                        *peer_id_recv.write().await = Some(from.clone());
                                        let _ = status_tx_recv.send(format!("🔐 Encrypted session established with {}", &from[..12]));

                                        // Show join notification
                                        if !already_had_secret {
                                            let join_msg = PlainMessage::system(
                                                from.clone(),
                                                format!("{} has joined", &from[..12]),
                                            );
                                            let _ = incoming_tx.send(join_msg);
                                        }

                                        // Send our public key back so the peer can also complete the exchange
                                        if !already_had_secret {
                                            let reply = Message::KeyExchange {
                                                from: session_id_recv.clone(),
                                                public_key: public_key_bytes_recv.clone(),
                                            };
                                            if let Ok(reply_data) = bincode::serialize(&reply) {
                                                let _ = ke_reply_tx.send(reply_data);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        let _ = status_tx_recv.send(format!("❌ Key exchange failed: {}", e));
                                    }
                                }
                            }
                            Message::Encrypted { nonce, ciphertext, .. } => {
                                let secret = shared_secret_recv.read().await.clone();
                                if let Some(key) = secret {
                                    match decrypt_message(&key, &nonce, &ciphertext) {
                                        Ok(plaintext) => {
                                            if let Ok(plain_msg) = bincode::deserialize::<PlainMessage>(&plaintext) {
                                                let _ = incoming_tx.send(plain_msg);
                                            }
                                        }
                                        Err(e) => {
                                            let _ = status_tx_recv.send(format!("❌ Decryption failed: {}", e));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Peer disconnected - show leave message
            let peer = peer_id_recv.read().await.clone();
            if let Some(pid) = peer {
                let leave_msg = PlainMessage::system(
                    pid.clone(),
                    format!("{} has left", &pid[..12.min(pid.len())]),
                );
                let _ = incoming_tx.send(leave_msg);
            }
        });

        // Spawn sender task
        let shared_secret_send = shared_secret.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Handle key exchange replies (send our public key back to peer)
                    Some(ke_data) = ke_reply_rx.recv() => {
                        let _ = ws_sender.send(WsMessage::Binary(ke_data)).await;
                    }
                    // Handle outgoing chat messages
                    Some(plain_msg) = msg_rx.recv() => {
                        let secret = shared_secret_send.read().await.clone();
                        if let Some(key) = secret {
                            let serialized = bincode::serialize(&plain_msg).unwrap();
                            match encrypt_message(&key, &serialized) {
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
                        }
                    }
                    else => break,
                }
            }
        });

        Ok((msg_tx, incoming_rx, status_rx))
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
