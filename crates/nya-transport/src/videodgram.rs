//! Video frames as QUIC datagrams with Reed-Solomon forward error correction
//! (FEATURE_VIDEO_DATAGRAM, design doc §6.2).
//!
//! On a reliable stream one lost packet holds up every frame behind it until
//! it is retransmitted (an RTT or more). In game mode the host can instead
//! cut each frame into datagram-sized *shards* and add parity shards; the
//! client rebuilds the frame from any `original_count` of them. A frame that
//! can't be rebuilt is dropped, the decoder notices the gap in frame ids and
//! asks for a keyframe, exactly as after any other loss.
//!
//! Datagram layout (little endian, 32-byte header):
//! ```text
//! u8  type            datagram_type::VIDEO
//! u8  slot            client window
//! u16 shard_bytes     size of every shard but the last original one (even)
//! u64 stream_id
//! u64 frame_id
//! u32 frame_len       bytes of the frame (video frame header + payload)
//! u16 original_count  ceil(frame_len / shard_bytes)
//! u16 recovery_count  parity shards
//! u16 index           0..original_count = original, then recovery
//! u16 flags           0 (receivers ignore unknown bits)
//! payload             original shard i: frame[i*S .. min((i+1)*S, len)]; recovery: S bytes
//! ```
//! Shards are sent originals first, so without loss a frame is complete as
//! soon as its last original shard arrives and parity is never decoded.

use std::collections::{BTreeMap, HashMap};

use nya_proto::frame::datagram_type;

pub const HEADER_LEN: usize = 32;
/// Parity shards per frame at the least (losses come in bursts).
pub const MIN_RECOVERY: usize = 2;
/// Parity as a percentage of the original shards: default and limits.
pub const DEFAULT_FEC_PERCENT: u32 = 20;
pub const MIN_FEC_PERCENT: u32 = 10;
pub const MAX_FEC_PERCENT: u32 = 50;
/// Partly received frames kept per slot; older ones are given up.
const MAX_PARTIAL: usize = 16;
/// Frames this far behind the newest are final for the loss statistics.
const TALLY_DEPTH: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardHeader {
    pub slot: u8,
    pub shard_bytes: u16,
    pub stream_id: u64,
    pub frame_id: u64,
    pub frame_len: u32,
    pub original_count: u16,
    pub recovery_count: u16,
    pub index: u16,
}

impl ShardHeader {
    fn write(&self, out: &mut Vec<u8>) {
        out.push(datagram_type::VIDEO);
        out.push(self.slot);
        out.extend_from_slice(&self.shard_bytes.to_le_bytes());
        out.extend_from_slice(&self.stream_id.to_le_bytes());
        out.extend_from_slice(&self.frame_id.to_le_bytes());
        out.extend_from_slice(&self.frame_len.to_le_bytes());
        out.extend_from_slice(&self.original_count.to_le_bytes());
        out.extend_from_slice(&self.recovery_count.to_le_bytes());
        out.extend_from_slice(&self.index.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
    }

    /// Parse and check a VIDEO datagram; `None` if it is not one or is malformed.
    pub fn parse(d: &[u8], max_frame_len: usize) -> Option<(Self, &[u8])> {
        if d.len() < HEADER_LEN || d[0] != datagram_type::VIDEO {
            return None;
        }
        let u16_at = |i: usize| u16::from_le_bytes([d[i], d[i + 1]]);
        let u64_at = |i: usize| u64::from_le_bytes(d[i..i + 8].try_into().unwrap());
        let h = Self {
            slot: d[1],
            shard_bytes: u16_at(2),
            stream_id: u64_at(4),
            frame_id: u64_at(12),
            frame_len: u32::from_le_bytes(d[20..24].try_into().unwrap()),
            original_count: u16_at(24),
            recovery_count: u16_at(26),
            index: u16_at(28),
        };
        let s = h.shard_bytes as usize;
        let len = h.frame_len as usize;
        if s < 2 || s % 2 != 0 || len == 0 || len > max_frame_len || h.original_count as usize != len.div_ceil(s) {
            return None;
        }
        if h.index as usize >= h.original_count as usize + h.recovery_count as usize {
            return None;
        }
        let payload = &d[HEADER_LEN..];
        if payload.len() != h.shard_len(h.index as usize) {
            return None;
        }
        Some((h, payload))
    }

    /// Bytes carried by shard `i`.
    fn shard_len(&self, i: usize) -> usize {
        let s = self.shard_bytes as usize;
        if i < self.original_count as usize {
            s.min(self.frame_len as usize - i * s)
        } else {
            s
        }
    }
}

/// Parity shards for a frame of `original` shards.
pub fn recovery_count(original: usize, fec_percent: u32) -> usize {
    if fec_percent == 0 {
        return 0;
    }
    let mut r = (original * fec_percent as usize).div_ceil(100).max(MIN_RECOVERY);
    while r > 0 && !reed_solomon_simd::ReedSolomonEncoder::supports(original, r) {
        r /= 2;
    }
    r
}

/// Cut one frame (video frame header + payload) into datagrams no larger
/// than `max_datagram`, originals first, then parity.
pub fn split(
    slot: u8,
    stream_id: u64,
    frame_id: u64,
    frame: &[u8],
    max_datagram: usize,
    fec_percent: u32,
) -> anyhow::Result<Vec<Vec<u8>>> {
    anyhow::ensure!(!frame.is_empty(), "empty frame");
    anyhow::ensure!(max_datagram >= HEADER_LEN + 64, "datagrams too small ({max_datagram} bytes)");
    let s = (max_datagram - HEADER_LEN).min(u16::MAX as usize) & !1;
    let original = frame.len().div_ceil(s);
    anyhow::ensure!(original <= u16::MAX as usize && frame.len() <= u32::MAX as usize, "frame too large");
    let recovery = recovery_count(original, fec_percent).min(u16::MAX as usize - original);
    let mut h = ShardHeader {
        slot,
        shard_bytes: s as u16,
        stream_id,
        frame_id,
        frame_len: frame.len() as u32,
        original_count: original as u16,
        recovery_count: recovery as u16,
        index: 0,
    };
    let mut out = Vec::with_capacity(original + recovery);
    for (i, chunk) in frame.chunks(s).enumerate() {
        h.index = i as u16;
        let mut d = Vec::with_capacity(HEADER_LEN + chunk.len());
        h.write(&mut d);
        d.extend_from_slice(chunk);
        out.push(d);
    }
    if recovery > 0 {
        // Every original shard must be S bytes for the code: pad the last.
        let mut padded = frame[(original - 1) * s..].to_vec();
        padded.resize(s, 0);
        let shards = frame.chunks(s).take(original - 1).chain(std::iter::once(&padded[..]));
        let parity = reed_solomon_simd::encode(original, recovery, shards)?;
        for (j, p) in parity.into_iter().enumerate() {
            h.index = (original + j) as u16;
            let mut d = Vec::with_capacity(HEADER_LEN + s);
            h.write(&mut d);
            d.extend_from_slice(&p);
            out.push(d);
        }
    }
    Ok(out)
}

/// A rebuilt frame.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub slot: u8,
    pub stream_id: u64,
    pub frame_id: u64,
    /// Video frame header + payload, as on the video stream.
    pub data: Vec<u8>,
}

/// Counters since the last `take_stats`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DgramStats {
    pub shards_received: u32,
    /// Shards of finished frames that never arrived.
    pub shards_lost: u32,
    pub frames_completed: u32,
    /// Completed only thanks to parity.
    pub frames_recovered: u32,
    /// Given up: not enough shards before a newer frame completed.
    pub frames_lost: u32,
    pub malformed: u32,
}

struct Partial {
    head: ShardHeader,
    original: Vec<Option<Vec<u8>>>,
    recovery: Vec<Option<Vec<u8>>>,
    have: usize,
}

#[derive(Default)]
struct Slot {
    stream_id: u64,
    /// Newest frame delivered or given up; older shards are ignored.
    done: Option<u64>,
    partial: BTreeMap<u64, Partial>,
    /// frame id -> (shards sent, shards received), for the loss statistics.
    tally: BTreeMap<u64, (u32, u32)>,
}

/// Rebuilds frames from shards, per slot. Complete frames come out in
/// order; a frame still incomplete when a newer one completes is dropped.
pub struct Reassembler {
    slots: HashMap<u8, Slot>,
    max_frame_len: usize,
    stats: DgramStats,
}

impl Reassembler {
    pub fn new(max_frame_len: usize) -> Self {
        Self { slots: HashMap::new(), max_frame_len, stats: DgramStats::default() }
    }

    pub fn take_stats(&mut self) -> DgramStats {
        std::mem::take(&mut self.stats)
    }

    /// Forget a slot (its window closed, or reconnect).
    pub fn reset(&mut self) {
        self.slots.clear();
    }

    /// One VIDEO datagram in; a frame out when this shard completed one.
    pub fn push(&mut self, d: &[u8]) -> Option<Frame> {
        let Some((h, payload)) = ShardHeader::parse(d, self.max_frame_len) else {
            self.stats.malformed += 1;
            return None;
        };
        self.stats.shards_received += 1;
        let slot = self.slots.entry(h.slot).or_default();
        if h.stream_id != slot.stream_id {
            if h.stream_id < slot.stream_id {
                return None; // an old stream's straggler
            }
            *slot = Slot { stream_id: h.stream_id, ..Default::default() };
        }

        let total = h.original_count as u32 + h.recovery_count as u32;
        slot.tally.entry(h.frame_id).or_insert((total, 0)).1 += 1;
        let newest = *slot.tally.keys().next_back().unwrap();
        while let Some((&id, &(sent, got))) = slot.tally.first_key_value() {
            if id + TALLY_DEPTH > newest {
                break;
            }
            self.stats.shards_lost += sent.saturating_sub(got);
            slot.tally.pop_first();
        }

        if slot.done.is_some_and(|d| h.frame_id <= d) {
            return None;
        }
        let p = slot.partial.entry(h.frame_id).or_insert_with(|| Partial {
            head: h,
            original: vec![None; h.original_count as usize],
            recovery: vec![None; h.recovery_count as usize],
            have: 0,
        });
        if (p.head.frame_len, p.head.original_count, p.head.recovery_count, p.head.shard_bytes)
            != (h.frame_len, h.original_count, h.recovery_count, h.shard_bytes)
        {
            self.stats.malformed += 1;
            return None;
        }
        let i = h.index as usize;
        let cell = if i < p.original.len() { &mut p.original[i] } else { &mut p.recovery[i - p.original.len()] };
        if cell.is_some() {
            return None; // duplicate
        }
        *cell = Some(payload.to_vec());
        p.have += 1;

        if p.have < p.original.len() {
            while slot.partial.len() > MAX_PARTIAL {
                slot.partial.pop_first();
                self.stats.frames_lost += 1;
            }
            return None;
        }
        let p = slot.partial.remove(&h.frame_id).unwrap();
        // Older frames can't be shown any more (the decoder needs them in order).
        while slot.partial.first_key_value().is_some_and(|(&id, _)| id < h.frame_id) {
            slot.partial.pop_first();
            self.stats.frames_lost += 1;
        }
        slot.done = Some(h.frame_id);
        match rebuild(p) {
            Ok((data, recovered)) => {
                self.stats.frames_completed += 1;
                if recovered {
                    self.stats.frames_recovered += 1;
                }
                Some(Frame { slot: h.slot, stream_id: h.stream_id, frame_id: h.frame_id, data })
            }
            Err(e) => {
                tracing::debug!("video frame {}: FEC decode failed: {e}", h.frame_id);
                self.stats.frames_lost += 1;
                None
            }
        }
    }
}

/// Join the original shards, decoding missing ones from parity first.
fn rebuild(p: Partial) -> anyhow::Result<(Vec<u8>, bool)> {
    let h = p.head;
    let s = h.shard_bytes as usize;
    let n = p.original.len();
    let mut original = p.original;
    let missing = original.iter().filter(|o| o.is_none()).count();
    if missing > 0 {
        let pad = |mut v: Vec<u8>| {
            v.resize(s, 0);
            v
        };
        let have: Vec<(usize, Vec<u8>)> =
            original.iter().enumerate().filter_map(|(i, o)| o.clone().map(|v| (i, pad(v)))).collect();
        let rec = p.recovery.iter().enumerate().filter_map(|(i, r)| r.as_ref().map(|v| (i, v.as_slice())));
        let restored = reed_solomon_simd::decode(n, p.recovery.len(), have, rec)?;
        for (i, v) in restored {
            original[i] = Some(v);
        }
    }
    let mut data = Vec::with_capacity(h.frame_len as usize);
    for o in original {
        data.extend_from_slice(&o.ok_or_else(|| anyhow::anyhow!("shard missing after decode"))?);
    }
    data.truncate(h.frame_len as usize);
    Ok((data, missing > 0))
}

/// Next parity percentage from the client's report of one interval: rises
/// at once with loss (three times the loss rate plus a margin), falls back
/// by one point per report when the link is clean.
pub fn next_fec_percent(current: u32, shards_received: u32, shards_lost: u32, frames_lost: u32) -> u32 {
    let total = shards_received + shards_lost;
    let loss_pct = if total == 0 { 0.0 } else { shards_lost as f64 * 100.0 / total as f64 };
    let mut want = (MIN_FEC_PERCENT as f64 + 3.0 * loss_pct).ceil() as u32;
    if frames_lost > 0 {
        want = want.max(current + 10);
    }
    let next = if want > current { want } else { current.saturating_sub(1).max(want) };
    next.clamp(MIN_FEC_PERCENT, MAX_FEC_PERCENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    #[test]
    fn header_round_trip_and_validation() {
        let f = frame(5000, 1);
        let d = split(2, 77, 9, &f, 1200, 20).unwrap();
        let (h, p) = ShardHeader::parse(&d[0], 1 << 20).unwrap();
        assert_eq!((h.slot, h.stream_id, h.frame_id, h.frame_len), (2, 77, 9, 5000));
        assert_eq!(h.shard_bytes, 1168);
        assert_eq!(h.original_count, 5);
        assert_eq!(h.recovery_count, 2);
        assert_eq!(p, &f[..1168]);
        assert!(d.iter().all(|x| x.len() <= 1200));
        // Truncated payload, wrong type, oversized frame.
        assert!(ShardHeader::parse(&d[0][..100], 1 << 20).is_none());
        let mut bad = d[0].clone();
        bad[0] = datagram_type::AUDIO;
        assert!(ShardHeader::parse(&bad, 1 << 20).is_none());
        assert!(ShardHeader::parse(&d[0], 4000).is_none());
    }

    #[test]
    fn complete_without_loss_needs_no_parity() {
        let f = frame(10_000, 3);
        let d = split(0, 1, 1, &f, 1200, 20).unwrap();
        let mut r = Reassembler::new(1 << 20);
        let n = d.len() - 2; // drop the parity
        let mut out = None;
        for x in &d[..n] {
            if let Some(fr) = r.push(x) {
                out = Some(fr);
            }
        }
        assert_eq!(out.unwrap().data, f);
        let st = r.take_stats();
        assert_eq!((st.frames_completed, st.frames_recovered, st.frames_lost), (1, 0, 0));
    }

    #[test]
    fn lost_originals_are_rebuilt_from_parity() {
        for len in [1, 700, 1168, 1169, 50_000, 300_000] {
            let f = frame(len, len as u8);
            let d = split(0, 1, 1, &f, 1200, 20).unwrap();
            let h = ShardHeader::parse(&d[0], 1 << 20).unwrap().0;
            let rec = h.recovery_count as usize;
            // Lose as many originals as there is parity (including the short last one).
            let lost: Vec<usize> = (0..h.original_count as usize).rev().step_by(2).take(rec).collect();
            let mut r = Reassembler::new(1 << 20);
            let mut out = None;
            for (i, x) in d.iter().enumerate() {
                if !lost.contains(&i) {
                    if let Some(fr) = r.push(x) {
                        out = Some(fr);
                    }
                }
            }
            assert_eq!(out.expect("rebuilt").data, f, "len {len}");
            assert_eq!(r.take_stats().frames_recovered, 1);
        }
    }

    #[test]
    fn too_much_loss_drops_the_frame_and_newer_frames_still_arrive() {
        let mut r = Reassembler::new(1 << 20);
        let f1 = frame(20_000, 1);
        let d1 = split(0, 1, 1, &f1, 1200, 10).unwrap();
        // Frame 1 loses more originals than it has parity.
        for x in d1.iter().skip(4) {
            assert!(r.push(x).is_none());
        }
        let f2 = frame(3000, 2);
        let mut got = Vec::new();
        for x in split(0, 1, 2, &f2, 1200, 10).unwrap() {
            got.extend(r.push(&x));
        }
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].frame_id, 2);
        // Late shards of frame 1 change nothing.
        for x in d1.iter().take(4) {
            assert!(r.push(x).is_none());
        }
        let st = r.take_stats();
        assert_eq!(st.frames_lost, 1);
        assert_eq!(st.frames_completed, 1);
    }

    #[test]
    fn reordering_duplicates_and_slots() {
        let mut r = Reassembler::new(1 << 20);
        let a = frame(4000, 5);
        let b = frame(4000, 6);
        let mut da = split(0, 1, 1, &a, 1200, 20).unwrap();
        let db = split(1, 9, 1, &b, 1200, 20).unwrap();
        da.reverse(); // parity first, then originals backwards
        let mut out = Vec::new();
        for (x, y) in da.iter().zip(db.iter()) {
            out.extend(r.push(x));
            out.extend(r.push(x)); // duplicate
            out.extend(r.push(y));
        }
        assert_eq!(out.len(), 2);
        assert!(out.iter().any(|f| f.slot == 0 && f.data == a));
        assert!(out.iter().any(|f| f.slot == 1 && f.data == b));
    }

    #[test]
    fn a_new_stream_replaces_the_old_one() {
        let mut r = Reassembler::new(1 << 20);
        let f = frame(3000, 1);
        let old = split(0, 5, 100, &f, 1200, 20).unwrap();
        r.push(&old[0]);
        // The host restarted the stream: frame ids start over.
        let new = split(0, 6, 1, &f, 1200, 20).unwrap();
        let got: Vec<_> = new.iter().filter_map(|x| r.push(x)).collect();
        assert_eq!(got.len(), 1);
        // Stragglers of the old stream are ignored.
        assert!(old.iter().all(|x| r.push(x).is_none()));
    }

    #[test]
    fn loss_statistics_count_shards_that_never_came() {
        let mut r = Reassembler::new(1 << 20);
        for id in 1..=30u64 {
            let d = split(0, 1, id, &frame(5000, id as u8), 1200, 20).unwrap();
            assert_eq!(d.len(), 7);
            for (i, x) in d.iter().enumerate() {
                // Every frame loses one parity shard.
                if i != 6 {
                    r.push(x);
                }
            }
        }
        let st = r.take_stats();
        assert_eq!(st.frames_completed, 30);
        assert_eq!(st.shards_received, 180);
        // Final for all but the last 8 frames.
        assert_eq!(st.shards_lost, 22);
    }

    #[test]
    fn fec_follows_loss() {
        assert_eq!(next_fec_percent(20, 1000, 0, 0), 19);
        assert_eq!(next_fec_percent(10, 1000, 0, 0), 10);
        assert_eq!(next_fec_percent(10, 950, 50, 0), 25);
        assert_eq!(next_fec_percent(20, 1000, 0, 2), 30);
        assert_eq!(next_fec_percent(45, 500, 500, 3), MAX_FEC_PERCENT);
        assert_eq!(recovery_count(1, 20), MIN_RECOVERY);
        assert_eq!(recovery_count(100, 20), 20);
        assert_eq!(recovery_count(100, 0), 0);
    }
}
