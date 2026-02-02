//! Safety Number generation for verifying E2EE sessions.
//!
//! Both peers derive the same safety number from their public keys.
//! If the numbers match (compared out-of-band), there's no MITM.

use sha2::{Sha256, Digest};

/// A set of emojis used for visual fingerprints (64 distinct, easy to distinguish)
const EMOJI_ALPHABET: &[&str] = &[
    "🔑", "🌊", "🎸", "🏔️", "🦊", "🌙", "⚡", "🎯",
    "🦋", "🌺", "🎪", "🚀", "🐉", "💎", "🌈", "🔥",
    "🎭", "🦁", "🌻", "⭐", "🎵", "🐺", "🌴", "🎲",
    "🦅", "🌸", "🎩", "💫", "🐬", "🌿", "🎪", "🔮",
    "🦜", "🌾", "🎻", "🌟", "🐙", "🍀", "🎨", "💥",
    "🦈", "🌵", "🎹", "✨", "🐝", "🌹", "🎬", "🌊",
    "🦉", "🍁", "🎺", "💠", "🐋", "🌼", "🎳", "🔷",
    "🦚", "🌱", "🎷", "💜", "🐧", "🌳", "🎶", "🔶",
];

/// Compute a safety number from two public keys.
///
/// The result is deterministic and identical regardless of which side computes it,
/// because we sort the keys before hashing.
pub fn compute_safety_number(my_pubkey: &[u8], peer_pubkey: &[u8]) -> SafetyNumber {
    // Sort keys so both sides produce the same hash
    let (first, second) = if my_pubkey <= peer_pubkey {
        (my_pubkey, peer_pubkey)
    } else {
        (peer_pubkey, my_pubkey)
    };

    // Hash: SHA-256( "WSP-SAFETY-NUMBER-v1" || len(first) || first || len(second) || second )
    // Using a domain separator and lengths to prevent ambiguity
    let mut hasher = Sha256::new();
    hasher.update(b"WSP-SAFETY-NUMBER-v1");
    hasher.update((first.len() as u32).to_le_bytes());
    hasher.update(first);
    hasher.update((second.len() as u32).to_le_bytes());
    hasher.update(second);

    let hash = hasher.finalize();

    SafetyNumber {
        hash: hash.into(),
    }
}

/// A computed safety number that can be displayed in multiple formats.
#[derive(Clone, Debug)]
pub struct SafetyNumber {
    hash: [u8; 32],
}

impl SafetyNumber {
    /// Format as a numeric code: 5 groups of 5 digits (like Signal)
    /// e.g. "34521 78903 12456 90834 56721"
    pub fn numeric(&self) -> String {
        // Use first 20 bytes, each 4 bytes → a 5-digit number (mod 100000)
        let groups: Vec<String> = (0..5)
            .map(|i| {
                let offset = i * 4;
                let val = u32::from_le_bytes([
                    self.hash[offset],
                    self.hash[offset + 1],
                    self.hash[offset + 2],
                    self.hash[offset + 3],
                ]);
                format!("{:05}", val % 100000)
            })
            .collect();
        groups.join(" ")
    }

    /// Format as an emoji fingerprint (8 emojis)
    pub fn emoji(&self) -> String {
        // Use bytes 20-27 to pick emojis
        (20..28)
            .map(|i| {
                let idx = self.hash[i] as usize % EMOJI_ALPHABET.len();
                EMOJI_ALPHABET[idx]
            })
            .collect::<Vec<&str>>()
            .join("")
    }

    /// Short numeric (first 3 groups) for compact display
    pub fn short_numeric(&self) -> String {
        let full = self.numeric();
        full.split_whitespace()
            .take(3)
            .collect::<Vec<&str>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safety_number_symmetric() {
        let key_a = vec![1u8; 32];
        let key_b = vec![2u8; 32];

        let sn1 = compute_safety_number(&key_a, &key_b);
        let sn2 = compute_safety_number(&key_b, &key_a);

        assert_eq!(sn1.numeric(), sn2.numeric());
        assert_eq!(sn1.emoji(), sn2.emoji());
    }

    #[test]
    fn test_safety_number_different_keys() {
        let key_a = vec![1u8; 32];
        let key_b = vec![2u8; 32];
        let key_c = vec![3u8; 32];

        let sn_ab = compute_safety_number(&key_a, &key_b);
        let sn_ac = compute_safety_number(&key_a, &key_c);

        assert_ne!(sn_ab.numeric(), sn_ac.numeric());
    }

    #[test]
    fn test_numeric_format() {
        let key_a = vec![42u8; 32];
        let key_b = vec![99u8; 32];
        let sn = compute_safety_number(&key_a, &key_b);

        let numeric = sn.numeric();
        let groups: Vec<&str> = numeric.split_whitespace().collect();
        assert_eq!(groups.len(), 5);
        for g in &groups {
            assert_eq!(g.len(), 5);
            assert!(g.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn test_emoji_format() {
        let key_a = vec![42u8; 32];
        let key_b = vec![99u8; 32];
        let sn = compute_safety_number(&key_a, &key_b);
        let emoji = sn.emoji();
        // Should have 8 emoji characters (though they may be multi-byte)
        assert!(!emoji.is_empty());
    }
}
