//! GDI screen capture: the fallback for an output whose Desktop Duplication
//! keeps losing access and hands back a stale image (seen on a physical
//! display that was switched off, likely behind a VGA adapter). Slower than
//! duplication (a CPU copy of the whole output per frame), so callers use it
//! only while duplication is broken.
//!
//! Also the pointer shape from a cursor handle, as GDI capture has no DXGI
//! pointer shapes.

use std::ffi::c_void;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dxgi::DXGI_OUTDUPL_POINTER_SHAPE_INFO;
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, SelectObject,
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, HCURSOR, HICON, ICONINFO};

use crate::duplication::{convert_pointer_shape, CursorShape};

/// Copies one screen rectangle (an output's desktop coordinates) into a
/// top-down BGRA buffer. The screen DC belongs to the calling thread's
/// desktop: re-create after the thread switched desktops.
pub struct GdiCapture {
    screen: HDC,
    mem: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *const u8,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

// The handles are used from the thread that owns the pipeline only; moving
// the pipeline between threads is fine as long as one thread uses it at a time.
unsafe impl Send for GdiCapture {}

impl GdiCapture {
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err(anyhow!("empty capture rectangle"));
        }
        unsafe {
            let screen = GetDC(HWND::default());
            if screen.is_invalid() {
                return Err(anyhow!("GetDC(screen) failed"));
            }
            let mem = CreateCompatibleDC(screen);
            if mem.is_invalid() {
                ReleaseDC(HWND::default(), screen);
                return Err(anyhow!("CreateCompatibleDC failed"));
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32), // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                r => {
                    if let Ok(b) = r {
                        let _ = DeleteObject(b);
                    }
                    let _ = DeleteDC(mem);
                    ReleaseDC(HWND::default(), screen);
                    return Err(anyhow!("CreateDIBSection {width}x{height} failed"));
                }
            };
            let old = SelectObject(mem, bitmap);
            Ok(Self { screen, mem, bitmap, old, bits: bits as *const u8, left, top, width, height })
        }
    }

    /// Copy the rectangle; returns the BGRA pixels (pitch `width * 4`). The
    /// alpha bytes are undefined.
    pub fn grab(&mut self) -> Result<&[u8]> {
        unsafe {
            BitBlt(
                self.mem,
                0,
                0,
                self.width as i32,
                self.height as i32,
                self.screen,
                self.left,
                self.top,
                SRCCOPY | CAPTUREBLT,
            )?;
            Ok(std::slice::from_raw_parts(self.bits, (self.width * self.height * 4) as usize))
        }
    }
}

impl Drop for GdiCapture {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.mem);
            ReleaseDC(HWND::default(), self.screen);
        }
    }
}

/// True when every sampled pixel is black (a protected or exclusive-fullscreen
/// surface reads back black through GDI).
pub fn looks_black(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).step_by(97).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
}

const SHAPE_MONOCHROME: u32 = 1;
const SHAPE_COLOR: u32 = 2;
const SHAPE_MASKED_COLOR: u32 = 4;

/// The pointer shape of a cursor handle (as `GetCursorInfo` returns it),
/// converted like DXGI's shapes.
pub fn cursor_shape(cursor: HCURSOR) -> Option<CursorShape> {
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(HICON(cursor.0), &mut ii) }.ok()?;
    let shape = unsafe { shape_from_icon(&ii) };
    unsafe {
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask);
        }
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor);
        }
    }
    shape
}

unsafe fn bitmap_size(b: HBITMAP) -> Option<(u32, u32)> {
    let mut bm = BITMAP::default();
    (GetObjectW(b, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut c_void)) != 0)
        .then(|| (bm.bmWidth.max(0) as u32, bm.bmHeight.max(0) as u32))
        .filter(|&(w, h)| w > 0 && h > 0 && w <= 512 && h <= 1024)
}

/// Rows of a bitmap as a top-down DIB with `bpp` bits per pixel (rows padded to 4 bytes).
unsafe fn dib_rows(b: HBITMAP, w: u32, h: u32, bpp: u16) -> Option<(Vec<u8>, usize)> {
    let pitch = (w as usize * bpp as usize).div_ceil(32) * 4;
    // Room for the 2-entry colour table of 1-bpp bitmaps.
    #[repr(C)]
    struct Info {
        header: BITMAPINFOHEADER,
        colors: [u32; 2],
    }
    let mut info = Info {
        header: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: bpp,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        colors: [0; 2],
    };
    let mut buf = vec![0u8; pitch * h as usize];
    let dc = GetDC(HWND::default());
    let lines = GetDIBits(dc, b, 0, h, Some(buf.as_mut_ptr() as *mut c_void), &mut info as *mut Info as *mut BITMAPINFO, DIB_RGB_COLORS);
    ReleaseDC(HWND::default(), dc);
    (lines == h as i32).then_some((buf, pitch))
}

unsafe fn shape_from_icon(ii: &ICONINFO) -> Option<CursorShape> {
    let (mw, mh) = bitmap_size(ii.hbmMask)?;
    let hot = windows::Win32::Foundation::POINT { x: ii.xHotspot as i32, y: ii.yHotspot as i32 };
    if ii.hbmColor.is_invalid() {
        // Monochrome: the mask bitmap holds the AND rows on top of the XOR rows,
        // which is DXGI's monochrome layout.
        let (buf, pitch) = dib_rows(ii.hbmMask, mw, mh, 1)?;
        let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO { Type: SHAPE_MONOCHROME, Width: mw, Height: mh, Pitch: pitch as u32, HotSpot: hot };
        return convert_pointer_shape(&info, &buf);
    }
    let (w, h) = bitmap_size(ii.hbmColor)?;
    let (mut color, pitch) = dib_rows(ii.hbmColor, w, h, 32)?;
    let has_alpha = color.chunks_exact(4).any(|p| p[3] != 0);
    if has_alpha {
        let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO { Type: SHAPE_COLOR, Width: w, Height: h, Pitch: pitch as u32, HotSpot: hot };
        return convert_pointer_shape(&info, &color);
    }
    // Colour without alpha plus an AND mask: DXGI's masked-colour layout puts the
    // mask in the alpha byte (0 = replace the screen, 0xFF = XOR with it).
    let (mask, mpitch) = dib_rows(ii.hbmMask, mw, mh, 1)?;
    for y in 0..h as usize {
        for x in 0..w as usize {
            let and = mask.get(y * mpitch + x / 8).is_some_and(|b| b & (0x80 >> (x % 8)) != 0);
            color[y * pitch + x * 4 + 3] = if and { 0xFF } else { 0 };
        }
    }
    let info = DXGI_OUTDUPL_POINTER_SHAPE_INFO { Type: SHAPE_MASKED_COLOR, Width: w, Height: h, Pitch: pitch as u32, HotSpot: hot };
    convert_pointer_shape(&info, &color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{LoadCursorW, IDC_ARROW, IDC_IBEAM};

    #[test]
    fn black_detection() {
        assert!(looks_black(&[0, 0, 0, 255].repeat(1000)));
        let mut img = [0u8, 0, 0, 0].repeat(1000);
        img[97 * 4 * 3] = 9;
        assert!(!looks_black(&img));
    }

    #[test]
    fn system_cursors_convert() {
        for id in [IDC_ARROW, IDC_IBEAM] {
            let c = unsafe { LoadCursorW(None, id) }.unwrap();
            let s = cursor_shape(c).expect("shape");
            assert!(s.width >= 16 && s.height >= 16, "{}x{}", s.width, s.height);
            assert!(s.rgba.chunks_exact(4).any(|p| p[3] != 0), "something visible");
        }
    }

    /// `cargo test -p nya-win --lib gdi_grab_speed -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gdi_grab_speed() {
        let (w, h) = unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
            (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32)
        };
        let mut g = GdiCapture::new(0, 0, w, h).unwrap();
        let t = std::time::Instant::now();
        for _ in 0..30 {
            g.grab().unwrap();
        }
        println!("{w}x{h}: {:.1} ms per grab", t.elapsed().as_secs_f64() * 1000.0 / 30.0);
    }

    #[test]
    fn capture_primary_corner() {
        // Needs an interactive desktop; skip quietly where there is none.
        let Ok(mut g) = GdiCapture::new(0, 0, 64, 32) else { return };
        if let Ok(px) = g.grab() {
            assert_eq!(px.len(), 64 * 32 * 4);
        }
    }
}
