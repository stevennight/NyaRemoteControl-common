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

const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;

/// Process id of the clipboard's owner (who copied last), if any.
pub fn owner_process() -> Option<u32> {
    use windows::Win32::System::DataExchange::GetClipboardOwner;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let owner = unsafe { GetClipboardOwner() }.ok().filter(|h| !h.is_invalid())?;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(owner, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

pub fn has_files() -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(CF_HDROP).is_ok() }
}

pub fn has_image() -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(CF_DIB).is_ok() }
}

pub fn has_text() -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() }
}

/// Paths of files copied in Explorer (CF_HDROP).
pub fn get_files() -> Result<Option<Vec<std::path::PathBuf>>> {
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    let _open = Open::new()?;
    let h = match unsafe { GetClipboardData(CF_HDROP) } {
        Ok(h) if !h.is_invalid() => h,
        _ => return Ok(None),
    };
    let drop = HDROP(h.0);
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    let mut out = Vec::new();
    for i in 0..count {
        let len = unsafe { DragQueryFileW(drop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        let n = unsafe { DragQueryFileW(drop, i, Some(&mut buf)) } as usize;
        out.push(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])));
    }
    Ok(Some(out))
}

/// Put files on the clipboard so Explorer can paste them.
pub fn set_files(paths: &[std::path::PathBuf]) -> Result<()> {
    let g = crate::clipboard_files::dropfiles(paths)?;
    let _open = Open::new()?;
    unsafe {
        EmptyClipboard()?;
        SetClipboardData(CF_HDROP, HANDLE(g.0))?;
    }
    Ok(())
}

/// Clipboard image as CF_DIB bytes (BITMAPINFOHEADER + pixels).
pub fn get_dib() -> Result<Option<Vec<u8>>> {
    use windows::Win32::System::Memory::GlobalSize;
    let _open = Open::new()?;
    let h = match unsafe { GetClipboardData(CF_DIB) } {
        Ok(h) if !h.is_invalid() => h,
        _ => return Ok(None),
    };
    let g = HGLOBAL(h.0);
    unsafe {
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() || size == 0 {
            return Ok(None);
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(g);
        Ok(Some(v))
    }
}

pub fn set_dib(dib: &[u8]) -> Result<()> {
    let _open = Open::new()?;
    unsafe {
        EmptyClipboard()?;
        let g = GlobalAlloc(GMEM_MOVEABLE, dib.len())?;
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(dib.as_ptr(), p, dib.len());
        let _ = GlobalUnlock(g);
        SetClipboardData(CF_DIB, HANDLE(g.0))?;
    }
    Ok(())
}
