//! QUIC transport for NyaRemoteControl.
//!
//! * [`identity`] – per-installation self-signed certificate and its fingerprint
//! * [`tls`] – rustls configs: server/client certificates are checked by
//!   fingerprint at the application level instead of by a CA
//! * [`pairing`] – pairing code and HMAC transcript proofs
//! * [`endpoint`] – quinn endpoints with tuned transport settings

pub mod clipfiles;
pub mod endpoint;
pub mod files;
pub mod identity;
pub mod pairing;
pub mod tls;

pub use identity::{Fingerprint, Identity};
pub use quinn;
