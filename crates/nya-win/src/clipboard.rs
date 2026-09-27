//! Unicode text clipboard access. Change detection uses the clipboard
//! sequence number, so no window is required.

use anyhow::{bail, Result};
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

struct Open;

impl Open {
    fn new() -> Result<Self> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(HWND::default()) }.is_ok() {
                return Ok(Open);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        bail!("clipboard busy")
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

pub fn get_text() -> Result<Option<String>> {
    let _open = Open::new()?;
    let h = match unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) } {
        Ok(h) if !h.is_invalid() => h,
        _ => return Ok(None),
    };
    let g = HGLOBAL(h.0);
    let p = unsafe { GlobalLock(g) } as *const u16;
    if p.is_null() {
        return Ok(None);
    }
    let mut len = 0;
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
    unsafe {
        let _ = GlobalUnlock(g);
    }
    Ok(Some(s))
}

pub fn set_text(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _open = Open::new()?;
    unsafe {
        EmptyClipboard()?;
        let g = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
        let p = GlobalLock(g) as *mut u16;
        if p.is_null() {
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        let _ = GlobalUnlock(g);
        // Ownership passes to the clipboard on success.
        SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(g.0))?;
    }
    Ok(())
}
