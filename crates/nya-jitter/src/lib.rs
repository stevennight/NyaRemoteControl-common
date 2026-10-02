//! Adaptive jitter buffer for 48 kHz stereo audio received as datagrams
//! (design doc §5).
//!
//! * The target depth follows the measured arrival jitter: the spread of
//!   packet delays (arrival time minus the packet's place in the media
//!   timeline) over the last 10 s, 97th percentile, plus a small margin,
//!   clamped to 20–80 ms. It rises at once and falls slowly (2 ms/s), so a
//!   jittery link does not underrun again and again, and a single late burst
//!   does not keep latency high.
//! * Clock drift between the two machines (and the move to a new target) is
//!   absorbed by playing slightly faster or slower (at most ±0.5 %, cubic
//!   interpolation) instead of dropping samples. Within a small dead band the
//!   audio passes through untouched.
//! * After an underrun, playback restarts once the target depth is buffered.
//!   Audio far beyond the target (a burst after a network stall) is dropped.
//! * The sender stops sending while nothing plays; its timestamps tell such a
//!   pause apart from lost packets (only the latter are filled with silence).
//!
//! Pure Rust with the clock passed in, so it is unit tested and usable on any
//! platform (Windows client through `nya_media::jitter`, Android core directly).

use std::collections::VecDeque;

const CHANNELS: usize = 2;
const FRAMES_PER_MS: f64 = 48.0;

pub const MIN_TARGET_MS: f64 = 20.0;
pub const MAX_TARGET_MS: f64 = 80.0;
const INITIAL_TARGET_MS: f64 = 30.0;
/// Added to the measured spread.
const MARGIN_MS: f64 = 5.0;
/// How fast the target may fall.
const TARGET_FALL_MS_PER_S: f64 = 2.0;
/// Delay history the spread is measured over.
const WINDOW_US: u64 = 10_000_000;
const PERCENTILE: f64 = 0.97;
/// Drop audio down to the target when this far above it.
const OVERFLOW_MS: f64 = 40.0;
/// Start correcting the speed when the smoothed depth is this far off...
const CORRECT_START_MS: f64 = 4.0;
/// ...and stop once back within this.
const CORRECT_STOP_MS: f64 = 1.0;
/// Speed change per ms of depth error, and its limit.
const SPEED_PER_MS: f64 = 0.0003;
const MAX_SPEED_DEV: f64 = 0.005;
/// Time constant of the smoothed depth.
const LEVEL_TAU_S: f64 = 1.0;
/// Fill losses of at most this many packets with silence.
const MAX_CONCEAL_PACKETS: u32 = 5;
/// A sequence number more than this many packets behind the last one means
/// the sender started over; less far behind is a late packet.
const RESTART_BEHIND: u32 = 50;
/// A sender timestamp step this much beyond the audio it carries means the
/// sender paused (nothing was playing), not that the network held it back.
const PAUSE_US: u64 = 50_000;

/// Numbers for the statistics panel.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct JitterStats {
    /// Current target depth (ms).
    pub target_ms: f32,
    /// Smoothed buffered audio (ms).
    pub level_ms: f32,
    /// Measured arrival jitter (spread, ms).
    pub jitter_ms: f32,
    /// Playback speed (1.0 = normal).
    pub speed: f32,
    /// Totals since creation.
    pub underruns: u32,
    pub dropped_ms: u32,
    pub concealed_ms: u32,
    pub late_packets: u32,
}

pub struct JitterBuffer {
    /// Interleaved stereo.
    buf: VecDeque<f32>,
    /// The frame just before `buf`'s first (cubic interpolation needs it).
    prev: [f32; CHANNELS],
    /// Read position in frames, relative to `buf`'s first frame.
    pos: f64,
    playing: bool,

    last_seq: Option<u32>,
    last_sender_us: u64,
    /// Media timeline: where the next packet starts (µs).
    media_us: u64,
    /// Delays are relative to the first packet's.
    base_us: Option<i64>,
    /// (arrival µs, relative delay µs) of recent packets.
    delays: VecDeque<(u64, i64)>,
    since_estimate: u32,
    jitter_ms: f64,
    /// What the jitter asks for; `target_ms` falls towards it slowly.
    want_ms: f64,
    target_ms: f64,

    level_ms: f64,
    last_pull_us: Option<u64>,
    correcting: bool,
    speed: f64,
    scratch: Vec<f32>,
    stats: JitterStats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            buf: VecDeque::new(),
            prev: [0.0; CHANNELS],
            pos: 0.0,
            playing: false,
            last_seq: None,
            last_sender_us: 0,
            media_us: 0,
            base_us: None,
            delays: VecDeque::new(),
            since_estimate: 0,
            jitter_ms: 0.0,
            want_ms: INITIAL_TARGET_MS,
            target_ms: INITIAL_TARGET_MS,
            level_ms: 0.0,
            last_pull_us: None,
            correcting: false,
            speed: 1.0,
            scratch: Vec::new(),
            stats: JitterStats::default(),
        }
    }

    /// Forget all audio and the timeline (the sender restarted, the output
    /// went idle). The jitter estimate, target and totals are kept.
    pub fn reset(&mut self) {
        let mut fresh = Self::new();
        fresh.jitter_ms = self.jitter_ms;
        fresh.want_ms = self.want_ms;
        fresh.target_ms = self.target_ms;
        fresh.stats = self.stats;
        *self = fresh;
    }

    /// Buffered audio in ms (not counting what the device already holds).
    pub fn buffered_ms(&self) -> f64 {
        ((self.buf.len() / CHANNELS) as f64 - self.pos) / FRAMES_PER_MS
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn stats(&self) -> JitterStats {
        JitterStats {
            target_ms: self.target_ms as f32,
            level_ms: self.level_ms as f32,
            jitter_ms: self.jitter_ms as f32,
            speed: self.speed as f32,
            ..self.stats
        }
    }

    /// A packet arrived at `now_us` (local clock). `sender_us` is the
    /// sender's timestamp; `decode` appends the packet's interleaved samples.
    /// Late or duplicate packets are not decoded; returns false for them.
    pub fn push(&mut self, seq: u32, sender_us: u64, now_us: u64, decode: impl FnOnce(&mut Vec<f32>)) -> bool {
        let mut gap = 0;
        if let Some(last) = self.last_seq {
            let d = seq.wrapping_sub(last);
            if d == 0 || (d > u32::MAX / 2 && d.wrapping_neg() <= RESTART_BEHIND) {
                self.stats.late_packets += 1;
                return false;
            } else if d > u32::MAX / 2 {
                // Far behind: the sender started over (reconnect, restart).
                self.reset();
            } else {
                gap = d - 1;
            }
        }
        self.scratch.clear();
        decode(&mut self.scratch);
        let frames = self.scratch.len() / CHANNELS;
        let packet_us = frames as u64 * 1000 / 48;

        if self.last_seq.is_some() {
            let sender_step = sender_us.saturating_sub(self.last_sender_us);
            if sender_step > (gap as u64 + 1) * packet_us + PAUSE_US {
                // The sender paused: move the timeline along, no silence.
                self.media_us += sender_step - packet_us;
            } else if gap > 0 {
                self.media_us += gap as u64 * packet_us;
                if gap <= MAX_CONCEAL_PACKETS && self.playing {
                    self.buf.extend(std::iter::repeat(0.0).take(gap as usize * frames * CHANNELS));
                    self.stats.concealed_ms += gap * packet_us as u32 / 1000;
                }
            }
        }
        self.last_seq = Some(seq);
        self.last_sender_us = sender_us;

        let rel = now_us as i64 - self.media_us as i64;
        let base = *self.base_us.get_or_insert(rel);
        self.delays.push_back((now_us, rel - base));
        while self.delays.front().is_some_and(|&(t, _)| now_us.saturating_sub(t) > WINDOW_US) {
            self.delays.pop_front();
        }
        self.since_estimate += 1;
        if self.since_estimate >= 10 || self.delays.len() < 20 {
            self.since_estimate = 0;
            self.estimate();
        }
        self.media_us += packet_us;
        self.buf.extend(self.scratch.iter().copied());
        true
    }

    fn estimate(&mut self) {
        let mut d: Vec<i64> = self.delays.iter().map(|&(_, d)| d).collect();
        d.sort_unstable();
        let idx = ((d.len() - 1) as f64 * PERCENTILE).round() as usize;
        self.jitter_ms = (d[idx] - d[0]) as f64 / 1000.0;
        self.want_ms = (self.jitter_ms + MARGIN_MS).clamp(MIN_TARGET_MS, MAX_TARGET_MS);
        self.target_ms = self.target_ms.max(self.want_ms);
    }

    /// Append up to `max_frames` frames (interleaved) to `out`; returns the
    /// number of frames written. `device_queued` is what the output device
    /// still holds: with nothing to give and an empty device, playback stops
    /// until the target depth is buffered again.
    pub fn pull(&mut self, now_us: u64, max_frames: usize, device_queued: usize, out: &mut Vec<f32>) -> usize {
        let dt = self.last_pull_us.map_or(0.0, |t| now_us.saturating_sub(t) as f64 / 1e6);
        self.last_pull_us = Some(now_us);
        if self.target_ms > self.want_ms {
            self.target_ms = (self.target_ms - TARGET_FALL_MS_PER_S * dt).max(self.want_ms);
        }

        if !self.playing {
            if self.buffered_ms() < self.target_ms {
                return 0;
            }
            self.playing = true;
            self.level_ms = self.buffered_ms();
            self.stop_correcting();
        }
        if self.buffered_ms() > self.target_ms + OVERFLOW_MS {
            let excess = self.buffered_ms() - self.target_ms;
            self.skip((excess * FRAMES_PER_MS) as usize);
            self.stats.dropped_ms += excess as u32;
            self.level_ms = self.buffered_ms();
        }

        let a = 1.0 - (-dt / LEVEL_TAU_S).exp();
        self.level_ms += (self.buffered_ms() - self.level_ms) * a;
        let err = self.level_ms - self.target_ms;
        if !self.correcting && err.abs() > CORRECT_START_MS {
            self.correcting = true;
        } else if self.correcting && err.abs() < CORRECT_STOP_MS {
            self.stop_correcting();
        }
        if self.correcting {
            self.speed = 1.0 + (err * SPEED_PER_MS).clamp(-MAX_SPEED_DEV, MAX_SPEED_DEV);
        }

        let n = self.resample(max_frames, out);
        if n == 0 && max_frames > 0 && device_queued == 0 {
            self.playing = false;
            self.stats.underruns += 1;
        }
        n
    }

    /// Back to untouched pass-through: whole-frame position, speed 1.
    fn stop_correcting(&mut self) {
        self.correcting = false;
        self.speed = 1.0;
        if self.pos >= 0.5 && self.buf.len() >= CHANNELS {
            self.skip(1);
        }
        self.pos = 0.0;
    }

    /// Discard `frames` frames from the front.
    fn skip(&mut self, frames: usize) {
        let frames = frames.min(self.buf.len() / CHANNELS);
        if frames == 0 {
            return;
        }
        let last = (frames - 1) * CHANNELS;
        self.prev = [self.buf[last], self.buf[last + 1]];
        self.buf.drain(..frames * CHANNELS);
        self.pos = (self.pos - frames as f64).max(0.0);
    }

    fn frame(&self, i: isize) -> [f32; CHANNELS] {
        if i < 0 {
            self.prev
        } else {
            let j = i as usize * CHANNELS;
            [self.buf[j], self.buf[j + 1]]
        }
    }

    fn resample(&mut self, max_frames: usize, out: &mut Vec<f32>) -> usize {
        let avail = self.buf.len() / CHANNELS;
        let mut n = 0;
        while n < max_frames {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            if t == 0.0 {
                if i >= avail {
                    break;
                }
                out.extend_from_slice(&self.frame(i as isize));
            } else {
                if i + 2 >= avail {
                    break;
                }
                let i = i as isize;
                let (xm, x0, x1, x2) = (self.frame(i - 1), self.frame(i), self.frame(i + 1), self.frame(i + 2));
                for c in 0..CHANNELS {
                    out.push(catmull_rom(xm[c], x0[c], x1[c], x2[c], t));
                }
            }
            self.pos += self.speed;
            n += 1;
        }
        let consumed = (self.pos.floor() as usize).min(avail);
        if consumed > 0 {
            let pos = self.pos;
            self.skip(consumed);
            self.pos = pos - consumed as f64;
        }
        n
    }
}

fn catmull_rom(xm: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    x0 + 0.5 * t * (x1 - xm + t * (2.0 * xm - 5.0 * x0 + 4.0 * x1 - x2 + t * (3.0 * (x0 - x1) + x2 - xm)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET_FRAMES: usize = 480;

    struct Sim {
        jb: JitterBuffer,
        /// Device queue (frames).
        queued: usize,
        /// Played frames (left channel).
        out: Vec<f32>,
        /// Packets in flight, in order: (arrival µs, seq, sender µs).
        flight: VecDeque<(u64, u32, u64)>,
        rng: u64,
    }

    impl Sim {
        fn new() -> Self {
            Self { jb: JitterBuffer::new(), queued: 0, out: Vec::new(), flight: VecDeque::new(), rng: 0x9e3779b97f4a7c15 }
        }

        fn rand(&mut self) -> f64 {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            (self.rng >> 11) as f64 / (1u64 << 53) as f64
        }

        /// Run for `secs`. The sender emits a 10 ms packet every `period_us`
        /// of our time (10 000 = same clock); each is held back by a random
        /// delay up to `jitter_us`, in order (queueing). Sample values count
        /// frames, so untouched playback is a ramp. The device plays 48
        /// frames/ms and is topped up to 20 ms.
        fn run(&mut self, secs: u64, start_us: u64, period_us: u64, jitter_us: u64, seq0: u32) -> u32 {
            let mut seq = seq0;
            let mut next_send = start_us;
            let end = start_us + secs * 1_000_000;
            let mut t = start_us;
            let mut played = Vec::new();
            while t < end {
                while next_send <= t {
                    let delay = (self.rand() * jitter_us as f64) as u64;
                    let arrival = (next_send + delay).max(self.flight.back().map_or(0, |f| f.0));
                    seq += 1;
                    self.flight.push_back((arrival, seq, seq as u64 * 10_000));
                    next_send += period_us;
                }
                while self.flight.front().is_some_and(|f| f.0 <= t) {
                    let (_, s, sender) = self.flight.pop_front().unwrap();
                    let base = s as f32 * PACKET_FRAMES as f32;
                    self.jb.push(s, sender, t, |o| o.extend((0..PACKET_FRAMES).flat_map(|k| [base + k as f32; 2])));
                }
                self.queued = self.queued.saturating_sub(48);
                if self.queued < 20 * 48 {
                    played.clear();
                    let n = self.jb.pull(t, 20 * 48 - self.queued, self.queued, &mut played);
                    self.queued += n;
                    self.out.extend(played.chunks(2).map(|f| f[0]));
                }
                t += 1000;
            }
            seq
        }
    }

    #[test]
    fn steady_stream_settles_at_minimum_and_passes_through() {
        let mut s = Sim::new();
        s.run(30, 0, 10_000, 1_000, 0);
        let st = s.jb.stats();
        assert_eq!(st.underruns, 0, "{st:?}");
        assert_eq!(st.dropped_ms, 0, "{st:?}");
        assert!((st.target_ms - MIN_TARGET_MS as f32).abs() < 0.01, "{st:?}");
        assert_eq!(st.speed, 1.0);
        // The last 10 s are the input, untouched.
        let tail = &s.out[s.out.len() - 480_000..];
        assert!(tail.windows(2).all(|w| w[1] - w[0] == 1.0));
    }

    #[test]
    fn jitter_raises_the_target_and_avoids_underruns() {
        let mut s = Sim::new();
        s.run(5, 0, 10_000, 40_000, 0);
        let before = s.jb.stats().underruns;
        s.run(30, 5_000_000, 10_000, 40_000, 500);
        let st = s.jb.stats();
        assert!(st.jitter_ms > 30.0 && st.jitter_ms <= 41.0, "{st:?}");
        assert!(st.target_ms > 35.0 && st.target_ms <= 46.0, "{st:?}");
        assert_eq!(st.underruns, before, "{st:?}");
    }

    #[test]
    fn target_falls_slowly_when_jitter_goes_away() {
        let mut s = Sim::new();
        let seq = s.run(12, 0, 10_000, 60_000, 0);
        let high = s.jb.stats().target_ms;
        assert!(high > 55.0, "{high}");
        // 10 s window still holds the jittery packets, then 2 ms/s fall.
        s.run(15, 12_000_000, 10_000, 0, seq);
        let st = s.jb.stats();
        assert!(st.target_ms < high - 5.0 && st.target_ms > MIN_TARGET_MS as f32, "{st:?}");
    }

    #[test]
    fn clock_drift_is_absorbed_without_drops_or_underruns() {
        for period in [9_990, 10_010] {
            // Sender 0.1 % fast / slow for 2 minutes: 120 ms of drift.
            let mut s = Sim::new();
            s.run(120, 0, period, 2_000, 0);
            let st = s.jb.stats();
            assert_eq!(st.dropped_ms, 0, "{period}: {st:?}");
            assert!(st.underruns <= 1, "{period}: {st:?}");
            assert!((st.level_ms - st.target_ms).abs() < 8.0, "{period}: {st:?}");
            assert!((st.speed - 1.0).abs() <= MAX_SPEED_DEV as f32);
        }
    }

    #[test]
    fn late_and_duplicate_packets_are_rejected() {
        let mut jb = JitterBuffer::new();
        let pcm = |o: &mut Vec<f32>| o.extend(std::iter::repeat(0.5).take(960));
        assert!(jb.push(500, 0, 0, pcm));
        assert!(!jb.push(500, 0, 1000, pcm));
        assert!(!jb.push(499, 0, 1000, pcm));
        assert!(jb.push(501, 10_000, 10_000, pcm));
        assert_eq!(jb.stats().late_packets, 2);
        // Far behind: the sender restarted; start over with it.
        assert!(jb.push(1, 0, 20_000, pcm));
        assert_eq!(jb.stats().late_packets, 2);
        assert!((jb.buffered_ms() - 10.0).abs() < 0.01);
        // Wrap-around is in order.
        let mut jb = JitterBuffer::new();
        assert!(jb.push(u32::MAX, 0, 0, pcm));
        assert!(jb.push(0, 10_000, 10_000, pcm));
    }

    #[test]
    fn lost_packets_are_concealed_but_sender_pauses_are_not() {
        let pcm = |o: &mut Vec<f32>| o.extend(std::iter::repeat(0.5).take(960));
        let mut jb = JitterBuffer::new();
        for i in 0..4u32 {
            jb.push(i, i as u64 * 10_000, i as u64 * 10_000, pcm);
        }
        let mut out = Vec::new();
        jb.pull(40_000, 0, 0, &mut out);
        assert!(jb.is_playing());
        // Packets 4 and 5 lost.
        jb.push(6, 60_000, 60_000, pcm);
        assert_eq!(jb.stats().concealed_ms, 20);
        assert!((jb.buffered_ms() - 70.0).abs() < 0.01);
        // Sender paused for 2 s (nothing playing): no silence, no jitter.
        jb.push(7, 2_070_000, 2_070_000, pcm);
        assert_eq!(jb.stats().concealed_ms, 20);
        assert!(jb.stats().jitter_ms < 1.0, "{:?}", jb.stats());
    }

    #[test]
    fn a_burst_after_a_stall_is_trimmed_to_the_target() {
        let pcm = |o: &mut Vec<f32>| o.extend(std::iter::repeat(0.5).take(960));
        let mut jb = JitterBuffer::new();
        for i in 0..15u32 {
            jb.push(i, i as u64 * 10_000, 150_000, pcm);
        }
        let mut out = Vec::new();
        let n = jb.pull(150_000, 960, 0, &mut out);
        assert_eq!(n, 960);
        assert!(jb.buffered_ms() <= jb.stats().target_ms as f64 + 0.01, "{}", jb.buffered_ms());
        assert!(jb.stats().dropped_ms > 0);
    }

    #[test]
    fn resampling_keeps_a_sine_clean() {
        let mut jb = JitterBuffer::new();
        let f = 1000.0 / 48_000.0 * std::f32::consts::TAU;
        let mut k = 0usize;
        for seq in 0..100u32 {
            jb.push(seq, seq as u64 * 10_000, seq as u64 * 10_000, |o| {
                for _ in 0..480 {
                    let v = (k as f32 * f).sin();
                    o.extend([v, v]);
                    k += 1;
                }
            });
        }
        jb.speed = 1.004;
        jb.correcting = true;
        jb.playing = true;
        let mut out = Vec::new();
        jb.resample(20_000, &mut out);
        // Output is a 1004 Hz sine: compare against the ideal.
        let mut worst = 0f32;
        for (j, s) in out.chunks(2).enumerate().skip(4) {
            let ideal = (j as f64 * 1.004 * f as f64).sin() as f32;
            worst = worst.max((s[0] - ideal).abs());
        }
        assert!(worst < 2e-3, "max error {worst}");
    }
}
