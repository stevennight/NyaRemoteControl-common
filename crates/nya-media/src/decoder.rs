//! Video decoding: D3D11VA hardware decoding when a device is given, with
//! automatic software fallback when the hardware can't handle the stream
//! (e.g. HEVC 4:4:4 on GPUs without RExt decode support).

use std::os::raw::c_int;
use std::ptr;

use anyhow::{bail, Result};
use nya_ffmpeg_sys as ff;

use crate::ff::{check, d3d11_device_ctx, BufRef, Frame, Packet};
use crate::VideoCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayout {
    /// D3D11 texture, DXGI NV12
    Nv12,
    /// D3D11 texture, DXGI AYUV (VUYX)
    Ayuv,
    /// D3D11 texture, DXGI P010 (10-bit 4:2:0, values in the top bits)
    P010,
    /// CPU planar 10-bit 4:2:0 (16-bit little-endian samples, values in the low bits)
    Yuv420p10,
    /// CPU planar 8-bit 4:2:0
    Yuv420p,
    /// CPU planar 8-bit 4:4:4
    Yuv444p,
    /// CPU semi-planar NV12
    Nv12Cpu,
    Other(i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matrix {
    Bt709,
    Bt601,
    Bt2020,
}

pub enum FrameData<'a> {
    D3d11 { texture: *mut std::ffi::c_void, index: u32 },
    Cpu { planes: [&'a [u8]; 3], strides: [usize; 3] },
}

pub struct DecodedFrame<'a> {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    pub full_range: bool,
    pub matrix: Matrix,
    /// HDR10: samples are PQ-encoded (SMPTE ST 2084).
    pub pq: bool,
    pub data: FrameData<'a>,
}

pub struct VideoDecoder {
    ctx: *mut ff::AVCodecContext,
    _device: BufRef,
    pkt: Packet,
    frame: Frame,
    hardware: bool,
}

unsafe impl Send for VideoDecoder {}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        unsafe { ff::avcodec_free_context(&mut self.ctx) };
    }
}

fn is_hw(fmt: ff::AVPixelFormat) -> bool {
    let d = unsafe { ff::av_pix_fmt_desc_get(fmt) };
    !d.is_null() && unsafe { (*d).flags } & ff::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0
}

unsafe extern "C" fn get_format(ctx: *mut ff::AVCodecContext, fmts: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    let want_hw = !(*ctx).hw_device_ctx.is_null();
    let mut p = fmts;
    while *p != ff::AV_PIX_FMT_NONE {
        if want_hw && *p == ff::AV_PIX_FMT_D3D11 {
            return *p;
        }
        p = p.add(1);
    }
    let mut p = fmts;
    while *p != ff::AV_PIX_FMT_NONE {
        if !is_hw(*p) {
            return *p;
        }
        p = p.add(1);
    }
    *fmts
}

impl VideoDecoder {
    /// `d3d_device`: AddRef'd `ID3D11Device*` for hardware decoding (ownership
    /// passes to the decoder), or null for software decoding.
    pub fn new(codec: VideoCodec, d3d_device: *mut std::ffi::c_void) -> Result<Self> {
        let id = match codec {
            VideoCodec::H264 => ff::AV_CODEC_ID_H264,
            VideoCodec::Hevc => ff::AV_CODEC_ID_HEVC,
            VideoCodec::Av1 => ff::AV_CODEC_ID_AV1,
        };
        let dec = unsafe { ff::avcodec_find_decoder(id) };
        if dec.is_null() {
            bail!("no decoder for {}", codec.name());
        }
        let device = if d3d_device.is_null() { BufRef::null() } else { d3d11_device_ctx(d3d_device)? };
        let ctx = unsafe { ff::avcodec_alloc_context3(dec) };
        if ctx.is_null() {
            bail!("avcodec_alloc_context3 failed");
        }
        let hardware = !device.0.is_null();
        unsafe {
            (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*ctx).get_format = Some(get_format);
            if hardware {
                (*ctx).hw_device_ctx = device.new_ref();
                (*ctx).thread_count = 1;
            } else {
                // Slice threads only: frame threads add a frame of latency per thread.
                (*ctx).thread_type = ff::FF_THREAD_SLICE as c_int;
                (*ctx).thread_count = 0;
            }
        }
        let d = Self { ctx, _device: device, pkt: Packet::new(), frame: Frame::new(), hardware };
        check(unsafe { ff::avcodec_open2(ctx, dec, ptr::null_mut()) }, "avcodec_open2(decoder)")?;
        Ok(d)
    }

    pub fn is_hardware(&self) -> bool {
        self.hardware
    }

    /// Decode one access unit; `on_frame` is called for each output picture.
    pub fn decode(&mut self, data: &[u8], mut on_frame: impl FnMut(&DecodedFrame)) -> Result<()> {
        unsafe {
            let p = self.pkt.0;
            check(ff::av_new_packet(p, data.len() as c_int), "av_new_packet")?;
            ptr::copy_nonoverlapping(data.as_ptr(), (*p).data, data.len());
            let r = ff::avcodec_send_packet(self.ctx, p);
            ff::av_packet_unref(p);
            check(r, "avcodec_send_packet")?;
            loop {
                let r = ff::avcodec_receive_frame(self.ctx, self.frame.0);
                if r == ff::AVERROR_EAGAIN || r == ff::AVERROR_EOF {
                    return Ok(());
                }
                check(r, "avcodec_receive_frame")?;
                let f = self.frame.0;
                let full_range = (*f).color_range == ff::AVCOL_RANGE_JPEG;
                let matrix = match (*f).colorspace {
                    x if x == ff::AVCOL_SPC_BT470BG || x == ff::AVCOL_SPC_SMPTE170M => Matrix::Bt601,
                    x if x == ff::AVCOL_SPC_BT2020_NCL || x == ff::AVCOL_SPC_BT2020_CL => Matrix::Bt2020,
                    _ => Matrix::Bt709,
                };
                let pq = (*f).color_trc == ff::AVCOL_TRC_SMPTE2084;
                let (layout, data) = if (*f).format == ff::AV_PIX_FMT_D3D11 {
                    let fc = (*(*f).hw_frames_ctx).data as *const ff::AVHWFramesContext;
                    let layout = match (*fc).sw_format {
                        x if x == ff::AV_PIX_FMT_NV12 => PixelLayout::Nv12,
                        x if x == ff::AV_PIX_FMT_VUYX => PixelLayout::Ayuv,
                        x if x == ff::AV_PIX_FMT_P010LE => PixelLayout::P010,
                        x => PixelLayout::Other(x),
                    };
                    (
                        layout,
                        FrameData::D3d11 { texture: (*f).data[0] as *mut _, index: (*f).data[1] as usize as u32 },
                    )
                } else {
                    let h = (*f).height as usize;
                    let layout = match (*f).format {
                        x if x == ff::AV_PIX_FMT_YUV420P || x == ff::AV_PIX_FMT_YUVJ420P => PixelLayout::Yuv420p,
                        x if x == ff::AV_PIX_FMT_YUV444P || x == ff::AV_PIX_FMT_YUVJ444P => PixelLayout::Yuv444p,
                        x if x == ff::AV_PIX_FMT_NV12 => PixelLayout::Nv12Cpu,
                        x if x == ff::AV_PIX_FMT_YUV420P10LE => PixelLayout::Yuv420p10,
                        x => PixelLayout::Other(x),
                    };
                    let rows = |i: usize| match (layout, i) {
                        (PixelLayout::Yuv420p | PixelLayout::Yuv420p10, 1 | 2) | (PixelLayout::Nv12Cpu, 1) => h.div_ceil(2),
                        (PixelLayout::Nv12Cpu, 2) => 0,
                        _ => h,
                    };
                    let mut planes: [&[u8]; 3] = [&[], &[], &[]];
                    let mut strides = [0usize; 3];
                    for i in 0..3 {
                        let ls = (*f).linesize[i];
                        if (*f).data[i].is_null() || ls <= 0 {
                            continue;
                        }
                        strides[i] = ls as usize;
                        planes[i] = std::slice::from_raw_parts((*f).data[i], ls as usize * rows(i));
                    }
                    (layout, FrameData::Cpu { planes, strides })
                };
                on_frame(&DecodedFrame {
                    width: (*f).width as u32,
                    height: (*f).height as u32,
                    layout,
                    full_range,
                    matrix,
                    pq,
                    data,
                });
                ff::av_frame_unref(f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{Backend, EncoderConfig, VideoEncoder};

    /// Pure CPU: OpenH264 encode -> software H.264 decode.
    #[test]
    fn software_h264_roundtrip() {
        let (w, h) = (320u32, 240u32);
        let cfg = EncoderConfig {
            backend: Backend::Software,
            codec: VideoCodec::H264,
            yuv444: false,
            width: w,
            height: h,
            fps: 30,
            bitrate_kbps: 2000,
            game_mode: false,
            hdr: false,
        };
        let mut enc = VideoEncoder::open(&cfg, std::ptr::null_mut()).unwrap();
        let mut dec = VideoDecoder::new(VideoCodec::H264, std::ptr::null_mut()).unwrap();
        let mut nv12 = vec![128u8; (w * h * 3 / 2) as usize];
        let mut decoded = 0;
        for i in 0..10u32 {
            for y in 0..h {
                for x in 0..w {
                    nv12[(y * w + x) as usize] = ((x + y + i * 8) % 220 + 16) as u8;
                }
            }
            let mut pkts = Vec::new();
            enc.encode_nv12_cpu(&nv12, i == 0, &mut pkts).unwrap();
            for p in &pkts {
                if i == 0 {
                    assert!(p.keyframe);
                }
                dec.decode(&p.data, |f| {
                    assert_eq!((f.width, f.height), (w, h));
                    assert_eq!(f.layout, PixelLayout::Yuv420p);
                    decoded += 1;
                })
                .unwrap();
            }
        }
        assert!(decoded >= 9, "decoded {decoded} frames");
    }
}
