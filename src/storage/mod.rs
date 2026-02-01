use anyhow::Result;
use std::path::Path;

use crate::crypto::{decrypt_message, encrypt_message};
use crate::protocol::PlainMessage;

/// Encrypted chat history storage
pub struct HistoryStorage {
    path: std::path::PathBuf,
    key: Vec<u8>,
}

impl HistoryStorage {
    pub fn new<P: AsRef<Path>>(path: P, encryption_key: &[u8]) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            key: encryption_key.to_vec(),
        }
    }

    /// Save a message to encrypted storage
    pub fn save_message(&self, msg: &PlainMessage) -> Result<()> {
        let serialized = rmp_serde::to_vec(msg)?;
        let (nonce, ciphertext) = encrypt_message(&self.key, &serialized)?;
        
        let mut data = nonce;
        data.extend(ciphertext);
        
        // Append to file
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        
        // Write length prefix
        file.write_all(&(data.len() as u32).to_le_bytes())?;
        file.write_all(&data)?;
        
        Ok(())
    }

    /// Load all messages from encrypted storage
    pub fn load_messages(&self) -> Result<Vec<PlainMessage>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        use std::io::Read;
        let mut file = std::fs::File::open(&self.path)?;
        let mut messages = Vec::new();

        loop {
            let mut len_bytes = [0u8; 4];
            match file.read_exact(&mut len_bytes) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }

            let len = u32::from_le_bytes(len_bytes) as usize;
            let mut data = vec![0u8; len];
            file.read_exact(&mut data)?;

            if data.len() < 12 {
                continue; // Invalid entry
            }

            let nonce = &data[..12];
            let ciphertext = &data[12..];

            match decrypt_message(&self.key, nonce, ciphertext) {
                Ok(plaintext) => {
                    if let Ok(msg) = rmp_serde::from_slice::<PlainMessage>(&plaintext)
                        .or_else(|_| bincode::deserialize::<PlainMessage>(&plaintext)) {
                        messages.push(msg);
                    }
                }
                Err(_) => continue, // Skip corrupted messages
            }
        }

        Ok(messages)
    }
}
