//! How each side proves who said something.
//!
//! - Every request a device makes carries a signature over the method, path,
//!   time, a random nonce and a hash of the body. The Hub accepts it only
//!   within [`MAX_SKEW`] of its own clock and only once per nonce, so a
//!   recorded request can't be replayed.
//! - Things passed on through the Hub (commands from the Hub, decisions from
//!   an approver's phone) are [`Signed`] envelopes, checked by whoever acts
//!   on them, so the Hub can't forge a decision and a fake Hub can't forge a
//!   command.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::keys::{KeyPair, PublicKey, random_id, sha256_hex};

/// Request headers of a signed device request.
pub const DEVICE_HEADER: &str = "x-disktree-device";
pub const TIME_HEADER: &str = "x-disktree-time";
pub const NONCE_HEADER: &str = "x-disktree-nonce";
pub const SIGNATURE_HEADER: &str = "x-disktree-signature";

/// How far a request's time may be from the Hub's clock, in seconds.
pub const MAX_SKEW: i64 = 300;

/// The signature headers of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestSignature {
    pub device: String,
    pub time: i64,
    pub nonce: String,
    pub signature: String,
}

/// What a request signature covers. `path` includes the query string.
fn request_message(method: &str, path: &str, time: i64, nonce: &str, body: &[u8]) -> Vec<u8> {
    format!(
        "disktree-request-v1\n{}\n{path}\n{time}\n{nonce}\n{}",
        method.to_ascii_uppercase(),
        sha256_hex(body)
    )
    .into_bytes()
}

/// Signs a request from device `device` at `time` (seconds since 1970).
pub fn sign_request(
    key: &KeyPair,
    device: &str,
    method: &str,
    path: &str,
    body: &[u8],
    time: i64,
) -> RequestSignature {
    let nonce = random_id::<16>();
    let signature = key.sign(&request_message(method, path, time, &nonce, body));
    RequestSignature {
        device: device.to_string(),
        time,
        nonce,
        signature,
    }
}

/// Whether `signature` is `key`'s, for this request. The caller checks the
/// time and that the nonce is new.
pub fn verify_request(
    key: &PublicKey,
    method: &str,
    path: &str,
    body: &[u8],
    signature: &RequestSignature,
) -> bool {
    key.verify(
        &request_message(method, path, signature.time, &signature.nonce, body),
        &signature.signature,
    )
}

/// What an envelope is for, so a signature made for one purpose can't be
/// passed off as another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    PairRequest,
    PairResponse,
    Command,
    Decision,
    /// A [`crate::HubAddress`].
    Move,
}

impl Purpose {
    fn label(self) -> &'static str {
        match self {
            Purpose::PairRequest => "pair-request",
            Purpose::PairResponse => "pair-response",
            Purpose::Command => "command",
            Purpose::Decision => "decision",
            Purpose::Move => "move",
        }
    }
}

/// A value signed by one key, kept as the exact JSON text that was signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Signed<T> {
    /// The value as JSON.
    pub body: String,
    /// Fingerprint of the signing key.
    pub signer: String,
    pub signature: String,
    #[serde(skip)]
    kind: PhantomData<fn() -> T>,
}

/// Why an envelope couldn't be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    WrongSigner,
    BadSignature,
    Malformed(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::WrongSigner => write!(f, "signed by a different key"),
            OpenError::BadSignature => write!(f, "the signature doesn't match"),
            OpenError::Malformed(why) => write!(f, "malformed: {why}"),
        }
    }
}

impl std::error::Error for OpenError {}

fn envelope_message(purpose: Purpose, body: &str) -> Vec<u8> {
    format!("disktree-{}-v1\n{body}", purpose.label()).into_bytes()
}

impl<T: Serialize + DeserializeOwned> Signed<T> {
    pub fn sign(value: &T, key: &KeyPair, purpose: Purpose) -> Signed<T> {
        let body = serde_json::to_string(value).expect("messages serialize");
        let signature = key.sign(&envelope_message(purpose, &body));
        Signed {
            body,
            signer: key.public().fingerprint(),
            signature,
            kind: PhantomData,
        }
    }

    /// The value, if `key` signed it for `purpose`.
    pub fn open(&self, key: &PublicKey, purpose: Purpose) -> Result<T, OpenError> {
        if self.signer != key.fingerprint() {
            return Err(OpenError::WrongSigner);
        }
        if !key.verify(&envelope_message(purpose, &self.body), &self.signature) {
            return Err(OpenError::BadSignature);
        }
        serde_json::from_str(&self.body).map_err(|e| OpenError::Malformed(e.to_string()))
    }

    /// The value without checking the signature, for showing or routing it.
    /// Never act on it without [`Signed::open`].
    pub fn peek(&self) -> Result<T, OpenError> {
        serde_json::from_str(&self.body).map_err(|e| OpenError::Malformed(e.to_string()))
    }

    /// SHA-256 of the signed text, to refer to exactly this value.
    pub fn hash(&self) -> String {
        sha256_hex(self.body.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_verify_only_unchanged() {
        let key = KeyPair::from_seed([3; 32]);
        let sig = sign_request(&key, "dev1", "post", "/api/v1/report", b"{}", 1_700_000_000);
        let public = key.public();
        assert!(verify_request(
            &public,
            "POST",
            "/api/v1/report",
            b"{}",
            &sig
        ));
        assert!(!verify_request(
            &public,
            "POST",
            "/api/v1/poll",
            b"{}",
            &sig
        ));
        assert!(!verify_request(
            &public,
            "POST",
            "/api/v1/report",
            b"{ }",
            &sig
        ));
        let later = RequestSignature {
            time: sig.time + 1,
            ..sig.clone()
        };
        assert!(!verify_request(
            &public,
            "POST",
            "/api/v1/report",
            b"{}",
            &later
        ));
        let other_nonce = sign_request(&key, "dev1", "POST", "/api/v1/report", b"{}", sig.time);
        assert_ne!(other_nonce.nonce, sig.nonce);
    }

    #[test]
    fn envelopes_check_signer_and_purpose() {
        let hub = KeyPair::from_seed([4; 32]);
        let other = KeyPair::from_seed([5; 32]);
        let signed = Signed::sign(&vec![1u32, 2, 3], &hub, Purpose::Command);
        assert_eq!(
            signed.open(&hub.public(), Purpose::Command),
            Ok(vec![1, 2, 3])
        );
        assert_eq!(
            signed.open(&other.public(), Purpose::Command),
            Err(OpenError::WrongSigner)
        );
        assert_eq!(
            signed.open(&hub.public(), Purpose::Decision),
            Err(OpenError::BadSignature)
        );
        let mut tampered = signed.clone();
        tampered.body = "[1,2,4]".into();
        assert_eq!(
            tampered.open(&hub.public(), Purpose::Command),
            Err(OpenError::BadSignature)
        );
        // Survives a trip through JSON.
        let text = serde_json::to_string(&signed).unwrap();
        let back: Signed<Vec<u32>> = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.open(&hub.public(), Purpose::Command),
            Ok(vec![1, 2, 3])
        );
        assert_eq!(back.hash(), signed.hash());
    }
}
