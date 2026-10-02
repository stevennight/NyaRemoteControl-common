//! WASAPI shared-mode capture (system loopback, microphone) and playback,
//! all as 48 kHz stereo f32 with the audio engine doing format conversion.

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

fn enumerator() -> Result<IMMDeviceEnumerator> {
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).context("MMDeviceEnumerator")? })
}

fn friendly_name(d: &IMMDevice) -> Option<String> {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::System::Com::STGM_READ;
    unsafe {
        let store = d.OpenPropertyStore(STGM_READ).ok()?;
        let v = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        Some(v.to_string())
    }
}

/// Names of the active playback devices.
pub fn render_device_names() -> Vec<String> {
    let Ok(e) = enumerator() else { return Vec::new() };
    let Ok(list) = (unsafe { e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }) else { return Vec::new() };
    let n = unsafe { list.GetCount() }.unwrap_or(0);
    (0..n).filter_map(|i| unsafe { list.Item(i) }.ok()).filter_map(|d| friendly_name(&d)).collect()
}

/// First active playback device whose name contains `part` (case-insensitive).
pub fn find_render_device(part: &str) -> Option<(IMMDevice, String)> {
    let e = enumerator().ok()?;
    let list = unsafe { e.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }.ok()?;
    let n = unsafe { list.GetCount() }.ok()?;
    let part = part.to_lowercase();
    (0..n).filter_map(|i| unsafe { list.Item(i) }.ok()).find_map(|d| {
        let name = friendly_name(&d)?;
        name.to_lowercase().contains(&part).then_some((d, name))
    })
}

fn device_id(d: &IMMDevice) -> Option<String> {
    unsafe {
        let p = d.GetId().ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Endpoint id and name of the first active recording device whose name contains `part`.
pub fn find_capture_device(part: &str) -> Option<(String, String)> {
    let e = enumerator().ok()?;
    let list = unsafe { e.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) }.ok()?;
    let n = unsafe { list.GetCount() }.ok()?;
    let part = part.to_lowercase();
    (0..n).filter_map(|i| unsafe { list.Item(i) }.ok()).find_map(|d| {
        let name = friendly_name(&d)?;
        if name.to_lowercase().contains(&part) {
            Some((device_id(&d)?, name))
        } else {
            None
        }
    })
}

/// Endpoint id of the default recording device for `role` (eConsole, eCommunications).
pub fn default_capture_id(role: ERole) -> Option<String> {
    let e = enumerator().ok()?;
    let d = unsafe { e.GetDefaultAudioEndpoint(eCapture, role) }.ok()?;
    device_id(&d)
}

/// Make endpoint `id` the default device for `role`, like the Sound control
/// panel does (the undocumented but long-stable IPolicyConfig interface).
pub fn set_default_endpoint(id: &str, role: ERole) -> Result<()> {
    use std::ffi::c_void;
    use windows::core::{IUnknown, Interface, GUID, HRESULT, HSTRING, PCWSTR};
    const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c3bc9);
    const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);
    // IUnknown (3) + GetMixFormat .. SetPropertyValue (10): SetDefaultEndpoint is slot 13.
    const SET_DEFAULT_ENDPOINT: usize = 13;
    type SetDefault = unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT;
    unsafe {
        let unk: IUnknown = CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL).context("PolicyConfig")?;
        let mut p: *mut c_void = std::ptr::null_mut();
        unk.query(&IID_POLICY_CONFIG, &mut p).ok().context("IPolicyConfig")?;
        // Owns the reference QueryInterface added; released on drop.
        let policy = IUnknown::from_raw(p);
        let vtbl = *(p as *const *const usize);
        let f: SetDefault = std::mem::transmute(*vtbl.add(SET_DEFAULT_ENDPOINT));
        let w = HSTRING::from(id);
        let hr = f(p, PCWSTR(w.as_ptr()), role);
        drop(policy);
        hr.ok().context("SetDefaultEndpoint")?;
    }
    Ok(())
}

fn open_client(flow: EDataFlow, extra_flags: u32, buffer_hns: i64) -> Result<IAudioClient> {

    let device = default_device(flow)?;
    open_device(&device, extra_flags, buffer_hns)
}

fn open_device(device: &IMMDevice, extra_flags: u32, buffer_hns: i64) -> Result<IAudioClient> {
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

/// Is the default playback device's name containing `part` (case-insensitive)?
pub fn default_render_is(part: &str) -> bool {
    default_device(eRender)
        .ok()
        .and_then(|d| friendly_name(&d))
        .is_some_and(|n| n.to_lowercase().contains(&part.to_lowercase()))
}

#[windows::core::implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivateDone(std::sync::mpsc::SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivateDone_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

fn process_loopback_client(pid: u32) -> Result<IAudioClient> {
    use std::time::Duration;
    use windows::core::{Interface, PROPVARIANT};

    // PROPVARIANT holding a VT_BLOB that points at the activation parameters.
    #[repr(C)]
    struct BlobVariant {
        vt: u16,
        reserved: [u16; 3],
        size: u32,
        data: *const AUDIOCLIENT_ACTIVATION_PARAMS,
    }
    const _: () = assert!(std::mem::size_of::<BlobVariant>() == std::mem::size_of::<PROPVARIANT>());
    const VT_BLOB: u16 = 65;

    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let var = BlobVariant {
        vt: VT_BLOB,
        reserved: [0; 3],
        size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        data: &params,
    };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = ActivateDone(tx).into();
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&var as *const BlobVariant as *const PROPVARIANT),
            &handler,
        )
        .context("ActivateAudioInterfaceAsync")?
    };
    rx.recv_timeout(Duration::from_secs(5)).context("process loopback activation timed out")?;
    let mut hr = windows::core::HRESULT(0);
    let mut iface = None;
    unsafe { op.GetActivateResult(&mut hr, &mut iface)? };
    hr.ok().context("process loopback activation")?;
    iface.context("no audio client")?.cast::<IAudioClient>().context("IAudioClient")
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

    /// Everything the system plays except audio rendered by process `pid` and
    /// its children (process loopback, Windows 10 2004+ / build 20348+). Used
    /// so the host never records the microphone it plays into VB-Cable,
    /// whichever device is the default.
    pub fn excluding_process(pid: u32) -> Result<Self> {
        let client = process_loopback_client(pid)?;
        let fmt = float_format();
        unsafe {
            client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                    2_000_000,
                    0,
                    &fmt,
                    None,
                )
                .context("IAudioClient::Initialize (process loopback)")?;
            // Event mode is required here; the event is never waited on, `read` polls.
            let event = windows::Win32::System::Threading::CreateEventW(None, false, false, None)?;
            client.SetEventHandle(event)?;
        }
        let capture: IAudioCaptureClient = unsafe { client.GetService()? };
        unsafe { client.Start()? };
        Ok(Self { client, capture })
    }

    /// The default recording device (microphone) instead of the loopback.
    pub fn microphone() -> Result<Self> {
        let client = open_client(eCapture, 0, 400_000)?;
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
        Self::with_client(open_client(eRender, 0, 1_000_000)?)
    }

    /// Play on a specific device (e.g. the VB-Cable input for the microphone).
    pub fn on_device(device: &IMMDevice) -> Result<Self> {
        Self::with_client(open_device(device, 0, 1_000_000)?)
    }

    fn with_client(client: IAudioClient) -> Result<Self> {
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
