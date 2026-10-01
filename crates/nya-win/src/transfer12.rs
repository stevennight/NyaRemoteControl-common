//! Moving textures between GPUs, T2 (design doc §3.5): a D3D12 heap shared
//! across adapters. The GPUs copy over PCIe themselves; the CPU only waits.
//!
//! Per frame:
//! 1. source D3D11: copy the frame into a texture shared with D3D12 (same GPU);
//! 2. source D3D12 (copy queue): texture planes → buffer in the cross-adapter
//!    heap, then signal a fence shared with the other GPU;
//! 3. destination D3D12: wait for that fence, buffer → texture shared with
//!    the destination's D3D11, signal a local fence the CPU waits on;
//! 4. destination D3D11: copy into the encoder's surface.
//!
//! [`GpuToGpu`] uses this when it can be set up and falls back to the T1
//! copy through system memory ([`crate::transfer::CrossGpuCopy`]) otherwise,
//! also for good when T2 fails while running.

use std::mem::ManuallyDrop;

use anyhow::{anyhow, Context, Result};
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

use crate::d3d::{tex_desc, D3dDevice};
use crate::transfer::CrossGpuCopy;

const GENERIC_ALL: u32 = 0x1000_0000;

/// One GPU's side: its D3D12 device, a copy queue and a command list.
struct Side {
    dev: ID3D12Device,
    queue: ID3D12CommandQueue,
    alloc: ID3D12CommandAllocator,
    list: ID3D12GraphicsCommandList,
}

impl Side {
    fn new(d3d11: &D3dDevice) -> Result<Self> {
        unsafe {
            let adapter = d3d11.device.cast::<IDXGIDevice>()?.GetAdapter()?;
            let mut dev: Option<ID3D12Device> = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut dev).context("D3D12CreateDevice")?;
            let dev = dev.ok_or_else(|| anyhow!("no D3D12 device"))?;
            let queue: ID3D12CommandQueue =
                dev.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_COPY, ..Default::default() })?;
            let alloc: ID3D12CommandAllocator = dev.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
            let list: ID3D12GraphicsCommandList = dev.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_COPY, &alloc, None)?;
            list.Close()?;
            Ok(Self { dev, queue, alloc, list })
        }
    }

    /// A D3D11 texture on this GPU, opened in D3D12 as well.
    fn shared_texture(&self, d3d11: &D3dDevice, format: DXGI_FORMAT, w: u32, h: u32) -> Result<(ID3D11Texture2D, ID3D12Resource)> {
        let mut desc = tex_desc(w, h, format, D3D11_BIND_SHADER_RESOURCE);
        desc.MiscFlags = (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32;
        let tex = d3d11.texture(&desc).context("shared D3D11 texture")?;
        unsafe {
            let h = tex.cast::<IDXGIResource1>()?.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, None)?;
            let mut res: Option<ID3D12Resource> = None;
            let r = self.dev.OpenSharedHandle(h, &mut res);
            let _ = CloseHandle(h);
            r.context("open the shared texture in D3D12")?;
            Ok((tex, res.ok_or_else(|| anyhow!("no D3D12 resource"))?))
        }
    }

    fn run(&self, record: impl FnOnce(&ID3D12GraphicsCommandList)) -> Result<()> {
        unsafe {
            self.alloc.Reset()?;
            self.list.Reset(&self.alloc, None)?;
            record(&self.list);
            self.list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(self.list.cast()?)]);
        }
        Ok(())
    }
}

fn wait_fence(fence: &ID3D12Fence, value: u64, event: HANDLE) -> Result<()> {
    unsafe {
        if fence.GetCompletedValue() < value {
            fence.SetEventOnCompletion(value, event)?;
            WaitForSingleObject(event, INFINITE);
        }
    }
    Ok(())
}

fn location_texture(res: &ID3D12Resource, sub: u32) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(res.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: sub },
    }
}

fn location_buffer(res: &ID3D12Resource, fp: D3D12_PLACED_SUBRESOURCE_FOOTPRINT) -> D3D12_TEXTURE_COPY_LOCATION {
    D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(res.clone())),
        Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
        Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp },
    }
}

fn release(mut l: D3D12_TEXTURE_COPY_LOCATION) {
    unsafe { ManuallyDrop::drop(&mut l.pResource) };
}

fn planes(format: DXGI_FORMAT) -> u32 {
    if format == DXGI_FORMAT_NV12 || format == DXGI_FORMAT_P010 {
        2
    } else {
        1
    }
}

/// T2: GPU-to-GPU copy through a cross-adapter D3D12 heap.
pub struct CrossAdapterCopy {
    src11: D3dDevice,
    dst11: D3dDevice,
    src: Side,
    dst: Side,
    src_tex11: ID3D11Texture2D,
    src_tex12: ID3D12Resource,
    dst_tex11: ID3D11Texture2D,
    dst_tex12: ID3D12Resource,
    src_buf: ID3D12Resource,
    dst_buf: ID3D12Resource,
    footprints: Vec<D3D12_PLACED_SUBRESOURCE_FOOTPRINT>,
    /// Shared across the adapters: source signals, destination waits.
    fence_src: ID3D12Fence,
    fence_dst: ID3D12Fence,
    /// Destination-local: the CPU waits for the copy into the texture.
    done: ID3D12Fence,
    event: HANDLE,
    value: u64,
}

// SAFETY: used from the one pipeline thread at a time, like the D3D11 devices it wraps.
unsafe impl Send for CrossAdapterCopy {}

impl Drop for CrossAdapterCopy {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.event);
        }
    }
}

impl CrossAdapterCopy {
    pub fn new(src11: &D3dDevice, dst11: &D3dDevice, format: DXGI_FORMAT, w: u32, h: u32) -> Result<Self> {
        let src = Side::new(src11).context("source GPU")?;
        let dst = Side::new(dst11).context("destination GPU")?;
        let (src_tex11, src_tex12) = src.shared_texture(src11, format, w, h)?;
        let (dst_tex11, dst_tex12) = dst.shared_texture(dst11, format, w, h)?;
        unsafe {
            // Plane layouts inside the buffer.
            let n = planes(format);
            let tdesc = src_tex12.GetDesc();
            let mut footprints = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); n as usize];
            let mut total = 0u64;
            src.dev.GetCopyableFootprints(&tdesc, 0, n, 0, Some(footprints.as_mut_ptr()), None, None, Some(&mut total));
            if total == 0 {
                return Err(anyhow!("no copyable footprint for {format:?}"));
            }
            let size = total.div_ceil(65536) * 65536;

            // The heap: created on the source GPU, opened on the destination.
            let heap_desc = D3D12_HEAP_DESC {
                SizeInBytes: size,
                Properties: D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() },
                Alignment: 0,
                Flags: D3D12_HEAP_FLAG_SHARED | D3D12_HEAP_FLAG_SHARED_CROSS_ADAPTER,
            };
            let mut heap: Option<ID3D12Heap> = None;
            src.dev.CreateHeap(&heap_desc, &mut heap).context("cross-adapter heap")?;
            let heap = heap.ok_or_else(|| anyhow!("no heap"))?;
            let hh = src.dev.CreateSharedHandle(&heap, None, GENERIC_ALL, None)?;
            let mut dst_heap: Option<ID3D12Heap> = None;
            let r = dst.dev.OpenSharedHandle(hh, &mut dst_heap);
            let _ = CloseHandle(hh);
            r.context("open the cross-adapter heap on the other GPU")?;
            let dst_heap = dst_heap.ok_or_else(|| anyhow!("no heap"))?;

            let buf_desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_UNKNOWN,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                Flags: D3D12_RESOURCE_FLAG_ALLOW_CROSS_ADAPTER,
                ..Default::default()
            };
            let mut src_buf: Option<ID3D12Resource> = None;
            src.dev.CreatePlacedResource(&heap, 0, &buf_desc, D3D12_RESOURCE_STATE_COMMON, None, &mut src_buf)?;
            let mut dst_buf: Option<ID3D12Resource> = None;
            dst.dev.CreatePlacedResource(&dst_heap, 0, &buf_desc, D3D12_RESOURCE_STATE_COMMON, None, &mut dst_buf)?;

            let fence_src: ID3D12Fence = src.dev.CreateFence(0, D3D12_FENCE_FLAG_SHARED | D3D12_FENCE_FLAG_SHARED_CROSS_ADAPTER)?;
            let fh = src.dev.CreateSharedHandle(&fence_src, None, GENERIC_ALL, None)?;
            let mut fence_dst: Option<ID3D12Fence> = None;
            let r = dst.dev.OpenSharedHandle(fh, &mut fence_dst);
            let _ = CloseHandle(fh);
            r.context("open the shared fence on the other GPU")?;
            let done: ID3D12Fence = dst.dev.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventW(None, false, false, None)?;

            Ok(Self {
                src11: src11.clone(),
                dst11: dst11.clone(),
                src,
                dst,
                src_tex11,
                src_tex12,
                dst_tex11,
                dst_tex12,
                src_buf: src_buf.ok_or_else(|| anyhow!("no source buffer"))?,
                dst_buf: dst_buf.ok_or_else(|| anyhow!("no destination buffer"))?,
                footprints,
                fence_src,
                fence_dst: fence_dst.ok_or_else(|| anyhow!("no fence"))?,
                done,
                event,
                value: 0,
            })
        }
    }

    /// Copy `src_tex` (single-slice, source GPU) into `dst_tex[dst_slice]` (destination GPU).
    pub fn copy(&mut self, src_tex: &ID3D11Texture2D, dst_tex: &ID3D11Texture2D, dst_slice: u32) -> Result<()> {
        self.value += 1;
        let v = self.value;
        // 1. Into the shared texture, finished before D3D12 reads it.
        unsafe { self.src11.context.CopyResource(&self.src_tex11, src_tex) };
        self.src11.flush_wait()?;
        // 2. Texture planes -> cross-adapter buffer.
        let (tex, buf, fps) = (&self.src_tex12, &self.src_buf, &self.footprints);
        self.src.run(|l| unsafe {
            for (i, fp) in fps.iter().enumerate() {
                let (d, s) = (location_buffer(buf, *fp), location_texture(tex, i as u32));
                l.CopyTextureRegion(&d, 0, 0, 0, &s, None);
                release(d);
                release(s);
            }
        })?;
        unsafe { self.src.queue.Signal(&self.fence_src, v)? };
        // 3. On the destination, once the source is done: buffer -> texture.
        unsafe { self.dst.queue.Wait(&self.fence_dst, v)? };
        let (tex, buf) = (&self.dst_tex12, &self.dst_buf);
        self.dst.run(|l| unsafe {
            for (i, fp) in fps.iter().enumerate() {
                let (d, s) = (location_texture(tex, i as u32), location_buffer(buf, *fp));
                l.CopyTextureRegion(&d, 0, 0, 0, &s, None);
                release(d);
                release(s);
            }
        })?;
        unsafe { self.dst.queue.Signal(&self.done, v)? };
        wait_fence(&self.done, v, self.event)?;
        // 4. Into the encoder's surface.
        unsafe { self.dst11.context.CopySubresourceRegion(dst_tex, dst_slice, 0, 0, 0, &self.dst_tex11, 0, None) };
        self.dst11.flush_wait()
    }
}

/// GPU-to-GPU copy: T2 when the GPUs can share a heap, else T1.
pub enum GpuToGpu {
    T2(CrossAdapterCopy, (D3dDevice, D3dDevice, DXGI_FORMAT, u32, u32)),
    T1(CrossGpuCopy),
}

impl GpuToGpu {
    pub fn new(src: &D3dDevice, dst: &D3dDevice, format: DXGI_FORMAT, w: u32, h: u32) -> Result<Self> {
        match CrossAdapterCopy::new(src, dst, format, w, h) {
            Ok(c) => {
                tracing::info!("cross-GPU copy: D3D12 cross-adapter heap (T2)");
                Ok(Self::T2(c, (src.clone(), dst.clone(), format, w, h)))
            }
            Err(e) => {
                tracing::info!("cross-GPU copy through system memory (T1); T2 unavailable: {e:#}");
                Ok(Self::T1(CrossGpuCopy::new(src, dst, format, w, h)?))
            }
        }
    }

    /// Only the T1 path (for comparing in diagnostics).
    pub fn t1(src: &D3dDevice, dst: &D3dDevice, format: DXGI_FORMAT, w: u32, h: u32) -> Result<Self> {
        Ok(Self::T1(CrossGpuCopy::new(src, dst, format, w, h)?))
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::T2(..) => "T2",
            Self::T1(_) => "T1",
        }
    }

    pub fn copy(&mut self, src_tex: &ID3D11Texture2D, dst_tex: &ID3D11Texture2D, dst_slice: u32) -> Result<()> {
        if let Self::T2(c, (s, d, f, w, h)) = self {
            match c.copy(src_tex, dst_tex, dst_slice) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!("T2 cross-GPU copy failed ({e:#}); using T1 from now on");
                    *self = Self::T1(CrossGpuCopy::new(&s.clone(), &d.clone(), *f, *w, *h)?);
                }
            }
        }
        match self {
            Self::T1(c) => c.copy(src_tex, dst_tex, dst_slice),
            Self::T2(..) => unreachable!(),
        }
    }
}
