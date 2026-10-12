//! What DiskTree devices and the DiskTree Hub say to each other, and how
//! each side proves who said it.
//!
//! Each device and the Hub has an Ed25519 key pair ([`keys`]). A device is
//! paired once with a one-time code ([`pairing`]); from then on every request
//! it makes is signed, and everything the Hub hands on (commands, an
//! approver's decisions) is signed by whoever made it ([`signing`]). The
//! messages themselves are in [`messages`].

pub mod keys;
pub mod messages;
pub mod pairing;
pub mod signing;

pub use keys::{KeyPair, PublicKey};
pub use messages::*;
pub use pairing::PairingLink;
pub use signing::{Purpose, RequestSignature, Signed};
