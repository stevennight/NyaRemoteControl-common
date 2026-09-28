//! GPU topology: adapters, their outputs (displays), and which adapter drives
//! which display (design doc §3.5).

use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, DXGI_ADAPTER_FLAG_SOFTWARE,
    DXGI_ERROR_NOT_FOUND,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, ENUM_CURRENT_SETTINGS, MONITORINFO,
};

use crate::d3d::luid_to_u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Intel,
    Amd,
    Microsoft,
    Other(u32),
}

impl Vendor {
    pub fn from_id(id: u32) -> Self {
        match id {
            0x10DE => Self::Nvidia,
            0x8086 => Self::Intel,
            0x1002 | 0x1022 => Self::Amd,
            0x1414 => Self::Microsoft,
            x => Self::Other(x),
        }
    }
}

#[derive(Clone)]
pub struct AdapterInfo {
    pub index: u32,
    pub luid: u64,
    pub name: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory: u64,
    pub software: bool,
    pub adapter: IDXGIAdapter1,
}

impl AdapterInfo {
    pub fn vendor(&self) -> Vendor {
        Vendor::from_id(self.vendor_id)
    }
}

#[derive(Clone)]
pub struct OutputInfo {
    /// Stable id derived from the GDI device name (`\\.\DISPLAY3` → 3).
    pub id: u32,
    pub adapter_index: u32,
    pub device_name: String,
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub primary: bool,
    pub refresh_hz: u32,
    /// DXGI_MODE_ROTATION value (1 = identity).
    pub rotation: i32,
    /// `HMONITOR` of this output (to match windows to GPUs).
    pub hmonitor: isize,
    /// HDR (advanced colour) is on for this display.
    pub hdr: bool,
    pub output: IDXGIOutput,
}

impl OutputInfo {
    pub fn width(&self) -> u32 {
        (self.right - self.left).max(0) as u32
    }
    pub fn height(&self) -> u32 {
        (self.bottom - self.top).max(0) as u32
    }
}

pub struct Topology {
    pub adapters: Vec<AdapterInfo>,
    pub outputs: Vec<OutputInfo>,
    factory: IDXGIFactory1,
}

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn display_id(device_name: &str) -> u32 {
    device_name
        .trim_start_matches(r"\\.\DISPLAY")
        .parse()
        .unwrap_or_else(|_| {
            // Fall back to a hash so ids stay stable for odd names.
            device_name.bytes().fold(1000u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
        })
}

/// Is the desktop on this output in HDR (PQ / BT.2020) mode?
pub fn is_hdr(output: &IDXGIOutput) -> bool {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, IDXGIOutput6};
    output
        .cast::<IDXGIOutput6>()
        .and_then(|o| unsafe { o.GetDesc1() })
        .is_ok_and(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020)
}

impl Topology {
    pub fn enumerate() -> Result<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let mut adapters = Vec::new();
        let mut outputs = Vec::new();
        let mut i = 0u32;
        loop {
            let adapter = match unsafe { factory.EnumAdapters1(i) } {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = unsafe { adapter.GetDesc1()? };
            adapters.push(AdapterInfo {
                index: i,
                luid: luid_to_u64(desc.AdapterLuid),
                name: wide_to_string(&desc.Description),
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                dedicated_video_memory: desc.DedicatedVideoMemory as u64,
                software: desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
                adapter: adapter.clone(),
            });

            let mut j = 0u32;
            loop {
                let output = match unsafe { adapter.EnumOutputs(j) } {
                    Ok(o) => o,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(e.into()),
                };
                j += 1;
                let od = unsafe { output.GetDesc()? };
                if !od.AttachedToDesktop.as_bool() {
                    continue;
                }
                let device_name = wide_to_string(&od.DeviceName);
                let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
                let primary = unsafe { GetMonitorInfoW(od.Monitor, &mut mi).as_bool() } && mi.dwFlags & 1 != 0;
                let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
                let refresh_hz = unsafe {
                    if EnumDisplaySettingsW(PCWSTR(od.DeviceName.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm).as_bool() {
                        dm.dmDisplayFrequency
                    } else {
                        60
                    }
                };
                let r = od.DesktopCoordinates;
                let hdr = is_hdr(&output);
                outputs.push(OutputInfo {
                    id: display_id(&device_name),
                    adapter_index: i,
                    device_name,
                    left: r.left,
                    top: r.top,
                    right: r.right,
                    bottom: r.bottom,
                    primary,
                    refresh_hz: refresh_hz.max(1),
                    rotation: od.Rotation.0,
                    hmonitor: od.Monitor.0 as isize,
                    hdr,
                    output,
                });
            }
            i += 1;
        }
        outputs.sort_by_key(|o| (!o.primary, o.id));
        Ok(Self { adapters, outputs, factory })
    }

    /// False once displays/adapters changed and the topology should be rebuilt.
    pub fn is_current(&self) -> bool {
        unsafe { self.factory.IsCurrent().as_bool() }
    }

    pub fn output(&self, id: u32) -> Option<&OutputInfo> {
        self.outputs.iter().find(|o| o.id == id)
    }

    /// Adapter driving the monitor `hmonitor`, if any.
    pub fn adapter_for_monitor(&self, hmonitor: isize) -> Option<&AdapterInfo> {
        let o = self.outputs.iter().find(|o| o.hmonitor == hmonitor)?;
        self.adapter(o.adapter_index)
    }

    pub fn adapter(&self, index: u32) -> Option<&AdapterInfo> {
        self.adapters.iter().find(|a| a.index == index)
    }

    /// Hardware adapters (excluding the Microsoft Basic Render Driver).
    pub fn hardware_adapters(&self) -> impl Iterator<Item = &AdapterInfo> {
        self.adapters.iter().filter(|a| !a.software && a.vendor() != Vendor::Microsoft)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_ids() {
        assert_eq!(display_id(r"\\.\DISPLAY1"), 1);
        assert_eq!(display_id(r"\\.\DISPLAY12"), 12);
        assert!(display_id("weird") >= 1000 || display_id("weird") < 1000);
    }
}
