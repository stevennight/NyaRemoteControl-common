//! GPU colour conversion: BGRA desktop texture → encoder input texture
//! (NV12 / AYUV / BGRA), optionally scaled. Renders into array slices, so it
//! can target FFmpeg's hardware frame pools directly.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_AYUV, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8_UNORM, DXGI_FORMAT_R8_UNORM,
};

use crate::d3d::{compile_shader, D3dDevice};

const HLSL: &str = include_str!("shaders/convert.hlsl");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetFormat {
    Nv12,
    Ayuv,
    Bgra,
}

impl TargetFormat {
    pub fn dxgi(self) -> DXGI_FORMAT {
        match self {
            Self::Nv12 => DXGI_FORMAT_NV12,
            Self::Ayuv => DXGI_FORMAT_AYUV,
            Self::Bgra => DXGI_FORMAT_B8G8R8A8_UNORM,
        }
    }
}

pub struct Converter {
    dev: D3dDevice,
    vs: ID3D11VertexShader,
    ps_y: ID3D11PixelShader,
    ps_uv: ID3D11PixelShader,
    ps_ayuv: ID3D11PixelShader,
    ps_copy: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    cbuf: ID3D11Buffer,
    rtvs: HashMap<(usize, u32, u32), ID3D11RenderTargetView>,
}

impl Converter {
    pub fn new(dev: &D3dDevice) -> Result<Self> {
        let d = &dev.device;
        let vs_code = compile_shader(HLSL, "vs_main", "vs_4_0")?;
        let mut vs = None;
        unsafe { d.CreateVertexShader(&vs_code, None, Some(&mut vs))? };
        let ps = |entry: &str| -> Result<ID3D11PixelShader> {
            let code = compile_shader(HLSL, entry, "ps_4_0")?;
            let mut p = None;
            unsafe { d.CreatePixelShader(&code, None, Some(&mut p))? };
            p.ok_or_else(|| anyhow!("CreatePixelShader {entry}"))
        };
        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler = None;
        unsafe { d.CreateSamplerState(&sd, Some(&mut sampler))? };
        let rect: [f32; 4] = [0.0, 0.0, 1.0, 1.0];
        let bd = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: rect.as_ptr() as *const _, ..Default::default() };
        let mut cbuf = None;
        unsafe { d.CreateBuffer(&bd, Some(&init), Some(&mut cbuf))? };
        Ok(Self {
            dev: dev.clone(),
            vs: vs.unwrap(),
            ps_y: ps("ps_y")?,
            ps_uv: ps("ps_uv")?,
            ps_ayuv: ps("ps_ayuv")?,
            ps_copy: ps("ps_copy")?,
            sampler: sampler.unwrap(),
            cbuf: cbuf.unwrap(),
            rtvs: HashMap::new(),
        })
    }

    fn rtv(&mut self, tex: &ID3D11Texture2D, slice: u32, view_fmt: DXGI_FORMAT) -> Result<ID3D11RenderTargetView> {
        let key = (tex.as_raw() as usize, slice, view_fmt.0 as u32);
        if let Some(v) = self.rtvs.get(&key) {
            return Ok(v.clone());
        }
        if self.rtvs.len() > 128 {
            self.rtvs.clear();
        }
        let desc = D3D11_RENDER_TARGET_VIEW_DESC {
            Format: view_fmt,
            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                Texture2DArray: D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: slice, ArraySize: 1 },
            },
        };
        let mut v = None;
        unsafe { self.dev.device.CreateRenderTargetView(tex, Some(&desc), Some(&mut v))? };
        let v = v.ok_or_else(|| anyhow!("CreateRenderTargetView"))?;
        self.rtvs.insert(key, v.clone());
        Ok(v)
    }

    /// Create the render target views for `dst[slice]` up front, so an
    /// unusable encoder surface is detected when the pipeline is built.
    pub fn prepare_target(&mut self, dst: &ID3D11Texture2D, slice: u32, target: TargetFormat) -> Result<()> {
        match target {
            TargetFormat::Nv12 => {
                self.rtv(dst, slice, DXGI_FORMAT_R8_UNORM)?;
                self.rtv(dst, slice, DXGI_FORMAT_R8G8_UNORM)?;
            }
            TargetFormat::Ayuv => {
                self.rtv(dst, slice, DXGI_FORMAT_R8G8B8A8_UNORM)?;
            }
            TargetFormat::Bgra => {
                self.rtv(dst, slice, DXGI_FORMAT_B8G8R8A8_UNORM)?;
            }
        }
        Ok(())
    }

    fn pass(&self, rtv: &ID3D11RenderTargetView, ps: &ID3D11PixelShader, w: u32, h: u32) {
        let ctx = &self.dev.context;
        let vp = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: w as f32, Height: h as f32, MinDepth: 0.0, MaxDepth: 1.0 };
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[vp]));
            ctx.PSSetShader(ps, None);
            ctx.Draw(3, 0);
        }
    }

    /// Convert/scale `src` into `dst[slice]` of size `w`×`h` (must be even for NV12).
    pub fn convert(
        &mut self,
        src: &ID3D11ShaderResourceView,
        dst: &ID3D11Texture2D,
        slice: u32,
        target: TargetFormat,
        w: u32,
        h: u32,
    ) -> Result<()> {
        let ctx = self.dev.context.clone();
        unsafe {
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.RSSetState(None);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.PSSetShaderResources(0, Some(&[Some(src.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            // The vertex shader reads src_rect: without this every pixel samples (0,0).
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
        }
        match target {
            TargetFormat::Nv12 => {
                let y = self.rtv(dst, slice, DXGI_FORMAT_R8_UNORM)?;
                let uv = self.rtv(dst, slice, DXGI_FORMAT_R8G8_UNORM)?;
                self.pass(&y, &self.ps_y, w, h);
                self.pass(&uv, &self.ps_uv, w / 2, h / 2);
            }
            TargetFormat::Ayuv => {
                let v = self.rtv(dst, slice, DXGI_FORMAT_R8G8B8A8_UNORM)?;
                self.pass(&v, &self.ps_ayuv, w, h);
            }
            TargetFormat::Bgra => {
                let v = self.rtv(dst, slice, DXGI_FORMAT_B8G8R8A8_UNORM)?;
                self.pass(&v, &self.ps_copy, w, h);
            }
        }
        unsafe {
            ctx.OMSetRenderTargets(None, None);
            ctx.PSSetShaderResources(0, Some(&[None]));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HLSL compilation runs on the CPU (d3dcompiler_47), no GPU involved.
    #[test]
    fn shaders_compile() {
        compile_shader(HLSL, "vs_main", "vs_4_0").unwrap();
        for ps in ["ps_y", "ps_uv", "ps_ayuv", "ps_copy"] {
            compile_shader(HLSL, ps, "ps_4_0").unwrap();
        }
    }
}
