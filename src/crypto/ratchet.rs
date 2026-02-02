//! Double Ratchet Protocol implementation for WSP.
//!
//! Provides forward secrecy and break-in recovery via:
//! - Symmetric-key KDF chains (sending & receiving)
//! - DH ratchet steps using X25519 ephemeral keys
//!
//! Based on Signal's Double Ratchet Algorithm.

use anyhow::Result;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use hkdf::Hkdf;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

/// Max number of skipped message keys to store (prevents DoS via huge gaps)
const MAX_SKIP: u32 = 100;

/// Info strings for HKDF domain separation
const KDF_RK_INFO: &[u8] = b"wsp-ratchet-root";
const KDF_VOICE_INFO: &[u8] = b"wsp-voice-key";
const KDF_SCREEN_INFO: &[u8] = b"wsp-screen-key";

/// Header sent with each ratcheted message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RatchetHeader {
    /// Sender's current DH ratchet public key
    pub dh_public: [u8; 32],
    /// Number of messages in the previous sending chain
    pub prev_chain_len: u32,
    /// Message number in the current sending chain
    pub msg_num: u32,
}

/// A skipped message key, indexed by (DH public key, message number)
#[derive(Hash, Eq, PartialEq, Clone)]
struct SkippedKey {
    dh_public: [u8; 32],
    msg_num: u32,
}

/// The Double Ratchet session state for one peer
pub struct RatchetSession {
    // DH ratchet state
    dh_self_secret: [u8; 32],   // Our current ephemeral secret key (raw bytes)
    dh_self_public: [u8; 32],   // Our current ephemeral public key
    dh_remote: Option<[u8; 32]>, // Their current DH public key (None until first msg received)

    // Root key
    root_key: [u8; 32],

    // Sending chain
    chain_key_send: Option<[u8; 32]>,
    send_msg_num: u32,

    // Receiving chain
    chain_key_recv: Option<[u8; 32]>,
    recv_msg_num: u32,

    // Previous sending chain length (included in headers)
    prev_chain_len: u32,

    // Skipped message keys for out-of-order delivery
    skipped_keys: std::collections::HashMap<SkippedKey, [u8; 32]>,

    // Whether we are "Alice" (the party who initiates the first DH ratchet)
    is_alice: bool,

    // Whether the initial send ratchet has been performed
    initial_ratchet_done: bool,

    // Stable voice base key — derived at init from shared secret, never changes
    // (root_key changes with every ratchet step, so can't use it for voice)
    voice_base_key: [u8; 32],

    // Cached voice key (derived from voice_base_key, stable during call)
    voice_key: Option<[u8; 32]>,

    // Cached screen share key (derived from voice_base_key, stable during session)
    screen_key: Option<[u8; 32]>,
}

impl Drop for RatchetSession {
    fn drop(&mut self) {
        self.dh_self_secret.zeroize();
        self.root_key.zeroize();
        if let Some(ref mut ck) = self.chain_key_send {
            ck.zeroize();
        }
        if let Some(ref mut ck) = self.chain_key_recv {
            ck.zeroize();
        }
        for (_, key) in self.skipped_keys.iter_mut() {
            key.zeroize();
        }
        self.voice_base_key.zeroize();
        if let Some(ref mut vk) = self.voice_key {
            vk.zeroize();
        }
        if let Some(ref mut sk) = self.screen_key {
            sk.zeroize();
        }
    }
}

impl RatchetSession {
    /// Initialize a new ratchet session after X25519 key exchange.
    ///
    /// `shared_secret` is the initial shared secret from the X25519 handshake.
    /// `is_alice` determines who initiates the first DH ratchet (lower session_id = Alice).
    pub fn init(shared_secret: &[u8], is_alice: bool) -> Self {
        // Generate our first ephemeral DH keypair
        let secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let public = PublicKey::from(&secret);

        // Derive root key and initial chain keys from the shared secret
        let hk = Hkdf::<Sha256>::new(None, shared_secret);

        let mut root_key = [0u8; 32];
        hk.expand(KDF_RK_INFO, &mut root_key)
            .expect("HKDF expand failed");

        // Derive separate send/recv chain keys based on role
        // Alice sends on "alice-send", Bob sends on "bob-send"
        // Alice's send = Bob's recv, and vice versa
        let mut chain_a = [0u8; 32];
        let mut chain_b = [0u8; 32];
        hk.expand(b"wsp-chain-alice", &mut chain_a)
            .expect("HKDF expand failed");
        hk.expand(b"wsp-chain-bob", &mut chain_b)
            .expect("HKDF expand failed");

        let (chain_key_send, chain_key_recv) = if is_alice {
            (chain_a, chain_b)
        } else {
            (chain_b, chain_a)
        };

        // Derive a stable voice base key from the shared secret — this never changes
        // with ratchet advancement, so both sides always agree on it
        let mut voice_base_key = [0u8; 32];
        hk.expand(b"wsp-voice-base", &mut voice_base_key)
            .expect("HKDF expand failed");

        Self {
            dh_self_secret: secret.to_bytes(),
            dh_self_public: *public.as_bytes(),
            dh_remote: None,
            root_key,
            chain_key_send: Some(chain_key_send),
            chain_key_recv: Some(chain_key_recv),
            send_msg_num: 0,
            recv_msg_num: 0,
            prev_chain_len: 0,
            skipped_keys: std::collections::HashMap::new(),
            is_alice,
            initial_ratchet_done: false,
            voice_base_key,
            voice_key: None,
            screen_key: None,
        }
    }

    /// Get our current DH ratchet public key (included in message headers)
    pub fn public_key(&self) -> [u8; 32] {
        self.dh_self_public
    }

    /// Set the remote peer's initial DH public key (from key exchange).
    /// This lets us detect when they do a DH ratchet step (their key changes),
    /// which is critical for the first message after Alice's initial ratchet.
    pub fn set_remote_dh(&mut self, remote_dh: [u8; 32]) {
        if self.dh_remote.is_none() {
            self.dh_remote = Some(remote_dh);
        }
    }

    /// Encrypt a plaintext message, returning (header, nonce, ciphertext).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<(RatchetHeader, Vec<u8>, Vec<u8>)> {
        // If we're Alice, have received their DH key, and haven't done the initial
        // send ratchet yet, do it now to start the DH ratchet chain
        if self.is_alice && self.dh_remote.is_some() && !self.initial_ratchet_done {
            self.ratchet_send()?;
            self.initial_ratchet_done = true;
        }

        let chain_key = self
            .chain_key_send
            .ok_or_else(|| anyhow::anyhow!("No sending chain key"))?;

        // Derive message key from chain key
        let (new_chain_key, message_key) = kdf_chain(&chain_key);
        self.chain_key_send = Some(new_chain_key);

        let header = RatchetHeader {
            dh_public: self.dh_self_public,
            prev_chain_len: self.prev_chain_len,
            msg_num: self.send_msg_num,
        };
        self.send_msg_num += 1;

        // Encrypt with message key
        let (nonce, ciphertext) = encrypt_with_key(&message_key, plaintext)?;

        Ok((header, nonce, ciphertext))
    }

    /// Decrypt a message given its header, nonce, and ciphertext.
    pub fn decrypt(
        &mut self,
        header: &RatchetHeader,
        nonce: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        // 1. Try skipped message keys first
        let skip_key = SkippedKey {
            dh_public: header.dh_public,
            msg_num: header.msg_num,
        };
        if let Some(mk) = self.skipped_keys.remove(&skip_key) {
            return decrypt_with_key(&mk, nonce, ciphertext);
        }

        // 2. Check if we need a DH ratchet step (new DH key from peer)
        let need_dh_ratchet = match self.dh_remote {
            None => {
                self.dh_remote = Some(header.dh_public);
                false
            }
            Some(remote) if remote != header.dh_public => true,
            _ => false,
        };

        if need_dh_ratchet {
            self.skip_message_keys(header.prev_chain_len)?;
            self.ratchet_recv(&header.dh_public)?;
        }

        // 3. Skip ahead if msg_num > recv_msg_num (out-of-order within same chain)
        self.skip_message_keys(header.msg_num)?;

        // 4. Derive message key from current receiving chain
        let chain_key = self
            .chain_key_recv
            .ok_or_else(|| anyhow::anyhow!("No receiving chain key"))?;

        let (new_chain_key, message_key) = kdf_chain(&chain_key);
        self.chain_key_recv = Some(new_chain_key);
        self.recv_msg_num += 1;

        decrypt_with_key(&message_key, nonce, ciphertext)
    }

    /// Derive (or return cached) voice key for audio encryption.
    /// Uses the stable voice_base_key (derived at init from shared secret),
    /// NOT the root key (which changes with every DH ratchet step).
    /// Both sides always derive the same voice key regardless of ratchet state.
    pub fn derive_voice_key(&mut self) -> [u8; 32] {
        if let Some(vk) = self.voice_key {
            return vk;
        }
        let hk = Hkdf::<Sha256>::new(None, &self.voice_base_key);
        let mut vk = [0u8; 32];
        hk.expand(KDF_VOICE_INFO, &mut vk)
            .expect("HKDF expand failed");
        self.voice_key = Some(vk);
        vk
    }

    /// Derive (or return cached) screen share key for frame encryption.
    /// Uses the stable voice_base_key with different HKDF info.
    pub fn derive_screen_key(&mut self) -> [u8; 32] {
        if let Some(sk) = self.screen_key {
            return sk;
        }
        let hk = Hkdf::<Sha256>::new(None, &self.voice_base_key);
        let mut sk = [0u8; 32];
        hk.expand(KDF_SCREEN_INFO, &mut sk)
            .expect("HKDF expand failed");
        self.screen_key = Some(sk);
        sk
    }

    /// Clear cached screen share key
    pub fn clear_screen_key(&mut self) {
        if let Some(ref mut sk) = self.screen_key {
            sk.zeroize();
        }
        self.screen_key = None;
    }

    /// Clear cached voice key (call this when a voice call ends)
    pub fn clear_voice_key(&mut self) {
        if let Some(ref mut vk) = self.voice_key {
            vk.zeroize();
        }
        self.voice_key = None;
    }

    /// Perform a DH ratchet step on the sending side.
    /// Generates a new ephemeral keypair and derives new root + send chain keys.
    fn ratchet_send(&mut self) -> Result<()> {
        let remote_pub = self
            .dh_remote
            .ok_or_else(|| anyhow::anyhow!("No remote DH key for send ratchet"))?;

        // Save previous send chain length
        self.prev_chain_len = self.send_msg_num;
        self.send_msg_num = 0;

        // Generate new ephemeral keypair
        let new_secret = StaticSecret::random_from_rng(rand::rngs::OsRng);
        let new_public = PublicKey::from(&new_secret);

        // DH with our NEW secret and their current public key
        let remote_key = PublicKey::from(remote_pub);
        let dh_output = new_secret.diffie_hellman(&remote_key);

        // KDF: derive new root key and send chain key
        let (new_root_key, new_chain_key) = kdf_root(&self.root_key, dh_output.as_bytes());

        self.root_key = new_root_key;
        self.chain_key_send = Some(new_chain_key);
        self.dh_self_secret = new_secret.to_bytes();
        self.dh_self_public = *new_public.as_bytes();

        Ok(())
    }

    /// Perform a DH ratchet step on the receiving side.
    /// Uses the peer's new DH public key to derive new root + recv chain keys,
    /// then immediately does a send ratchet to generate a fresh keypair.
    fn ratchet_recv(&mut self, new_remote_pub: &[u8; 32]) -> Result<()> {
        self.dh_remote = Some(*new_remote_pub);

        // DH with our CURRENT secret and their NEW public key
        let remote_key = PublicKey::from(*new_remote_pub);
        let self_secret = StaticSecret::from(self.dh_self_secret);
        let dh_output = self_secret.diffie_hellman(&remote_key);

        // KDF: derive new root key and recv chain key
        let (new_root_key, new_chain_key) = kdf_root(&self.root_key, dh_output.as_bytes());

        self.root_key = new_root_key;
        self.chain_key_recv = Some(new_chain_key);
        self.recv_msg_num = 0;

        // Now do a send ratchet (generate new keypair for our next outgoing message)
        self.ratchet_send()?;

        Ok(())
    }

    /// Skip message keys up to `until` in the current receiving chain,
    /// storing them for out-of-order decryption.
    fn skip_message_keys(&mut self, until: u32) -> Result<()> {
        if until < self.recv_msg_num {
            return Ok(()); // Nothing to skip
        }
        let num_to_skip = until - self.recv_msg_num;
        if num_to_skip > MAX_SKIP {
            return Err(anyhow::anyhow!(
                "Too many skipped messages ({} > {})",
                num_to_skip,
                MAX_SKIP
            ));
        }

        let chain_key = match self.chain_key_recv {
            Some(ck) => ck,
            None => return Ok(()), // No chain yet
        };

        let dh_pub = match self.dh_remote {
            Some(dh) => dh,
            None => return Ok(()),
        };

        let mut ck = chain_key;
        for _ in self.recv_msg_num..until {
            let (new_ck, mk) = kdf_chain(&ck);
            let skip_key = SkippedKey {
                dh_public: dh_pub,
                msg_num: self.recv_msg_num,
            };
            self.skipped_keys.insert(skip_key, mk);
            ck = new_ck;
            self.recv_msg_num += 1;
        }
        self.chain_key_recv = Some(ck);

        // Evict oldest skipped keys if we're over the limit
        while self.skipped_keys.len() > MAX_SKIP as usize {
            // Remove an arbitrary entry (HashMap doesn't have pop_first easily)
            if let Some(key) = self.skipped_keys.keys().next().cloned() {
                self.skipped_keys.remove(&key);
            }
        }

        Ok(())
    }
}

/// KDF for root key chain: (root_key, dh_output) → (new_root_key, chain_key)
fn kdf_root(root_key: &[u8; 32], dh_output: &[u8]) -> ([u8; 32], [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(root_key), dh_output);
    let mut new_root = [0u8; 32];
    let mut chain_key = [0u8; 32];
    hk.expand(b"wsp-root", &mut new_root)
        .expect("HKDF expand failed");
    hk.expand(b"wsp-chain", &mut chain_key)
        .expect("HKDF expand failed");
    (new_root, chain_key)
}

/// KDF for chain key advancement: chain_key → (new_chain_key, message_key)
fn kdf_chain(chain_key: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    // Use BLAKE3 for speed in the hot path (message-level)
    let new_chain = blake3::keyed_hash(chain_key, b"chain");
    let msg_key = blake3::keyed_hash(chain_key, b"message");
    (*new_chain.as_bytes(), *msg_key.as_bytes())
}

/// Encrypt with a one-time message key
fn encrypt_with_key(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let cipher = ChaCha20Poly1305::new(key.into());
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|_| anyhow::anyhow!("Ratchet encryption failed"))?;
    Ok((nonce_bytes.to_vec(), ciphertext))
}

/// Decrypt with a one-time message key
fn decrypt_with_key(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(nonce.len() == 12, "Nonce must be 12 bytes");
    let cipher = ChaCha20Poly1305::new(key.into());
    let nonce = Nonce::from_slice(nonce);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("Ratchet decryption failed"))?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_ratchet() {
        let shared = [42u8; 32];
        let mut alice = RatchetSession::init(&shared, true);
        let mut bob = RatchetSession::init(&shared, false);

        // Exchange initial DH keys (simulates KE reply carrying dh_ratchet_key)
        let alice_dh = alice.public_key();
        let bob_dh = bob.public_key();
        alice.set_remote_dh(bob_dh);
        bob.set_remote_dh(alice_dh);

        // Alice sends, Bob receives
        let (header, nonce, ct) = alice.encrypt(b"hello from alice").unwrap();
        let pt = bob.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"hello from alice");

        // Bob sends, Alice receives
        let (header, nonce, ct) = bob.encrypt(b"hello from bob").unwrap();
        let pt = alice.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"hello from bob");
    }

    #[test]
    fn test_bob_sends_first_then_alice_replies() {
        // This was the original bug: Bob sends first, Alice replies with
        // a ratcheted chain, Bob can't decrypt.
        let shared = [42u8; 32];
        let mut alice = RatchetSession::init(&shared, true);
        let mut bob = RatchetSession::init(&shared, false);

        // Exchange initial DH keys (from KE handshake)
        let alice_dh = alice.public_key();
        let bob_dh = bob.public_key();
        alice.set_remote_dh(bob_dh);
        bob.set_remote_dh(alice_dh);

        // Bob sends first
        let (header, nonce, ct) = bob.encrypt(b"hey").unwrap();
        let pt = alice.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"hey");

        // Alice replies — this triggers her initial ratchet_send
        let (header, nonce, ct) = alice.encrypt(b"hey back").unwrap();
        // Bob must detect the DH key change and do ratchet_recv
        let pt = bob.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"hey back");

        // Continue conversation
        let (header, nonce, ct) = bob.encrypt(b"how are you?").unwrap();
        let pt = alice.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"how are you?");
    }

    #[test]
    fn test_multiple_messages() {
        let shared = [42u8; 32];
        let mut alice = RatchetSession::init(&shared, true);
        let mut bob = RatchetSession::init(&shared, false);

        // Exchange initial DH keys
        let alice_dh = alice.public_key();
        let bob_dh = bob.public_key();
        alice.set_remote_dh(bob_dh);
        bob.set_remote_dh(alice_dh);

        for i in 0..10 {
            let msg = format!("alice msg {}", i);
            let (header, nonce, ct) = alice.encrypt(msg.as_bytes()).unwrap();
            let pt = bob.decrypt(&header, &nonce, &ct).unwrap();
            assert_eq!(pt, msg.as_bytes());
        }

        for i in 0..10 {
            let msg = format!("bob msg {}", i);
            let (header, nonce, ct) = bob.encrypt(msg.as_bytes()).unwrap();
            let pt = alice.decrypt(&header, &nonce, &ct).unwrap();
            assert_eq!(pt, msg.as_bytes());
        }
    }

    #[test]
    fn test_alternating_messages_triggers_ratchet() {
        let shared = [42u8; 32];
        let mut alice = RatchetSession::init(&shared, true);
        let mut bob = RatchetSession::init(&shared, false);

        // Exchange initial DH keys
        let alice_dh = alice.public_key();
        let bob_dh = bob.public_key();
        alice.set_remote_dh(bob_dh);
        bob.set_remote_dh(alice_dh);

        // Alice → Bob
        let (h, n, c) = alice.encrypt(b"a1").unwrap();
        assert_eq!(bob.decrypt(&h, &n, &c).unwrap(), b"a1");

        // Bob → Alice (Bob now has Alice's DH key, will ratchet on reply)
        let (h, n, c) = bob.encrypt(b"b1").unwrap();
        assert_eq!(alice.decrypt(&h, &n, &c).unwrap(), b"b1");

        // Alice → Bob (new DH key from Alice)
        let (h, n, c) = alice.encrypt(b"a2").unwrap();
        assert_eq!(bob.decrypt(&h, &n, &c).unwrap(), b"a2");

        // Bob → Alice again
        let (h, n, c) = bob.encrypt(b"b2").unwrap();
        assert_eq!(alice.decrypt(&h, &n, &c).unwrap(), b"b2");
    }

    #[test]
    fn test_no_dh_exchange_symmetric_still_works() {
        // Without DH key exchange (legacy/fallback), symmetric chains work
        // as long as Alice doesn't trigger the initial ratchet
        let shared = [42u8; 32];
        let mut alice = RatchetSession::init(&shared, true);
        let mut bob = RatchetSession::init(&shared, false);

        // Alice sends FIRST (before receiving from Bob, so dh_remote is None)
        // → no initial ratchet triggered → uses symmetric chain
        let (header, nonce, ct) = alice.encrypt(b"alice first").unwrap();
        let pt = bob.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"alice first");

        // Bob replies
        let (header, nonce, ct) = bob.encrypt(b"bob reply").unwrap();
        let pt = alice.decrypt(&header, &nonce, &ct).unwrap();
        assert_eq!(pt, b"bob reply");
    }

    #[test]
    fn test_voice_key_derivation() {
        let shared = [42u8; 32];
        let mut alice = RatchetSession::init(&shared, true);
        let mut bob = RatchetSession::init(&shared, false);

        let vk_a = alice.derive_voice_key();
        let vk_b = bob.derive_voice_key();
        assert_eq!(vk_a, vk_b, "Voice keys should match");

        // Calling again should return cached value
        assert_eq!(alice.derive_voice_key(), vk_a);

        // Clear and re-derive should still match
        alice.clear_voice_key();
        assert_eq!(alice.derive_voice_key(), vk_a);
    }
}
