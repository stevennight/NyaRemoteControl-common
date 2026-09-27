//! Small safe helpers around raw FFmpeg calls.

use std::ffi::CString;
use std::os::raw::c_int;
use std::ptr;

use nya_ffmpeg_sys as ff;

#[derive(Debug, thiserror::Error)]
#[error("{what}: {msg} ({code})")]
pub struct FfError {
    pub what: String,
    pub code: c_int,
    pub msg: String,
}

pub fn check(code: c_int, what: &str) -> Result<c_int, FfError> {
    if code < 0 {
        Err(FfError { what: what.to_owned(), code, msg: ff::err_to_string(code) })
    } else {
        Ok(code)
    }
}

pub fn set_log_level(verbose: bool) {
    unsafe { ff::av_log_set_level(if verbose { ff::AV_LOG_VERBOSE as c_int } else { ff::AV_LOG_ERROR as c_int }) };
}

/// Fail early if the FFmpeg DLLs next to the executable don't match the bindings.
pub fn check_runtime_versions() -> anyhow::Result<()> {
    let (codec, util) = ff::runtime_versions();
    if codec != ff::EXPECTED_AVCODEC_MAJOR || util != ff::EXPECTED_AVUTIL_MAJOR {
        anyhow::bail!(
            "FFmpeg DLL 版本不匹配：avcodec {codec}/avutil {util}，需要 {}/{}",
            ff::EXPECTED_AVCODEC_MAJOR,
            ff::EXPECTED_AVUTIL_MAJOR
        );
    }
    Ok(())
}

/// Owned `AVDictionary` of codec options.
pub struct Dict(pub *mut ff::AVDictionary);

impl Dict {
    pub fn new() -> Self {
        Self(ptr::null_mut())
    }

    pub fn set(&mut self, k: &str, v: &str) {
        let k = CString::new(k).unwrap();
        let v = CString::new(v).unwrap();
        unsafe { ff::av_dict_set(&mut self.0, k.as_ptr(), v.as_ptr(), 0) };
    }

    /// Keys left over after `avcodec_open2` (= options the codec didn't accept).
    pub fn keys(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut e: *const ff::AVDictionaryEntry = ptr::null();
        let empty = CString::new("").unwrap();
        loop {
            e = unsafe { ff::av_dict_get(self.0, empty.as_ptr(), e, ff::AV_DICT_IGNORE_SUFFIX as c_int) };
            if e.is_null() {
                return out;
            }
            out.push(unsafe { std::ffi::CStr::from_ptr((*e).key).to_string_lossy().into_owned() });
        }
    }
}

impl Drop for Dict {
    fn drop(&mut self) {
        unsafe { ff::av_dict_free(&mut self.0) };
    }
}

pub struct Frame(pub *mut ff::AVFrame);

impl Frame {
    pub fn new() -> Self {
        Self(unsafe { ff::av_frame_alloc() })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { ff::av_frame_free(&mut self.0) };
    }
}

pub struct Packet(pub *mut ff::AVPacket);

impl Packet {
    pub fn new() -> Self {
        Self(unsafe { ff::av_packet_alloc() })
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        unsafe { ff::av_packet_free(&mut self.0) };
    }
}

pub struct BufRef(pub *mut ff::AVBufferRef);

impl BufRef {
    pub fn null() -> Self {
        Self(ptr::null_mut())
    }

    pub fn new_ref(&self) -> *mut ff::AVBufferRef {
        if self.0.is_null() {
            ptr::null_mut()
        } else {
            unsafe { ff::av_buffer_ref(self.0) }
        }
    }
}

impl Drop for BufRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ff::av_buffer_unref(&mut self.0) };
        }
    }
}

// FFmpeg objects are only used from the thread that owns the codec.
unsafe impl Send for Frame {}
unsafe impl Send for Packet {}
unsafe impl Send for BufRef {}
unsafe impl Send for Dict {}

/// Create a D3D11VA device context that takes ownership of one reference to `device`.
pub fn d3d11_device_ctx(device: *mut std::ffi::c_void) -> Result<BufRef, FfError> {
    unsafe {
        let r = ff::av_hwdevice_ctx_alloc(ff::AV_HWDEVICE_TYPE_D3D11VA);
        if r.is_null() {
            return Err(FfError { what: "av_hwdevice_ctx_alloc".into(), code: ff::AVERROR_ENOMEM, msg: "OOM".into() });
        }
        let buf = BufRef(r);
        let dev = (*r).data as *mut ff::AVHWDeviceContext;
        let d3d = (*dev).hwctx as *mut ff::AVD3D11VADeviceContext;
        (*d3d).device = device;
        check(ff::av_hwdevice_ctx_init(r), "av_hwdevice_ctx_init(d3d11va)")?;
        Ok(buf)
    }
}
