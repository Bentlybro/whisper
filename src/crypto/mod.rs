pub mod ratchet;
pub mod safety_number;

use anyhow::Result;
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::path::Path;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

/// User's identity keypair
#[derive(Zeroize, Serialize, Deserialize)]
#[zeroize(drop)]
pub struct Identity {
    #[serde(with = "secret_serde")]
    secret_key: StaticSecret,
    #[serde(with = "public_key_serde")]
    public_key: PublicKey,
}

impl Identity {
    /// Generate a new random identity
    pub fn generate() -> Self {
        let secret_key = StaticSecret::random_from_rng(OsRng);
        let public_key = PublicKey::from(&secret_key);
        Self {
            secret_key,
            public_key,
        }
    }

    /// Get public key as bytes
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.public_key.as_bytes().to_vec()
    }

    /// Get public key as base64 string (this is the user's ID)
    pub fn public_key_b64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(self.public_key.as_bytes())
    }

    /// Perform X25519 key exchange
    pub fn key_exchange(&self, peer_public_key: &[u8]) -> Result<Vec<u8>> {
        let peer_key = PublicKey::from(<[u8; 32]>::try_from(peer_public_key)?);
        let shared_secret = self.secret_key.diffie_hellman(&peer_key);
        
        // Derive symmetric key using BLAKE3
        let key = blake3::hash(shared_secret.as_bytes());
        Ok(key.as_bytes().to_vec())
    }

    /// Save identity to disk (encrypted with password)
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P, password: &str) -> Result<()> {
        let serialized = bincode::serialize(self)?;
        
        // Derive key from password
        let key_hash = blake3::hash(password.as_bytes());
        let cipher = ChaCha20Poly1305::new(key_hash.as_bytes().into());
        
        // Generate random nonce
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        
        let ciphertext = cipher
            .encrypt(nonce, serialized.as_ref())
            .map_err(|_| anyhow::anyhow!("Failed to encrypt identity"))?;
        
        let mut output = nonce_bytes.to_vec();
        output.extend(ciphertext);
        
        std::fs::write(path, output)?;
        Ok(())
    }

    /// Load identity from disk (decrypt with password)
    pub fn load_from_file<P: AsRef<Path>>(path: P, password: &str) -> Result<Self> {
        let data = std::fs::read(path)?;
        
        anyhow::ensure!(data.len() > 12, "Invalid identity file");
        
        let nonce = Nonce::from_slice(&data[..12]);
        let ciphertext = &data[12..];
        
        // Derive key from password
        let key_hash = blake3::hash(password.as_bytes());
        let cipher = ChaCha20Poly1305::new(key_hash.as_bytes().into());
        
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| anyhow::anyhow!("Failed to decrypt identity (wrong password?)"))?;
        
        let identity = bincode::deserialize(&plaintext)?;
        Ok(identity)
    }
}

/// Encrypt a message using ChaCha20Poly1305
pub fn encrypt_message(key: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    anyhow::ensure!(key.len() == 32, "Key must be 32 bytes");
    
    let cipher = ChaCha20Poly1305::new(key.into());
    
    // Generate random nonce
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| anyhow::anyhow!("Encryption failed"))?;
    
    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Decrypt a message using ChaCha20Poly1305
pub fn decrypt_message(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(key.len() == 32, "Key must be 32 bytes");
    anyhow::ensure!(nonce.len() == 12, "Nonce must be 12 bytes");
    
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce);
    
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Decryption failed (wrong key or corrupted data)"))?;
    
    Ok(plaintext)
}

/// Custom serialization for StaticSecret
mod secret_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(secret: &StaticSecret, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(secret.to_bytes().as_ref())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<StaticSecret, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: Vec<u8> = serde::Deserialize::deserialize(deserializer)?;
        let array: [u8; 32] = bytes.try_into()
            .map_err(|_| serde::de::Error::custom("Invalid key length"))?;
        Ok(StaticSecret::from(array))
    }
}

/// Custom serialization for PublicKey
mod public_key_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(public_key: &PublicKey, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(public_key.as_bytes())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<PublicKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes: Vec<u8> = serde::Deserialize::deserialize(deserializer)?;
        let array: [u8; 32] = bytes.try_into()
            .map_err(|_| serde::de::Error::custom("Invalid key length"))?;
        Ok(PublicKey::from(array))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_exchange() {
        let alice = Identity::generate();
        let bob = Identity::generate();

        let alice_shared = alice.key_exchange(&bob.public_key_bytes()).unwrap();
        let bob_shared = bob.key_exchange(&alice.public_key_bytes()).unwrap();

        assert_eq!(alice_shared, bob_shared);
    }

    #[test]
    fn test_encryption() {
        let key = vec![0u8; 32];
        let message = b"Hello, World!";

        let (nonce, ciphertext) = encrypt_message(&key, message).unwrap();
        let plaintext = decrypt_message(&key, &nonce, &ciphertext).unwrap();

        assert_eq!(plaintext, message);
    }
}
