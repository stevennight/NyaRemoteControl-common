//! Media codecs on top of FFmpeg.
//!
//! * [`encoder`] – hardware video encoders fed with D3D11 textures
//!   (NVENC / QSV / AMF) and a software H.264 fallback (OpenH264)
//! * [`decoder`] – D3D11VA hardware decoding with software fallback
//! * [`audio`] – Opus encoder / decoder (libopus)

pub mod audio;
pub mod decoder;
pub mod encoder;
mod ff;

pub use ff::{check_runtime_versions, set_log_level, FfError};

/// Runtime (avcodec, avutil) major versions.
pub fn ffmpeg_versions() -> (u32, u32) {
    nya_ffmpeg_sys::runtime_versions()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }
}
