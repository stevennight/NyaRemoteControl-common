//! What NVIDIA's NVDEC can decode, asked of the driver itself
//! (`cuvidGetDecoderCaps`). Used where D3D11VA lacks a format NVDEC has
//! (typically HEVC 4:4:4); decoding then goes through FFmpeg's `*_cuvid`
//! decoders ([`crate::decoder::VideoDecoder::new_nvdec`]).
//!
//! The CUDA driver (`nvcuda.dll`, `nvcuvid.dll`) is loaded at run time:
//! machines without an NVIDIA driver simply report nothing.

use std::ffi::{c_void, CString};
use std::sync::OnceLock;

use crate::VideoCodec;

#[repr(C)]
#[derive(Default)]
struct DecodeCaps {
    codec: i32,
    chroma: i32,
    bit_depth_minus8: u32,
    reserved1: [u32; 3],
    supported: u8,
    num_nvdecs: u8,
    output_format_mask: u16,
    max_width: u32,
    max_height: u32,
    max_mb_count: u32,
    min_width: u16,
    min_height: u16,
    // Newer drivers write more (histogram fields, reserved words): room to spare.
    rest: [u32; 24],
}

const CODEC_H264: i32 = 4;
const CODEC_HEVC: i32 = 8;
const CODEC_AV1: i32 = 11;
const CHROMA_420: i32 = 1;
const CHROMA_444: i32 = 3;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryA(name: *const i8) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const i8) -> *mut c_void;
}

unsafe fn sym<T>(lib: *mut c_void, name: &str) -> Option<T> {
    let n = CString::new(name).ok()?;
    let p = GetProcAddress(lib, n.as_ptr());
    (!p.is_null()).then(|| std::mem::transmute_copy(&p))
}

/// One decodable format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub codec: VideoCodec,
    pub yuv444: bool,
    pub ten_bit: bool,
    pub max_width: u32,
    pub max_height: u32,
}

fn probe() -> Vec<Format> {
    // SAFETY: documented CUDA driver entry points; the context lives only here.
    unsafe {
        let cuda = LoadLibraryA(c"nvcuda.dll".as_ptr());
        let cuvid = LoadLibraryA(c"nvcuvid.dll".as_ptr());
        if cuda.is_null() || cuvid.is_null() {
            return Vec::new();
        }
        type Init = unsafe extern "system" fn(u32) -> i32;
        type DeviceGet = unsafe extern "system" fn(*mut i32, i32) -> i32;
        type CtxCreate = unsafe extern "system" fn(*mut *mut c_void, u32, i32) -> i32;
        type CtxDestroy = unsafe extern "system" fn(*mut c_void) -> i32;
        type GetCaps = unsafe extern "system" fn(*mut DecodeCaps) -> i32;
        let (Some(init), Some(device_get), Some(ctx_create), Some(ctx_destroy), Some(get_caps)) = (
            sym::<Init>(cuda, "cuInit"),
            sym::<DeviceGet>(cuda, "cuDeviceGet"),
            sym::<CtxCreate>(cuda, "cuCtxCreate_v2"),
            sym::<CtxDestroy>(cuda, "cuCtxDestroy_v2"),
            sym::<GetCaps>(cuvid, "cuvidGetDecoderCaps"),
        ) else {
            return Vec::new();
        };
        let mut dev = 0;
        let mut ctx = std::ptr::null_mut();
        if init(0) != 0 || device_get(&mut dev, 0) != 0 || ctx_create(&mut ctx, 0, dev) != 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (codec, c, yuv444, ten_bit) in [
            (VideoCodec::H264, CODEC_H264, false, false),
            (VideoCodec::Hevc, CODEC_HEVC, false, false),
            (VideoCodec::Hevc, CODEC_HEVC, true, false),
            (VideoCodec::Hevc, CODEC_HEVC, false, true),
            (VideoCodec::Av1, CODEC_AV1, false, false),
        ] {
            let mut caps = DecodeCaps {
                codec: c,
                chroma: if yuv444 { CHROMA_444 } else { CHROMA_420 },
                bit_depth_minus8: if ten_bit { 2 } else { 0 },
                ..Default::default()
            };
            if get_caps(&mut caps) == 0 && caps.supported != 0 {
                out.push(Format { codec, yuv444, ten_bit, max_width: caps.max_width, max_height: caps.max_height });
            }
        }
        ctx_destroy(ctx);
        out
    }
}

/// Formats NVDEC decodes on this machine's first NVIDIA GPU (probed once).
pub fn formats() -> &'static [Format] {
    static F: OnceLock<Vec<Format>> = OnceLock::new();
    F.get_or_init(|| {
        let f = probe();
        // FFmpeg must have the matching cuvid decoder too.
        let f: Vec<Format> = f.into_iter().filter(|x| crate::decoder::nvdec_available(x.codec)).collect();
        if !f.is_empty() {
            tracing::info!("NVDEC decodes: {f:?}");
        }
        f
    })
}

pub fn supports(codec: VideoCodec, yuv444: bool, ten_bit: bool) -> bool {
    formats().iter().any(|f| f.codec == codec && f.yuv444 == yuv444 && f.ten_bit == ten_bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_struct_has_room() {
        // The SDK's CUVIDDECODECAPS is 88 bytes today.
        assert!(std::mem::size_of::<DecodeCaps>() >= 88);
        assert_eq!(std::mem::offset_of!(DecodeCaps, supported), 24);
        assert_eq!(std::mem::offset_of!(DecodeCaps, max_width), 28);
    }

    #[test]
    fn probing_without_nvidia_is_harmless() {
        // On machines without the driver this is empty; with it, a list.
        let _ = formats();
    }
}
