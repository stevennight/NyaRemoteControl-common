//! Low-latency video encoders.
//!
//! Hardware backends take D3D11 textures from an FFmpeg frame pool: the
//! caller asks for an [`InputSurface`], renders into `texture[index]` on the
//! encoder's GPU, then passes the surface back to [`VideoEncoder::encode`].
//!
//! | backend  | 4:2:0 input | 4:4:4 input                                  |
//! |----------|-------------|----------------------------------------------|
//! | NVENC    | NV12 (BGRA if the driver can't render to NV12) | BGRA + `rgb_mode=yuv444` (NVENC converts) |
//! | QSV      | NV12        | VUYX / AYUV (HEVC only)                       |
//! | AMF      | NV12        | —                                            |
//! | software | CPU YUV420P | —                                            |
//!
//! HDR10 (`EncoderConfig::hdr`, HEVC Main10 on NVENC / QSV / AMF): P010
//! input holding BT.2020 PQ, tagged as such in the bitstream.

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;

use anyhow::{anyhow, bail, Result};
use nya_ffmpeg_sys as ff;

use crate::ff::{check, d3d11_device_ctx, BufRef, Dict, Frame, Packet};
use crate::VideoCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    Nvenc,
    Qsv,
    Amf,
    Software,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Nvenc => "nvenc",
            Self::Qsv => "qsv",
            Self::Amf => "amf",
            Self::Software => "software",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "nvenc" | "nvidia" => Some(Self::Nvenc),
            "qsv" | "intel" => Some(Self::Qsv),
            "amf" | "amd" => Some(Self::Amf),
            "software" | "sw" | "cpu" => Some(Self::Software),
            _ => None,
        }
    }

    pub fn encoder_name(self, codec: VideoCodec) -> Option<&'static str> {
        Some(match (self, codec) {
            (Self::Nvenc, VideoCodec::H264) => "h264_nvenc",
            (Self::Nvenc, VideoCodec::Hevc) => "hevc_nvenc",
            (Self::Nvenc, VideoCodec::Av1) => "av1_nvenc",
            (Self::Qsv, VideoCodec::H264) => "h264_qsv",
            (Self::Qsv, VideoCodec::Hevc) => "hevc_qsv",
            (Self::Qsv, VideoCodec::Av1) => "av1_qsv",
            (Self::Amf, VideoCodec::H264) => "h264_amf",
            (Self::Amf, VideoCodec::Hevc) => "hevc_amf",
            (Self::Amf, VideoCodec::Av1) => "av1_amf",
            (Self::Software, VideoCodec::H264) => "libopenh264",
            _ => return None,
        })
    }

    pub fn is_hardware(self) -> bool {
        self != Self::Software
    }

    /// Whether this backend can produce HDR10 (10-bit 4:2:0, PQ) for `codec`.
    pub fn supports_hdr(self, codec: VideoCodec) -> bool {
        matches!((self, codec), (Self::Nvenc | Self::Qsv | Self::Amf, VideoCodec::Hevc))
    }

    /// Whether this backend can produce 4:4:4 for `codec` at all.
    pub fn supports_444(self, codec: VideoCodec) -> bool {
        matches!((self, codec), (Self::Nvenc, VideoCodec::H264 | VideoCodec::Hevc) | (Self::Qsv, VideoCodec::Hevc))
    }
}

/// What the encoder wants rendered into its input surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFormat {
    Nv12,
    Bgra,
    Ayuv,
    /// 10-bit 4:2:0 (HDR10).
    P010,
    /// CPU memory, filled with [`VideoEncoder::encode_nv12_cpu`].
    CpuNv12,
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub backend: Backend,
    pub codec: VideoCodec,
    pub yuv444: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Game mode: fastest preset, CBR, tight VBV. Office: better quality, room for bursts.
    pub game_mode: bool,
    /// HDR10: P010 input (BT.2020, PQ), Main10 profile.
    pub hdr: bool,
}

pub struct EncodedPacket {
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// A pool texture to render the next frame into.
pub struct InputSurface {
    /// `ID3D11Texture2D*` (borrowed; valid while the surface lives).
    pub texture: *mut std::ffi::c_void,
    /// Array slice to render into.
    pub index: u32,
    frame: Frame,
    _mapped: Option<Frame>,
}

pub struct VideoEncoder {
    ctx: *mut ff::AVCodecContext,
    _device: BufRef,
    _qsv_device: BufRef,
    frames: BufRef,
    input: InputFormat,
    cfg: EncoderConfig,
    pkt: Packet,
    pts: i64,
    name: &'static str,
}

unsafe impl Send for VideoEncoder {}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        unsafe { ff::avcodec_free_context(&mut self.ctx) };
    }
}

fn pool_frames(device: &BufRef, format: ff::AVPixelFormat, sw: ff::AVPixelFormat, w: u32, h: u32, size: c_int) -> Result<BufRef> {
    unsafe {
        let r = ff::av_hwframe_ctx_alloc(device.0);
        if r.is_null() {
            bail!("av_hwframe_ctx_alloc failed");
        }
        let buf = BufRef(r);
        let fc = (*r).data as *mut ff::AVHWFramesContext;
        (*fc).format = format;
        (*fc).sw_format = sw;
        (*fc).width = w as c_int;
        (*fc).height = h as c_int;
        (*fc).initial_pool_size = size;
        if format == ff::AV_PIX_FMT_D3D11 {
            let hw = (*fc).hwctx as *mut ff::AVD3D11VAFramesContext;
            (*hw).BindFlags = ff::D3D11_BIND_RENDER_TARGET;
        } else if format == ff::AV_PIX_FMT_QSV {
            let hw = (*fc).hwctx as *mut ff::AVQSVFramesContext;
            (*hw).frame_type = ff::MFX_MEMTYPE_VIDEO_MEMORY_PROCESSOR_TARGET;
        }
        check(ff::av_hwframe_ctx_init(r), "av_hwframe_ctx_init")?;
        Ok(buf)
    }
}

impl VideoEncoder {
    /// Open an encoder. For hardware backends `d3d_device` must be an
    /// AddRef'd `ID3D11Device*` on the encoding GPU; ownership of that
    /// reference passes to the encoder (also on error).
    pub fn open(cfg: &EncoderConfig, d3d_device: *mut std::ffi::c_void) -> Result<Self> {
        let name = cfg
            .backend
            .encoder_name(cfg.codec)
            .ok_or_else(|| anyhow!("{} 不支持 {}", cfg.backend.name(), cfg.codec.name()))?;
        if cfg.yuv444 && !cfg.backend.supports_444(cfg.codec) {
            bail!("{name} 不支持 4:4:4");
        }
        if cfg.hdr && (cfg.yuv444 || !cfg.backend.supports_hdr(cfg.codec)) {
            bail!("{name} 不支持 HDR10（需要 HEVC 4:2:0 硬件编码）");
        }
        if cfg.width % 2 != 0 || cfg.height % 2 != 0 || cfg.width == 0 || cfg.height == 0 {
            bail!("分辨率必须为非零偶数：{}x{}", cfg.width, cfg.height);
        }

        let mut device = BufRef::null();
        let mut qsv_device = BufRef::null();
        let input;
        let frames;
        let (w, h) = (cfg.width, cfg.height);
        match cfg.backend {
            Backend::Software => {
                if !d3d_device.is_null() {
                    // We don't need the device; drop the reference we were given.
                    drop(d3d11_device_ctx(d3d_device));
                }
                input = InputFormat::CpuNv12;
                frames = BufRef::null();
            }
            Backend::Nvenc | Backend::Amf => {
                if d3d_device.is_null() {
                    bail!("hardware encoder needs a D3D11 device");
                }
                device = d3d11_device_ctx(d3d_device)?;
                if cfg.hdr {
                    let d3d = ff::AV_PIX_FMT_D3D11;
                    frames = pool_frames(&device, d3d, ff::AV_PIX_FMT_P010LE, w, h, 6)
                        .or_else(|_| pool_frames(&device, d3d, ff::AV_PIX_FMT_P010LE, w, h, 0))?;
                    input = InputFormat::P010;
                } else if cfg.yuv444 {
                    input = InputFormat::Bgra;
                    frames = pool_frames(&device, ff::AV_PIX_FMT_D3D11, ff::AV_PIX_FMT_BGRA, w, h, 6)?;
                } else {
                    // Many drivers reject NV12 texture *arrays* bound as render targets
                    // (E_INVALIDARG). Fall back to individually allocated textures, and
                    // for NVENC finally to BGRA input converted by NVENC itself.
                    let d3d = ff::AV_PIX_FMT_D3D11;
                    match pool_frames(&device, d3d, ff::AV_PIX_FMT_NV12, w, h, 6)
                        .or_else(|_| pool_frames(&device, d3d, ff::AV_PIX_FMT_NV12, w, h, 0))
                    {
                        Ok(f) => {
                            input = InputFormat::Nv12;
                            frames = f;
                        }
                        Err(e) if cfg.backend == Backend::Nvenc => {
                            tracing::info!("NV12 encoder surfaces unavailable ({e:#}); using BGRA input");
                            input = InputFormat::Bgra;
                            frames = pool_frames(&device, d3d, ff::AV_PIX_FMT_BGRA, w, h, 6)?;
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            Backend::Qsv => {
                if d3d_device.is_null() {
                    bail!("hardware encoder needs a D3D11 device");
                }
                device = d3d11_device_ctx(d3d_device)?;
                let mut q = ptr::null_mut();
                check(
                    unsafe { ff::av_hwdevice_ctx_create_derived(&mut q, ff::AV_HWDEVICE_TYPE_QSV, device.0, 0) },
                    "derive QSV device",
                )?;
                qsv_device = BufRef(q);
                let (sw, inp) = if cfg.hdr {
                    (ff::AV_PIX_FMT_P010LE, InputFormat::P010)
                } else if cfg.yuv444 {
                    (ff::AV_PIX_FMT_VUYX, InputFormat::Ayuv)
                } else {
                    (ff::AV_PIX_FMT_NV12, InputFormat::Nv12)
                };
                input = inp;
                frames = pool_frames(&qsv_device, ff::AV_PIX_FMT_QSV, sw, w, h, 16)?;
            }
        }

        let cname = CString::new(name).unwrap();
        let codec = unsafe { ff::avcodec_find_encoder_by_name(cname.as_ptr()) };
        if codec.is_null() {
            bail!("FFmpeg 中没有编码器 {name}");
        }
        let ctx = unsafe { ff::avcodec_alloc_context3(codec) };
        if ctx.is_null() {
            bail!("avcodec_alloc_context3 failed");
        }
        let mut enc = Self {
            ctx,
            _device: device,
            _qsv_device: qsv_device,
            frames,
            input,
            cfg: cfg.clone(),
            pkt: Packet::new(),
            pts: 0,
            name,
        };
        enc.configure_and_open(codec)?;
        Ok(enc)
    }

    fn configure_and_open(&mut self, codec: *const ff::AVCodec) -> Result<()> {
        let cfg = &self.cfg;
        let fps = cfg.fps.max(1) as c_int;
        let bitrate = cfg.bitrate_kbps.max(100) as i64 * 1000;
        let c = self.ctx;
        unsafe {
            (*c).width = cfg.width as c_int;
            (*c).height = cfg.height as c_int;
            (*c).time_base = ff::AVRational { num: 1, den: fps };
            (*c).framerate = ff::AVRational { num: fps, den: 1 };
            // Keyframes only on demand (client request / new stream).
            (*c).gop_size = if cfg.backend == Backend::Software { fps * 60 } else { i32::MAX / 2 };
            (*c).max_b_frames = 0;
            (*c).bit_rate = bitrate;
            (*c).rc_max_rate = if cfg.game_mode { bitrate } else { bitrate * 3 / 2 };
            // VBV: ~1 frame in game mode, ~4 frames in office mode (sharper keyframes).
            let frames_in_vbv = if cfg.game_mode { 1 } else { 4 };
            (*c).rc_buffer_size = ((bitrate / fps as i64) * frames_in_vbv).min(i32::MAX as i64) as c_int;
            (*c).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*c).color_range = ff::AVCOL_RANGE_MPEG;
            if cfg.hdr {
                (*c).colorspace = ff::AVCOL_SPC_BT2020_NCL;
                (*c).color_primaries = ff::AVCOL_PRI_BT2020;
                (*c).color_trc = ff::AVCOL_TRC_SMPTE2084;
            } else {
                (*c).colorspace = ff::AVCOL_SPC_BT709;
                (*c).color_primaries = ff::AVCOL_PRI_BT709;
                (*c).color_trc = ff::AVCOL_TRC_BT709;
            }
            match self.input {
                InputFormat::CpuNv12 => {
                    (*c).pix_fmt = ff::AV_PIX_FMT_YUV420P;
                }
                _ => {
                    let fc = (*self.frames.0).data as *const ff::AVHWFramesContext;
                    (*c).pix_fmt = (*fc).format;
                    (*c).sw_pix_fmt = (*fc).sw_format;
                    (*c).hw_frames_ctx = self.frames.new_ref();
                }
            }
        }

        let mut opts = Dict::new();
        let game = cfg.game_mode;
        match cfg.backend {
            Backend::Nvenc => {
                opts.set("preset", if game { "p1" } else { "p4" });
                opts.set("tune", if game { "ull" } else { "ll" });
                opts.set("rc", if game { "cbr" } else { "vbr" });
                opts.set("zerolatency", "1");
                opts.set("delay", "0");
                opts.set("forced-idr", "1");
                opts.set("b_ref_mode", "disabled");
                if cfg.yuv444 {
                    opts.set("rgb_mode", "yuv444");
                    opts.set("profile", if cfg.codec == VideoCodec::H264 { "high444p" } else { "rext" });
                }
                if cfg.hdr {
                    opts.set("profile", "main10");
                }
            }
            Backend::Qsv => {
                opts.set("preset", if game { "veryfast" } else { "medium" });
                opts.set("async_depth", "1");
                opts.set("forced_idr", "1");
                if cfg.codec == VideoCodec::H264 {
                    opts.set("look_ahead", "0");
                }
                if cfg.yuv444 {
                    opts.set("profile", "rext");
                }
                if cfg.hdr {
                    opts.set("profile", "main10");
                }
            }
            Backend::Amf => {
                opts.set("usage", if game { "ultralowlatency" } else { "lowlatency" });
                opts.set("quality", if game { "speed" } else { "balanced" });
                opts.set("rc", "cbr");
                if cfg.hdr {
                    opts.set("profile", "main10");
                }
            }
            Backend::Software => {
                opts.set("allow_skip_frames", "0");
            }
        }
        let r = unsafe { ff::avcodec_open2(self.ctx, codec, &mut opts.0) };
        check(r, &format!("avcodec_open2({})", self.name))?;
        let unused = opts.keys();
        if !unused.is_empty() {
            tracing::debug!("{} ignored options: {:?}", self.name, unused);
        }
        Ok(())
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Change the target bitrate while encoding. NVENC and QSV reconfigure on the
    /// next frame without a keyframe; returns false for encoders that can't.
    pub fn set_bitrate(&mut self, kbps: u32) -> bool {
        if !matches!(self.cfg.backend, Backend::Nvenc | Backend::Qsv) {
            return false;
        }
        let fps = self.cfg.fps.max(1) as i64;
        let bitrate = kbps.max(100) as i64 * 1000;
        let frames_in_vbv = if self.cfg.game_mode { 1 } else { 4 };
        unsafe {
            let c = self.ctx;
            (*c).bit_rate = bitrate;
            (*c).rc_max_rate = if self.cfg.game_mode { bitrate } else { bitrate * 3 / 2 };
            (*c).rc_buffer_size = ((bitrate / fps) * frames_in_vbv).min(i32::MAX as i64) as c_int;
        }
        self.cfg.bitrate_kbps = kbps;
        true
    }

    pub fn input_format(&self) -> InputFormat {
        self.input
    }

    /// Get the next pool texture to render into (hardware backends).
    pub fn surface(&mut self) -> Result<InputSurface> {
        if self.input == InputFormat::CpuNv12 {
            bail!("software encoder has no GPU surfaces");
        }
        let frame = Frame::new();
        check(unsafe { ff::av_hwframe_get_buffer(self.frames.0, frame.0, 0) }, "av_hwframe_get_buffer")?;
        unsafe {
            if (*frame.0).format == ff::AV_PIX_FMT_QSV {
                // Map the QSV surface to its D3D11 texture so we can render into it.
                let mapped = Frame::new();
                (*mapped.0).format = ff::AV_PIX_FMT_D3D11;
                check(
                    ff::av_hwframe_map(
                        mapped.0,
                        frame.0,
                        (ff::AV_HWFRAME_MAP_WRITE | ff::AV_HWFRAME_MAP_OVERWRITE) as c_int,
                    ),
                    "map QSV surface to D3D11",
                )?;
                return Ok(InputSurface {
                    texture: (*mapped.0).data[0] as *mut _,
                    index: (*mapped.0).data[1] as usize as u32,
                    frame,
                    _mapped: Some(mapped),
                });
            }
            Ok(InputSurface {
                texture: (*frame.0).data[0] as *mut _,
                index: (*frame.0).data[1] as usize as u32,
                frame,
                _mapped: None,
            })
        }
    }

    /// Encode a surface previously returned by [`surface`](Self::surface).
    pub fn encode(&mut self, surface: InputSurface, keyframe: bool, out: &mut Vec<EncodedPacket>) -> Result<()> {
        let InputSurface { frame, _mapped, .. } = surface;
        // Unmap before encoding so the QSV surface is not locked by the mapping.
        drop(_mapped);
        self.send(&frame, keyframe, out)
    }

    /// Software path: `nv12` holds a tightly packed NV12 image.
    pub fn encode_nv12_cpu(&mut self, nv12: &[u8], keyframe: bool, out: &mut Vec<EncodedPacket>) -> Result<()> {
        let (w, h) = (self.cfg.width as usize, self.cfg.height as usize);
        if nv12.len() < w * h * 3 / 2 {
            bail!("NV12 buffer too small");
        }
        let frame = Frame::new();
        unsafe {
            let f = frame.0;
            (*f).format = ff::AV_PIX_FMT_YUV420P;
            (*f).width = w as c_int;
            (*f).height = h as c_int;
            check(ff::av_frame_get_buffer(f, 32), "av_frame_get_buffer")?;
            for y in 0..h {
                ptr::copy_nonoverlapping(nv12.as_ptr().add(y * w), (*f).data[0].add(y * (*f).linesize[0] as usize), w);
            }
            let uv = &nv12[w * h..];
            for y in 0..h / 2 {
                let urow = (*f).data[1].add(y * (*f).linesize[1] as usize);
                let vrow = (*f).data[2].add(y * (*f).linesize[2] as usize);
                for x in 0..w / 2 {
                    *urow.add(x) = uv[y * w + 2 * x];
                    *vrow.add(x) = uv[y * w + 2 * x + 1];
                }
            }
        }
        self.send(&frame, keyframe, out)
    }

    fn send(&mut self, frame: &Frame, keyframe: bool, out: &mut Vec<EncodedPacket>) -> Result<()> {
        unsafe {
            (*frame.0).pts = self.pts;
            self.pts += 1;
            if keyframe {
                (*frame.0).pict_type = ff::AV_PICTURE_TYPE_I;
                (*frame.0).flags |= ff::AV_FRAME_FLAG_KEY as c_int;
            }
            check(ff::avcodec_send_frame(self.ctx, frame.0), "avcodec_send_frame")?;
            loop {
                let r = ff::avcodec_receive_packet(self.ctx, self.pkt.0);
                if r == ff::AVERROR_EAGAIN || r == ff::AVERROR_EOF {
                    return Ok(());
                }
                check(r, "avcodec_receive_packet")?;
                let p = self.pkt.0;
                out.push(EncodedPacket {
                    data: std::slice::from_raw_parts((*p).data, (*p).size as usize).to_vec(),
                    keyframe: (*p).flags & ff::AV_PKT_FLAG_KEY as c_int != 0,
                });
                ff::av_packet_unref(p);
            }
        }
    }
}
