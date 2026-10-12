//! Ed25519 keys. Every device and the Hub has its own key pair; the private
//! half never leaves the machine that made it.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

/// A private key with its public half.
pub struct KeyPair(SigningKey);

impl KeyPair {
    /// A new key from the system's random number generator.
    pub fn generate() -> KeyPair {
        KeyPair::from_seed(random())
    }

    pub fn from_seed(seed: [u8; 32]) -> KeyPair {
        KeyPair(SigningKey::from_bytes(&seed))
    }

    /// The 32 bytes the key is made from, to store it.
    pub fn seed(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key().to_bytes())
    }

    /// A signature over `message`, base64.
    pub fn sign(&self, message: &[u8]) -> String {
        STANDARD.encode(self.0.sign(message).to_bytes())
    }
}

impl fmt::Debug for KeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyPair({})", self.public().fingerprint())
    }
}

/// A public key: 32 bytes, written as base64.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn from_bytes(bytes: [u8; 32]) -> PublicKey {
        PublicKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    pub fn from_base64(text: &str) -> Option<PublicKey> {
        let bytes = STANDARD.decode(text.trim()).ok()?;
        let bytes: [u8; 32] = bytes.try_into().ok()?;
        // Reject points that aren't keys.
        VerifyingKey::from_bytes(&bytes).ok()?;
        Some(PublicKey(bytes))
    }

    /// What people compare to know they have the right key:
    /// `ABCD-EFGH-JKMN-PQRS`, 80 bits of the key's SHA-256.
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.0)
    }

    /// Whether `signature` (base64) is this key's signature over `message`.
    pub fn verify(&self, message: &[u8], signature: &str) -> bool {
        let Ok(key) = VerifyingKey::from_bytes(&self.0) else {
            return false;
        };
        let Some(bytes) = STANDARD
            .decode(signature.trim())
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b).ok())
        else {
            return false;
        };
        key.verify_strict(message, &Signature::from_bytes(&bytes))
            .is_ok()
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint())
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_base64())
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        PublicKey::from_base64(&text).ok_or_else(|| serde::de::Error::custom("not a public key"))
    }
}

fn fingerprint_of(key: &[u8]) -> String {
    let hash = Sha256::digest(key);
    let text = crockford(&hash[..10]);
    text.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

/// Crockford's base32: digits and letters without I, L, O and U, so codes
/// read aloud or typed can't be mistaken.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub(crate) fn crockford(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut buffer = 0u32;
    let mut bits = 0;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(CROCKFORD[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(CROCKFORD[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// A typed code in Crockford's base32, uppercased, with dashes and spaces
/// removed and look-alike letters mapped to digits (O to 0, I and L to 1).
pub fn normalize_code(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '-' | ' ' | '_'))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

/// Random bytes from the system.
pub fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the system has no random number generator");
    bytes
}

/// Random text for ids and nonces: `n` random bytes in Crockford base32.
pub fn random_id<const N: usize>() -> String {
    crockford(&random::<N>())
}

/// SHA-256 of `bytes` in lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_verifies() {
        let key = KeyPair::from_seed([7; 32]);
        let public = key.public();
        let signature = key.sign(b"hello");
        assert!(public.verify(b"hello", &signature));
        assert!(!public.verify(b"hellO", &signature));
        assert!(!KeyPair::generate().public().verify(b"hello", &signature));
        assert!(!public.verify(b"hello", "not base64!"));

        let text = serde_json::to_string(&public).unwrap();
        assert_eq!(serde_json::from_str::<PublicKey>(&text).unwrap(), public);
        assert!(serde_json::from_str::<PublicKey>("\"AAAA\"").is_err());
    }

    #[test]
    fn fingerprints_are_grouped_crockford() {
        let fp = KeyPair::from_seed([1; 32]).public().fingerprint();
        assert_eq!(fp.len(), 19);
        assert_eq!(fp.matches('-').count(), 3);
        assert!(
            fp.chars()
                .all(|c| c == '-' || CROCKFORD.contains(&(c as u8)))
        );
        assert_ne!(fp, KeyPair::from_seed([2; 32]).public().fingerprint());
    }

    #[test]
    fn base32_and_typed_codes() {
        assert_eq!(crockford(&[0xff, 0xff]), "ZZZG");
        assert_eq!(crockford(&[0, 0, 0, 0, 0]), "00000000");
        assert_eq!(normalize_code("abcd-efgh io l"), "ABCDEFGH101");
        assert_eq!(random_id::<10>().len(), 16);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
