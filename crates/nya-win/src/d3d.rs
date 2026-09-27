//! D3D11 device creation (always on an explicit adapter) and shader helpers.

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::Fxc::{D3DCompile, D3DCOMPILE_OPTIMIZATION_LEVEL3};
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ERROR_NOT_FOUND};

pub fn luid_to_u64(l: LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

/// A D3D11 device and its immediate context, bound to one adapter.
/// Multithread protection is enabled because FFmpeg may touch the context.
#[derive(Clone)]
pub struct D3dDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub adapter: IDXGIAdapter1,
    pub luid: u64,
}

impl D3dDevice {
    pub fn for_adapter(adapter: &IDXGIAdapter1) -> Result<Self> {
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1];
        let try_create = |flags: D3D11_CREATE_DEVICE_FLAG| -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
            let mut dev = None;
            let mut ctx = None;
            unsafe {
                D3D11CreateDevice(
                    adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    flags,
                    Some(&levels),
                    D3D11_SDK_VERSION,
                    Some(&mut dev),
                    None,
                    Some(&mut ctx),
                )?;
            }
            Ok((dev.unwrap(), ctx.unwrap()))
        };
        // Video support is needed by FFmpeg's d3d11va; the basic render driver lacks it.
        let (device, context) = try_create(D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT)
            .or_else(|_| try_create(D3D11_CREATE_DEVICE_BGRA_SUPPORT))
            .context("D3D11CreateDevice")?;
        let mt: ID3D11Multithread = device.cast()?;
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
        let desc = unsafe { adapter.GetDesc1()? };
        Ok(Self { device, context, adapter: adapter.clone(), luid: luid_to_u64(desc.AdapterLuid) })
    }

    /// Device on the adapter with the given LUID.
    pub fn for_luid(luid: u64) -> Result<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let mut i = 0;
        loop {
            match unsafe { factory.EnumAdapters1(i) } {
                Ok(a) => {
                    let desc = unsafe { a.GetDesc1()? };
                    if luid_to_u64(desc.AdapterLuid) == luid {
                        return Self::for_adapter(&a);
                    }
                }
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            }
            i += 1;
        }
        bail!("adapter with LUID {luid:#x} not found")
    }

    /// Device on the first (system preferred) adapter.
    pub fn default_adapter() -> Result<Self> {
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
        let a = unsafe { factory.EnumAdapters1(0)? };
        Self::for_adapter(&a)
    }

    /// AddRef'd raw `ID3D11Device*` for handing ownership of one reference to FFmpeg.
    pub fn device_raw_owned(&self) -> *mut std::ffi::c_void {
        self.device.clone().into_raw()
    }

    pub fn texture(&self, desc: &D3D11_TEXTURE2D_DESC) -> Result<ID3D11Texture2D> {
        let mut t = None;
        unsafe { self.device.CreateTexture2D(desc, None, Some(&mut t))? };
        t.ok_or_else(|| anyhow!("CreateTexture2D returned null"))
    }

    pub fn srv(&self, tex: &ID3D11Texture2D) -> Result<ID3D11ShaderResourceView> {
        let mut v = None;
        unsafe { self.device.CreateShaderResourceView(tex, None, Some(&mut v))? };
        v.ok_or_else(|| anyhow!("CreateShaderResourceView returned null"))
    }
}

pub fn tex_desc(width: u32, height: u32, format: DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    }
}

pub fn staging_desc(width: u32, height: u32, format: DXGI_FORMAT, cpu: D3D11_CPU_ACCESS_FLAG) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: cpu.0 as u32,
        ..tex_desc(width, height, format, D3D11_BIND_FLAG(0))
    }
}

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) }
}

/// Compile HLSL at runtime (d3dcompiler_47.dll ships with Windows 10+).
pub fn compile_shader(src: &str, entry: &str, target: &str) -> Result<Vec<u8>> {
    let entry_c = std::ffi::CString::new(entry)?;
    let target_c = std::ffi::CString::new(target)?;
    let mut code = None;
    let mut errors = None;
    let r = unsafe {
        D3DCompile(
            src.as_ptr() as *const _,
            src.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr() as *const u8),
            PCSTR(target_c.as_ptr() as *const u8),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .as_ref()
            .map(|b| String::from_utf8_lossy(blob_bytes(b)).into_owned())
            .unwrap_or_default();
        bail!("compile {entry}: {e}: {msg}");
    }
    Ok(blob_bytes(code.as_ref().unwrap()).to_vec())
}
