//! WASAPI shared-mode loopback capture (host) and playback (client), both as
//! 48 kHz stereo f32 with the audio engine doing any format conversion.

use anyhow::{Context, Result};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

fn float_format() -> WAVEFORMATEX {
    let block = CHANNELS * 4;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT as u16,
        nChannels: CHANNELS,
        nSamplesPerSec: SAMPLE_RATE,
        nAvgBytesPerSec: SAMPLE_RATE * block as u32,
        nBlockAlign: block,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

fn default_device(flow: EDataFlow) -> Result<IMMDevice> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("MMDeviceEnumerator")? };
    Ok(unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole).context("default audio endpoint")? })
}

fn open_client(flow: EDataFlow, extra_flags: u32, buffer_hns: i64) -> Result<IAudioClient> {
    let device = default_device(flow)?;
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let fmt = float_format();
    let flags = extra_flags | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    unsafe {
        client
            .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buffer_hns, 0, &fmt, None)
            .context("IAudioClient::Initialize")?;
    }
    let _ = WAVE_FORMAT_EXTENSIBLE; // format negotiation is left to AUTOCONVERTPCM
    Ok(client)
}

/// Captures whatever the default playback device is playing.
pub struct LoopbackCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
}

// COM objects are created in the MTA; the capture is used by one thread.
unsafe impl Send for LoopbackCapture {}

impl LoopbackCapture {
    /// Caller must have initialised COM on this thread ([`crate::com_init`]).
    pub fn new() -> Result<Self> {
        let client = open_client(eRender, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000)?;
        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        unsafe { client.Start()? };
        Ok(Self { client, capture })
    }

    /// Append all available interleaved stereo samples to `out`.
    /// Errors (e.g. device invalidated) mean the capture must be recreated.
    pub fn read(&mut self, out: &mut Vec<f32>) -> Result<()> {
        loop {
            let packet = unsafe { self.capture.GetNextPacketSize()? };
            if packet == 0 {
                return Ok(());
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
            let n = frames as usize * CHANNELS as usize;
            if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                out.extend(std::iter::repeat(0.0).take(n));
            } else {
                let s = unsafe { std::slice::from_raw_parts(data as *const f32, n) };
                out.extend_from_slice(s);
            }
            unsafe { self.capture.ReleaseBuffer(frames)? };
        }
    }
}

impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Plays interleaved stereo f32 on the default output device.
pub struct AudioRenderer {
    client: IAudioClient,
    render: IAudioRenderClient,
    buffer_frames: u32,
}

unsafe impl Send for AudioRenderer {}

impl AudioRenderer {
    pub fn new() -> Result<Self> {
        let client = open_client(eRender, 0, 1_000_000)?;
        let render: IAudioRenderClient = unsafe { client.GetService()? };
        let buffer_frames = unsafe { client.GetBufferSize()? };
        unsafe { client.Start()? };
        Ok(Self { client, render, buffer_frames })
    }

    /// Frames queued in the device buffer and not yet played.
    pub fn queued_frames(&self) -> Result<u32> {
        Ok(unsafe { self.client.GetCurrentPadding()? })
    }

    /// Write as many frames as fit; returns the number of frames consumed.
    pub fn write(&mut self, samples: &[f32]) -> Result<usize> {
        let free = self.buffer_frames - self.queued_frames()?;
        let frames = (samples.len() / CHANNELS as usize).min(free as usize);
        if frames == 0 {
            return Ok(0);
        }
        unsafe {
            let buf = self.render.GetBuffer(frames as u32)?;
            std::ptr::copy_nonoverlapping(samples.as_ptr(), buf as *mut f32, frames * CHANNELS as usize);
            self.render.ReleaseBuffer(frames as u32, 0)?;
        }
        Ok(frames)
    }
}

impl Drop for AudioRenderer {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}
