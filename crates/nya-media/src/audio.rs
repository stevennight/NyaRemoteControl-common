//! Opus (libopus via FFmpeg), 48 kHz stereo, 10 ms frames, low-delay mode.

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;

use anyhow::{bail, Result};
use nya_ffmpeg_sys as ff;

use crate::ff::{check, Dict, Frame, Packet};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

fn find(encoder: bool) -> Result<*const ff::AVCodec> {
    let name = CString::new("libopus").unwrap();
    let c = unsafe {
        if encoder {
            ff::avcodec_find_encoder_by_name(name.as_ptr())
        } else {
            ff::avcodec_find_decoder_by_name(name.as_ptr())
        }
    };
    if c.is_null() {
        bail!("libopus not available in FFmpeg");
    }
    Ok(c)
}

pub struct OpusEncoder {
    ctx: *mut ff::AVCodecContext,
    frame: Frame,
    pkt: Packet,
    /// Samples per channel per packet (480 = 10 ms).
    pub frame_size: usize,
    pts: i64,
}

unsafe impl Send for OpusEncoder {}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        unsafe { ff::avcodec_free_context(&mut self.ctx) };
    }
}

impl OpusEncoder {
    pub fn new(bitrate: u32) -> Result<Self> {
        let codec = find(true)?;
        unsafe {
            let ctx = ff::avcodec_alloc_context3(codec);
            (*ctx).sample_rate = SAMPLE_RATE as c_int;
            (*ctx).sample_fmt = ff::AV_SAMPLE_FMT_FLT;
            ff::av_channel_layout_default(&mut (*ctx).ch_layout, CHANNELS as c_int);
            (*ctx).bit_rate = bitrate as i64;
            (*ctx).time_base = ff::AVRational { num: 1, den: SAMPLE_RATE as c_int };
            let mut opts = Dict::new();
            opts.set("application", "lowdelay");
            opts.set("frame_duration", "10");
            let mut enc = Self { ctx, frame: Frame::new(), pkt: Packet::new(), frame_size: 0, pts: 0 };
            check(ff::avcodec_open2(ctx, codec, &mut opts.0), "avcodec_open2(libopus)")?;
            enc.frame_size = (*ctx).frame_size.max(480) as usize;
            let f = enc.frame.0;
            (*f).format = ff::AV_SAMPLE_FMT_FLT;
            (*f).nb_samples = enc.frame_size as c_int;
            (*f).sample_rate = SAMPLE_RATE as c_int;
            check(ff::av_channel_layout_copy(&mut (*f).ch_layout, &(*ctx).ch_layout), "ch_layout")?;
            check(ff::av_frame_get_buffer(f, 0), "av_frame_get_buffer")?;
            Ok(enc)
        }
    }

    /// Encode exactly `frame_size * 2` interleaved samples.
    pub fn encode(&mut self, pcm: &[f32], out: &mut Vec<Vec<u8>>) -> Result<()> {
        if pcm.len() != self.frame_size * CHANNELS {
            bail!("expected {} samples, got {}", self.frame_size * CHANNELS, pcm.len());
        }
        unsafe {
            let f = self.frame.0;
            check(ff::av_frame_make_writable(f), "av_frame_make_writable")?;
            ptr::copy_nonoverlapping(pcm.as_ptr(), (*f).data[0] as *mut f32, pcm.len());
            (*f).pts = self.pts;
            self.pts += self.frame_size as i64;
            check(ff::avcodec_send_frame(self.ctx, f), "opus send_frame")?;
            loop {
                let r = ff::avcodec_receive_packet(self.ctx, self.pkt.0);
                if r == ff::AVERROR_EAGAIN || r == ff::AVERROR_EOF {
                    return Ok(());
                }
                check(r, "opus receive_packet")?;
                let p = self.pkt.0;
                out.push(std::slice::from_raw_parts((*p).data, (*p).size as usize).to_vec());
                ff::av_packet_unref(p);
            }
        }
    }
}

pub struct OpusDecoder {
    ctx: *mut ff::AVCodecContext,
    frame: Frame,
    pkt: Packet,
}

unsafe impl Send for OpusDecoder {}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        unsafe { ff::avcodec_free_context(&mut self.ctx) };
    }
}

impl OpusDecoder {
    pub fn new() -> Result<Self> {
        let codec = find(false)?;
        unsafe {
            let ctx = ff::avcodec_alloc_context3(codec);
            (*ctx).sample_rate = SAMPLE_RATE as c_int;
            (*ctx).request_sample_fmt = ff::AV_SAMPLE_FMT_FLT;
            ff::av_channel_layout_default(&mut (*ctx).ch_layout, CHANNELS as c_int);
            let d = Self { ctx, frame: Frame::new(), pkt: Packet::new() };
            check(ff::avcodec_open2(ctx, codec, ptr::null_mut()), "avcodec_open2(libopus dec)")?;
            Ok(d)
        }
    }

    /// Decode one packet, appending interleaved stereo f32 samples.
    pub fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<()> {
        unsafe {
            let p = self.pkt.0;
            check(ff::av_new_packet(p, data.len() as c_int), "av_new_packet")?;
            ptr::copy_nonoverlapping(data.as_ptr(), (*p).data, data.len());
            let r = ff::avcodec_send_packet(self.ctx, p);
            ff::av_packet_unref(p);
            check(r, "opus send_packet")?;
            loop {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame.0);
                if r == ff::AVERROR_EAGAIN || r == ff::AVERROR_EOF {
                    return Ok(());
                }
                check(r, "opus receive_frame")?;
                let f = self.frame.0;
                let n = (*f).nb_samples as usize;
                let ch = ((*f).ch_layout.nb_channels as usize).max(1);
                match (*f).format {
                    x if x == ff::AV_SAMPLE_FMT_FLT => {
                        let s = std::slice::from_raw_parts((*f).data[0] as *const f32, n * ch);
                        push_stereo(out, n, ch, |i, c| s[i * ch + c]);
                    }
                    x if x == ff::AV_SAMPLE_FMT_FLTP => {
                        let planes: Vec<&[f32]> = (0..ch)
                            .map(|c| std::slice::from_raw_parts((*f).extended_data.add(c).read() as *const f32, n))
                            .collect();
                        push_stereo(out, n, ch, |i, c| planes[c][i]);
                    }
                    x if x == ff::AV_SAMPLE_FMT_S16 => {
                        let s = std::slice::from_raw_parts((*f).data[0] as *const i16, n * ch);
                        push_stereo(out, n, ch, |i, c| s[i * ch + c] as f32 / 32768.0);
                    }
                    other => bail!("unexpected opus sample format {other}"),
                }
                ff::av_frame_unref(f);
            }
        }
    }
}

fn push_stereo(out: &mut Vec<f32>, n: usize, ch: usize, get: impl Fn(usize, usize) -> f32) {
    for i in 0..n {
        let l = get(i, 0);
        let r = if ch > 1 { get(i, 1) } else { l };
        out.push(l);
        out.push(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure CPU: encode a sine wave and decode it back.
    #[test]
    fn opus_roundtrip() {
        let mut enc = OpusEncoder::new(128_000).unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        assert_eq!(enc.frame_size, 480);
        let mut packets = Vec::new();
        let mut t = 0f32;
        for _ in 0..20 {
            let mut pcm = Vec::with_capacity(enc.frame_size * 2);
            for _ in 0..enc.frame_size {
                let v = (t * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
                pcm.push(v);
                pcm.push(v);
                t += 1.0;
            }
            enc.encode(&pcm, &mut packets).unwrap();
        }
        assert!(packets.len() >= 15);
        let mut out = Vec::new();
        for p in &packets {
            dec.decode(p, &mut out).unwrap();
        }
        assert!(out.len() >= 15 * 480 * 2);
        let energy: f32 = out[out.len() / 2..].iter().map(|v| v * v).sum::<f32>() / (out.len() / 2) as f32;
        assert!(energy > 0.01, "decoded signal too quiet: {energy}");
    }
}
