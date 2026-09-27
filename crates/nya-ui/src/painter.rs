//! Direct3D 11 renderer for egui meshes. Draws into any render target view,
//! so it can overlay a video frame (client) or fill a plain window (server GUI).

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use egui::epaint::{ImageDelta, Primitive, Vertex};
use egui::{ClippedPrimitive, ImageData, TextureId, TexturesDelta};
use windows::core::s;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::{D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D11_SRV_DIMENSION_TEXTURE2D};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use nya_win::d3d::{compile_shader, D3dDevice};

const HLSL: &str = include_str!("shaders/egui.hlsl");

struct Tex {
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    size: [usize; 2],
}

struct DynBuf {
    buf: ID3D11Buffer,
    cap: usize,
}

pub struct Painter {
    dev: D3dDevice,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    cbuf: ID3D11Buffer,
    vb: Option<DynBuf>,
    ib: Option<DynBuf>,
    textures: HashMap<TextureId, Tex>,
}

impl Painter {
    pub fn new(dev: &D3dDevice) -> Result<Self> {
        let d = &dev.device;
        let vs_code = compile_shader(HLSL, "vs_main", "vs_4_0")?;
        let ps_code = compile_shader(HLSL, "ps_main", "ps_4_0")?;
        let (mut vs, mut ps, mut layout) = (None, None, None);
        let elems = [
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("POSITION"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("TEXCOORD"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 8,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D11_INPUT_ELEMENT_DESC {
                SemanticName: s!("COLOR"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                InputSlot: 0,
                AlignedByteOffset: 16,
                InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];
        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut bd = D3D11_BLEND_DESC::default();
        bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            // egui colours are premultiplied.
            SrcBlend: D3D11_BLEND_ONE,
            DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: D3D11_BLEND_INV_DEST_ALPHA,
            DestBlendAlpha: D3D11_BLEND_ONE,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let rd = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            ScissorEnable: true.into(),
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        let cd = D3D11_BUFFER_DESC {
            ByteWidth: 16,
            Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
            ..Default::default()
        };
        let (mut sampler, mut blend, mut raster, mut cbuf) = (None, None, None, None);
        unsafe {
            d.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
            d.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
            d.CreateInputLayout(&elems, &vs_code, Some(&mut layout))?;
            d.CreateSamplerState(&sd, Some(&mut sampler))?;
            d.CreateBlendState(&bd, Some(&mut blend))?;
            d.CreateRasterizerState(&rd, Some(&mut raster))?;
            d.CreateBuffer(&cd, None, Some(&mut cbuf))?;
        }
        Ok(Self {
            dev: dev.clone(),
            vs: vs.unwrap(),
            ps: ps.unwrap(),
            layout: layout.unwrap(),
            sampler: sampler.unwrap(),
            blend: blend.unwrap(),
            raster: raster.unwrap(),
            cbuf: cbuf.unwrap(),
            vb: None,
            ib: None,
            textures: HashMap::new(),
        })
    }

    fn pixels(image: &ImageData) -> (Vec<u8>, [usize; 2]) {
        match image {
            ImageData::Color(img) => (img.pixels.iter().flat_map(|c| c.to_array()).collect(), img.size),
            ImageData::Font(img) => (img.srgba_pixels(None).flat_map(|c| c.to_array()).collect(), img.size),
        }
    }

    fn set_texture(&mut self, id: TextureId, delta: &ImageDelta) -> Result<()> {
        let (rgba, size) = Self::pixels(&delta.image);
        let d = &self.dev.device;
        match delta.pos {
            None => {
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: size[0] as u32,
                    Height: size[1] as u32,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let init = D3D11_SUBRESOURCE_DATA {
                    pSysMem: rgba.as_ptr() as *const _,
                    SysMemPitch: (size[0] * 4) as u32,
                    SysMemSlicePitch: 0,
                };
                let (mut tex, mut srv) = (None, None);
                let sd = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
                    },
                };
                unsafe {
                    d.CreateTexture2D(&desc, Some(&init), Some(&mut tex))?;
                    let tex_ref = tex.as_ref().ok_or_else(|| anyhow!("texture"))?;
                    d.CreateShaderResourceView(tex_ref, Some(&sd), Some(&mut srv))?;
                }
                self.textures.insert(id, Tex { tex: tex.unwrap(), srv: srv.unwrap(), size });
            }
            Some([x, y]) => {
                let Some(t) = self.textures.get(&id) else { return Ok(()) };
                if x + size[0] > t.size[0] || y + size[1] > t.size[1] {
                    return Ok(());
                }
                let bx = D3D11_BOX {
                    left: x as u32,
                    top: y as u32,
                    front: 0,
                    right: (x + size[0]) as u32,
                    bottom: (y + size[1]) as u32,
                    back: 1,
                };
                unsafe {
                    self.dev.context.UpdateSubresource(
                        &t.tex,
                        0,
                        Some(&bx),
                        rgba.as_ptr() as *const _,
                        (size[0] * 4) as u32,
                        0,
                    );
                }
            }
        }
        Ok(())
    }

    fn ensure_buffer(dev: &D3dDevice, slot: &mut Option<DynBuf>, bytes: usize, bind: D3D11_BIND_FLAG) -> Result<ID3D11Buffer> {
        if slot.as_ref().is_none_or(|b| b.cap < bytes) {
            let cap = bytes.next_power_of_two().max(64 * 1024);
            let desc = D3D11_BUFFER_DESC {
                ByteWidth: cap as u32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: bind.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            };
            let mut buf = None;
            unsafe { dev.device.CreateBuffer(&desc, None, Some(&mut buf))? };
            *slot = Some(DynBuf { buf: buf.unwrap(), cap });
        }
        Ok(slot.as_ref().unwrap().buf.clone())
    }

    fn upload(&self, buf: &ID3D11Buffer, data: &[u8]) -> Result<()> {
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.dev.context.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), m.pData as *mut u8, data.len());
            self.dev.context.Unmap(buf, 0);
        }
        Ok(())
    }

    /// Paint `primitives` into `rtv` (`size` in pixels). Applies and frees
    /// textures from `delta`.
    pub fn paint(
        &mut self,
        rtv: &ID3D11RenderTargetView,
        size: (u32, u32),
        pixels_per_point: f32,
        primitives: &[ClippedPrimitive],
        delta: &TexturesDelta,
    ) -> Result<()> {
        for (id, d) in &delta.set {
            self.set_texture(*id, d)?;
        }
        let ctx = self.dev.context.clone();
        let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);
        let ppp = pixels_per_point.max(0.1);
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&self.cbuf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
            *(m.pData as *mut [f32; 4]) = [w / ppp, h / ppp, 0.0, 0.0];
            ctx.Unmap(&self.cbuf, 0);

            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: w,
                Height: h,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            ctx.RSSetState(&self.raster);
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cbuf.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
        }

        for p in primitives {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            if mesh.indices.is_empty() {
                continue;
            }
            let Some(tex) = self.textures.get(&mesh.texture_id) else { continue };
            let srv = tex.srv.clone();
            let c = p.clip_rect;
            let rect = RECT {
                left: (c.min.x * ppp).floor().clamp(0.0, w) as i32,
                top: (c.min.y * ppp).floor().clamp(0.0, h) as i32,
                right: (c.max.x * ppp).ceil().clamp(0.0, w) as i32,
                bottom: (c.max.y * ppp).ceil().clamp(0.0, h) as i32,
            };
            if rect.right <= rect.left || rect.bottom <= rect.top {
                continue;
            }
            let vbytes = std::mem::size_of_val(mesh.vertices.as_slice());
            let ibytes = std::mem::size_of_val(mesh.indices.as_slice());
            let vb = Self::ensure_buffer(&self.dev, &mut self.vb, vbytes, D3D11_BIND_VERTEX_BUFFER)?;
            let ib = Self::ensure_buffer(&self.dev, &mut self.ib, ibytes, D3D11_BIND_INDEX_BUFFER)?;
            // SAFETY: Vertex is repr(C) plain data; u32 indices likewise.
            let vdata = unsafe { std::slice::from_raw_parts(mesh.vertices.as_ptr() as *const u8, vbytes) };
            let idata = unsafe { std::slice::from_raw_parts(mesh.indices.as_ptr() as *const u8, ibytes) };
            self.upload(&vb, vdata)?;
            self.upload(&ib, idata)?;
            let stride = std::mem::size_of::<Vertex>() as u32;
            let offset = 0u32;
            unsafe {
                ctx.IASetVertexBuffers(0, 1, Some(&Some(vb)), Some(&stride), Some(&offset));
                ctx.IASetIndexBuffer(&ib, DXGI_FORMAT_R32_UINT, 0);
                ctx.RSSetScissorRects(Some(&[rect]));
                ctx.PSSetShaderResources(0, Some(&[Some(srv)]));
                ctx.DrawIndexed(mesh.indices.len() as u32, 0, 0);
            }
        }

        unsafe {
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.RSSetState(None);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.IASetInputLayout(None);
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaders_compile() {
        compile_shader(HLSL, "vs_main", "vs_4_0").unwrap();
        compile_shader(HLSL, "ps_main", "ps_4_0").unwrap();
    }

    #[test]
    fn vertex_layout_matches_input_layout() {
        assert_eq!(std::mem::size_of::<Vertex>(), 20);
    }
}
