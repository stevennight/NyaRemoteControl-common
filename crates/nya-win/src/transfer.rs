//! Moving textures between GPUs (design doc §3.5, T1: through system memory)
//! and reading them back for software encoding.

use anyhow::{bail, Result};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12};

use crate::d3d::{staging_desc, D3dDevice};

/// (bytes per row, rows) of each plane.
pub fn plane_layout(format: DXGI_FORMAT, w: u32, h: u32) -> Vec<(usize, usize)> {
    if format == DXGI_FORMAT_NV12 {
        vec![(w as usize, h as usize), (w as usize, (h / 2) as usize)]
    } else {
        // BGRA / AYUV: 4 bytes per pixel.
        vec![(w as usize * 4, h as usize)]
    }
}

struct Mapped<'a> {
    ctx: &'a ID3D11DeviceContext,
    res: &'a ID3D11Texture2D,
    m: D3D11_MAPPED_SUBRESOURCE,
}

impl<'a> Mapped<'a> {
    fn new(ctx: &'a ID3D11DeviceContext, res: &'a ID3D11Texture2D, kind: D3D11_MAP) -> Result<Self> {
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { ctx.Map(res, 0, kind, 0, Some(&mut m))? };
        if m.pData.is_null() {
            bail!("Map returned null");
        }
        Ok(Self { ctx, res, m })
    }

    /// Pointer to the start of plane `i` (planes follow each other at RowPitch * rows).
    fn plane(&self, layout: &[(usize, usize)], i: usize) -> *mut u8 {
        let rows_before: usize = layout[..i].iter().map(|p| p.1).sum();
        unsafe { (self.m.pData as *mut u8).add(rows_before * self.m.RowPitch as usize) }
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        unsafe { self.ctx.Unmap(self.res, 0) };
    }
}

/// Copies a texture from one GPU to another through CPU memory.
pub struct CrossGpuCopy {
    src: D3dDevice,
    dst: D3dDevice,
    src_stage: ID3D11Texture2D,
    dst_stage: ID3D11Texture2D,
    layout: Vec<(usize, usize)>,
}

impl CrossGpuCopy {
    pub fn new(src: &D3dDevice, dst: &D3dDevice, format: DXGI_FORMAT, w: u32, h: u32) -> Result<Self> {
        Ok(Self {
            src_stage: src.texture(&staging_desc(w, h, format, D3D11_CPU_ACCESS_READ))?,
            dst_stage: dst.texture(&staging_desc(w, h, format, D3D11_CPU_ACCESS_WRITE))?,
            src: src.clone(),
            dst: dst.clone(),
            layout: plane_layout(format, w, h),
        })
    }

    /// Copy `src_tex` (single-slice texture on the source GPU) into
    /// `dst_tex[dst_slice]` on the destination GPU.
    pub fn copy(&mut self, src_tex: &ID3D11Texture2D, dst_tex: &ID3D11Texture2D, dst_slice: u32) -> Result<()> {
        unsafe { self.src.context.CopyResource(&self.src_stage, src_tex) };
        {
            // Map(READ) waits for the GPU copy; no extra frame of pipelining.
            let s = Mapped::new(&self.src.context, &self.src_stage, D3D11_MAP_READ)?;
            let d = Mapped::new(&self.dst.context, &self.dst_stage, D3D11_MAP_WRITE)?;
            for (i, &(row_bytes, rows)) in self.layout.iter().enumerate() {
                let sp = s.plane(&self.layout, i);
                let dp = d.plane(&self.layout, i);
                for r in 0..rows {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            sp.add(r * s.m.RowPitch as usize),
                            dp.add(r * d.m.RowPitch as usize),
                            row_bytes,
                        );
                    }
                }
            }
        }
        unsafe {
            self.dst.context.CopySubresourceRegion(dst_tex, dst_slice, 0, 0, 0, &self.dst_stage, 0, None);
        }
        // The encoder reads dst_tex next; make sure the copy really happened.
        self.dst.flush_wait()
    }
}

/// Reads a texture back into tightly packed planes.
pub struct Readback {
    dev: D3dDevice,
    stage: ID3D11Texture2D,
    layout: Vec<(usize, usize)>,
}

impl Readback {
    pub fn new(dev: &D3dDevice, format: DXGI_FORMAT, w: u32, h: u32) -> Result<Self> {
        Ok(Self {
            stage: dev.texture(&staging_desc(w, h, format, D3D11_CPU_ACCESS_READ))?,
            dev: dev.clone(),
            layout: plane_layout(format, w, h),
        })
    }

    pub fn layout(&self) -> &[(usize, usize)] {
        &self.layout
    }

    /// Returns the planes concatenated without padding.
    pub fn read(&mut self, tex: &ID3D11Texture2D, out: &mut Vec<u8>) -> Result<()> {
        unsafe { self.dev.context.CopyResource(&self.stage, tex) };
        let m = Mapped::new(&self.dev.context, &self.stage, D3D11_MAP_READ)?;
        out.clear();
        for (i, &(row_bytes, rows)) in self.layout.iter().enumerate() {
            let p = m.plane(&self.layout, i);
            for r in 0..rows {
                let row = unsafe { std::slice::from_raw_parts(p.add(r * m.m.RowPitch as usize), row_bytes) };
                out.extend_from_slice(row);
            }
        }
        Ok(())
    }
}
