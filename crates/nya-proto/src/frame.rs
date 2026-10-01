//! Binary layouts on the hot path: stream/datagram type tags, the video frame
//! header and the audio datagram header.
//!
//! Every uni stream starts with a varint stream type; every datagram starts
//! with a one-byte datagram type. Receivers drop streams/datagrams whose type
//! they don't know, which lets newer versions add channels.

use crate::pb::{Chroma, Codec};

/// Uni-directional stream types (varint written first on the stream).
pub mod stream_type {
    pub const INPUT: u64 = 1;
    /// `varint stream_id`, then (FEATURE_MULTI_STREAM) `varint slot`, then frames.
    pub const VIDEO: u64 = 2;
    pub const CURSOR: u64 = 3;
    /// File / clipboard-image transfer (FileHeader + bytes), either direction.
    pub const FILE: u64 = 4;
    /// Bidi TCP tunnel opened by the host (USB/IP): varint port, then raw bytes.
    pub const TUNNEL: u64 = 5;
}

/// Datagram types (first byte).
pub mod datagram_type {
    /// Host system audio -> client.
    pub const AUDIO: u8 = 1;
    /// Client microphone -> host (same layout as AUDIO).
    pub const MIC: u8 = 2;
    /// Host -> client: one shard of a video frame (FEATURE_VIDEO_DATAGRAM);
    /// layout in `nya_transport::videodgram`.
    pub const VIDEO: u8 = 3;
}

pub mod frame_flags {
    pub const KEYFRAME: u16 = 1 << 0;
    pub const CONFIG_CHANGE: u16 = 1 << 1;
    pub const STATIC_REFINE: u16 = 1 << 2;
}

/// Header in front of each encoded video frame on the video stream.
///
/// Wire layout (little endian), version 1, 28 bytes:
/// ```text
/// u8  header_version
/// u8  header_len        total header size; readers skip unknown trailing bytes
/// u16 flags
/// u64 frame_id
/// u64 capture_ts_us
/// u16 width
/// u16 height
/// u8  codec
/// u8  chroma
/// u16 reserved
/// ```
/// On the video stream each frame is `u32 LE frame_len` (header + payload) followed
/// by the header and the Annex-B payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoFrameHeader {
    pub flags: u16,
    pub frame_id: u64,
    pub capture_ts_us: u64,
    pub width: u16,
    pub height: u16,
    pub codec: u8,
    pub chroma: u8,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("frame header truncated")]
    Truncated,
    #[error("frame header length {0} is invalid")]
    BadLength(usize),
}

impl VideoFrameHeader {
    pub const VERSION: u8 = 1;
    pub const LEN_V1: usize = 28;

    pub fn is_keyframe(&self) -> bool {
        self.flags & frame_flags::KEYFRAME != 0
    }

    pub fn codec(&self) -> Codec {
        Codec::try_from(self.codec as i32).unwrap_or(Codec::Unspecified)
    }

    pub fn chroma(&self) -> Chroma {
        Chroma::try_from(self.chroma as i32).unwrap_or(Chroma::Unspecified)
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(Self::VERSION);
        out.push(Self::LEN_V1 as u8);
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.frame_id.to_le_bytes());
        out.extend_from_slice(&self.capture_ts_us.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        out.extend_from_slice(&self.height.to_le_bytes());
        out.push(self.codec);
        out.push(self.chroma);
        out.extend_from_slice(&0u16.to_le_bytes());
    }

    /// Parse a header; returns the header and the payload that follows it.
    pub fn parse(buf: &[u8]) -> Result<(Self, &[u8]), FrameError> {
        if buf.len() < 2 {
            return Err(FrameError::Truncated);
        }
        let header_len = buf[1] as usize;
        // Any version must at least contain the v1 fields.
        if header_len < Self::LEN_V1 {
            return Err(FrameError::BadLength(header_len));
        }
        if buf.len() < header_len {
            return Err(FrameError::Truncated);
        }
        let u16_at = |o: usize| u16::from_le_bytes([buf[o], buf[o + 1]]);
        let u64_at = |o: usize| u64::from_le_bytes(buf[o..o + 8].try_into().unwrap());
        let h = Self {
            flags: u16_at(2),
            frame_id: u64_at(4),
            capture_ts_us: u64_at(12),
            width: u16_at(20),
            height: u16_at(22),
            codec: buf[24],
            chroma: buf[25],
        };
        Ok((h, &buf[header_len..]))
    }
}

/// Audio datagram: `u8 type | u32 seq | u64 capture_ts_us | opus packet`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket {
    pub seq: u32,
    pub capture_ts_us: u64,
    pub data: Vec<u8>,
}

impl AudioPacket {
    pub const HEADER_LEN: usize = 13;

    pub fn encode(&self) -> Vec<u8> {
        self.encode_as(datagram_type::AUDIO)
    }

    /// Encode with another datagram type that shares this layout (MIC).
    pub fn encode_as(&self, ty: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(Self::HEADER_LEN + self.data.len());
        v.push(ty);
        v.extend_from_slice(&self.seq.to_le_bytes());
        v.extend_from_slice(&self.capture_ts_us.to_le_bytes());
        v.extend_from_slice(&self.data);
        v
    }

    /// Parse a datagram whose type byte is AUDIO.
    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.first() != Some(&datagram_type::AUDIO) {
            return None;
        }
        Self::decode_any(buf)
    }

    /// Decode regardless of the type byte (caller checked it).
    pub fn decode_any(buf: &[u8]) -> Option<Self> {
        if buf.len() < Self::HEADER_LEN {
            return None;
        }
        Some(Self {
            seq: u32::from_le_bytes(buf[1..5].try_into().unwrap()),
            capture_ts_us: u64::from_le_bytes(buf[5..13].try_into().unwrap()),
            data: buf[13..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = VideoFrameHeader {
            flags: frame_flags::KEYFRAME,
            frame_id: 42,
            capture_ts_us: 123_456_789,
            width: 1920,
            height: 1080,
            codec: Codec::Hevc as u8,
            chroma: Chroma::Yuv444 as u8,
        };
        let mut buf = Vec::new();
        h.write(&mut buf);
        assert_eq!(buf.len(), VideoFrameHeader::LEN_V1);
        buf.extend_from_slice(b"payload");
        let (p, rest) = VideoFrameHeader::parse(&buf).unwrap();
        assert_eq!(p, h);
        assert_eq!(rest, b"payload");
        assert!(p.is_keyframe());
    }

    #[test]
    fn header_from_future_version_is_skipped_correctly() {
        let h = VideoFrameHeader { frame_id: 7, ..Default::default() };
        let mut buf = Vec::new();
        h.write(&mut buf);
        // A future version appends 4 bytes and sets unknown flag bits.
        buf[0] = 2;
        buf[1] = (VideoFrameHeader::LEN_V1 + 4) as u8;
        buf[2] |= 0x80;
        buf.extend_from_slice(&[9, 9, 9, 9]);
        buf.extend_from_slice(b"data");
        let (p, rest) = VideoFrameHeader::parse(&buf).unwrap();
        assert_eq!(p.frame_id, 7);
        assert_eq!(rest, b"data");
    }

    #[test]
    fn header_rejects_short_input() {
        assert_eq!(VideoFrameHeader::parse(&[1]), Err(FrameError::Truncated));
        assert_eq!(VideoFrameHeader::parse(&[1, 4, 0, 0]), Err(FrameError::BadLength(4)));
        assert_eq!(VideoFrameHeader::parse(&[1, 28, 0]), Err(FrameError::Truncated));
    }

    #[test]
    fn audio_roundtrip() {
        let a = AudioPacket { seq: 5, capture_ts_us: 99, data: vec![1, 2, 3] };
        assert_eq!(AudioPacket::decode(&a.encode()), Some(a));
        assert_eq!(AudioPacket::decode(&[2, 0, 0]), None);
    }
}
