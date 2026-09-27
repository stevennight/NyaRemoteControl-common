//! A plain flip-model swap chain for windows that only show UI.

use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D11::{ID3D11RenderTargetView, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use nya_win::d3d::D3dDevice;

pub struct Surface {
    pub dev: D3dDevice,
    swap: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    pub width: u32,
    pub height: u32,
}

impl Surface {
    pub fn new(dev: &D3dDevice, hwnd: HWND, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let dxgi: IDXGIDevice = dev.device.cast()?;
            let factory: IDXGIFactory2 = dxgi.GetAdapter()?.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width.max(1),
                Height: height.max(1),
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&dev.device, hwnd, &desc, None, None)?;
            let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
            Ok(Self { dev: dev.clone(), swap, rtv: None, width: width.max(1), height: height.max(1) })
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) == (self.width, self.height) {
            return Ok(());
        }
        self.rtv = None;
        unsafe {
            self.dev.context.OMSetRenderTargets(None, None);
            self.swap.ResizeBuffers(0, width, height, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
        }
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Back buffer view, cleared to `color`.
    pub fn begin(&mut self, color: [f32; 4]) -> Result<ID3D11RenderTargetView> {
        if self.rtv.is_none() {
            unsafe {
                let back: ID3D11Texture2D = self.swap.GetBuffer(0)?;
                let mut rtv = None;
                self.dev.device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
                self.rtv = Some(rtv.ok_or_else(|| anyhow!("CreateRenderTargetView"))?);
            }
        }
        let rtv = self.rtv.clone().unwrap();
        unsafe { self.dev.context.ClearRenderTargetView(&rtv, &color) };
        Ok(rtv)
    }

    pub fn present(&self) -> Result<()> {
        let hr = unsafe { self.swap.Present(1, DXGI_PRESENT(0)) };
        if hr.is_err() {
            return Err(anyhow!("Present: {hr:?}"));
        }
        Ok(())
    }
}
