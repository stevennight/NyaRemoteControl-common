//! QUIC transport for NyaRemoteControl.
//!
//! * [`identity`] – per-installation self-signed certificate and its fingerprint
//! * [`tls`] – rustls configs: server/client certificates are checked by
//!   fingerprint at the application level instead of by a CA
//! * [`pairing`] – pairing code and HMAC transcript proofs
//! * [`endpoint`] – quinn endpoints with tuned transport settings
//! * [`videodgram`] – video frames as datagrams with Reed-Solomon FEC
//! * [`folders`] – client folders served to the host's mounted drive

pub mod clipfiles;
pub mod endpoint;
pub mod filechan;
pub mod files;
pub mod folders;
pub mod identity;
pub mod pairing;
pub mod tls;
pub mod videodgram;

pub use identity::{Fingerprint, Identity};
pub use quinn;
