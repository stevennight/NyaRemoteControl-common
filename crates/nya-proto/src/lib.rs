//! NyaRemoteControl wire protocol.
//!
//! * [`pb`] – generated Protobuf messages (`proto/nya.proto`)
//! * [`negotiate`] – version / feature negotiation
//! * [`frame`] – binary video frame header, stream and datagram type tags
//! * [`framing`] – length-delimited message I/O over async streams

pub mod frame;
pub mod framing;
pub mod negotiate;

/// Generated Protobuf types (package `nya.v1`).
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/nya.v1.rs"));
}

/// Current protocol version spoken by this build.
pub const PROTO_MAJOR: u32 = 1;
pub const PROTO_MINOR: u32 = 1;
/// Oldest MAJOR this build can still speak.
pub const MIN_PROTO_MAJOR: u32 = 1;

/// ALPN identifier used on the QUIC connection.
pub const ALPN: &[u8] = b"nya/1";

/// Default UDP port.
pub const DEFAULT_PORT: u16 = 47100;

/// Upper bound for a single control/input/cursor message.
pub const MAX_MESSAGE_LEN: usize = 1 << 20;
/// Upper bound for a single encoded video frame.
pub const MAX_VIDEO_FRAME_LEN: usize = 32 << 20;

/// Microseconds since the Unix epoch (used for latency measurement).
pub fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}
