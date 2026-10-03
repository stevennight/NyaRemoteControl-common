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

/// "explorer.exe (1234)" for the clipboard's owner, for logs.
pub fn owner_description() -> String {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    let Some(pid) = owner_process() else { return "-".into() };
    let name = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok().and_then(|p| {
            let mut buf = [0u16; 512];
            let mut n = buf.len() as u32;
            let r = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut n);
            let _ = CloseHandle(p);
            r.ok().map(|_| String::from_utf16_lossy(&buf[..n as usize]))
        })
    };
    match name {
        Some(full) => format!("{} ({pid})", full.rsplit('\\').next().unwrap_or(&full)),
        None => format!("pid {pid}"),
    }
}

/// The formats on the clipboard (registered names, or numbers), for logs.
pub fn format_names() -> String {
    use windows::Win32::System::DataExchange::{EnumClipboardFormats, GetClipboardFormatNameW};
    let Ok(_open) = Open::new() else { return "(clipboard busy)".into() };
    let mut out = Vec::new();
    let mut f = 0u32;
    while out.len() < 40 {
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        let mut name = [0u16; 128];
        let n = unsafe { GetClipboardFormatNameW(f, &mut name) };
        out.push(if n > 0 { String::from_utf16_lossy(&name[..n as usize]) } else { f.to_string() });
    }
    if out.is_empty() {
        format!("none ({})", std::io::Error::last_os_error())
    } else {
        out.join(", ")
    }
}

fn shell_idlist_format() -> u32 {
    unsafe { windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!("Shell IDList Array")) }
}

fn filename_format() -> u32 {
    unsafe { windows::Win32::System::DataExchange::RegisterClipboardFormatW(windows::core::w!("FileNameW")) }
}

/// Copied files: CF_HDROP, or only the shell's own formats (some programs
/// copy files as "Shell IDList Array" / "FileNameW" without CF_HDROP).
pub fn has_files() -> bool {
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
    unsafe {
        IsClipboardFormatAvailable(CF_HDROP).is_ok()
            || IsClipboardFormatAvailable(shell_idlist_format()).is_ok()
            || IsClipboardFormatAvailable(filename_format()).is_ok()
    }
}

/// The clipboard's "Shell IDList Array" as paths (diagnostics and tests).
#[doc(hidden)]
pub fn shell_idlist_files() -> Result<Vec<std::path::PathBuf>> {
    let _open = Open::new()?;
    match unsafe { GetClipboardData(shell_idlist_format()) } {
        Ok(h) if !h.is_invalid() => Ok(files_from_idlist(HGLOBAL(h.0))),
        _ => bail!("no Shell IDList Array"),
    }
}

/// File system paths of a "Shell IDList Array" (CIDA: count, offsets, the
/// folder's ID list, then each item's relative ID list).
fn files_from_idlist(g: HGLOBAL) -> Vec<std::path::PathBuf> {
    use windows::Win32::System::Memory::GlobalSize;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{ILCombine, ILFree, SHGetPathFromIDListW};
    let mut out = Vec::new();
    unsafe {
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() || size < 8 {
            return out;
        }
        let count = std::ptr::read_unaligned(p as *const u32) as usize;
        if 4 + (count + 1) * 4 > size {
            let _ = GlobalUnlock(g);
            return out;
        }
        let offset = |i: usize| std::ptr::read_unaligned(p.add(4 + i * 4) as *const u32) as usize;
        let folder = p.add(offset(0)) as *const ITEMIDLIST;
        for i in 1..=count {
            if offset(i) >= size {
                continue;
            }
            let full = ILCombine(Some(folder), Some(p.add(offset(i)) as *const ITEMIDLIST));
            if full.is_null() {
                continue;
            }
            let mut path = [0u16; 260];
            if SHGetPathFromIDListW(full, &mut path).as_bool() {
                let n = path.iter().position(|c| *c == 0).unwrap_or(path.len());
                out.push(std::path::PathBuf::from(String::from_utf16_lossy(&path[..n])));
            }
            ILFree(Some(full));
        }
        let _ = GlobalUnlock(g);
    }
    out
}

pub fn has_image() -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(CF_DIB).is_ok() }
}

pub fn has_text() -> bool {
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok() }
}

/// Paths of files copied in Explorer (CF_HDROP).
pub fn get_files() -> Result<Option<Vec<std::path::PathBuf>>> {
    use windows::Win32::UI::Shell::HDROP;
    let _open = Open::new()?;
    let h = match unsafe { GetClipboardData(CF_HDROP) } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            // No CF_HDROP: the shell's ID lists, else a single FileNameW.
            if let Ok(h) = unsafe { GetClipboardData(shell_idlist_format()) } {
                if !h.is_invalid() {
                    let files = files_from_idlist(HGLOBAL(h.0));
                    if !files.is_empty() {
                        return Ok(Some(files));
                    }
                }
            }
            if let Ok(h) = unsafe { GetClipboardData(filename_format()) } {
                if !h.is_invalid() {
                    let g = HGLOBAL(h.0);
                    let p = unsafe { GlobalLock(g) } as *const u16;
                    if !p.is_null() {
                        let mut len = 0;
                        while len < 32768 && unsafe { *p.add(len) } != 0 {
                            len += 1;
                        }
                        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
                        unsafe {
                            let _ = GlobalUnlock(g);
                        }
                        if !s.is_empty() {
                            return Ok(Some(vec![std::path::PathBuf::from(s)]));
                        }
                    }
                }
            }
            return Ok(None);
        }
    };
    Ok(Some(hdrop_paths(HDROP(h.0))))
}

/// The paths in a CF_HDROP.
fn hdrop_paths(drop: windows::Win32::UI::Shell::HDROP) -> Vec<std::path::PathBuf> {
    use windows::Win32::UI::Shell::DragQueryFileW;
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    let mut out = Vec::new();
    for i in 0..count {
        let len = unsafe { DragQueryFileW(drop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        let n = unsafe { DragQueryFileW(drop, i, Some(&mut buf)) } as usize;
        out.push(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])));
    }
    out
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

/// Only the OLE marker on the Win32 clipboard: the copying program (Explorer)
/// put its data object there with OleSetClipboard, and this process sees
/// none of its formats. Seen from the host's helper (SYSTEM) for every
/// Explorer copy: "DataObject" and nothing else.
pub fn only_ole_object() -> bool {
    use windows::Win32::System::DataExchange::{CountClipboardFormats, IsClipboardFormatAvailable, RegisterClipboardFormatW};
    unsafe {
        let data_object = RegisterClipboardFormatW(windows::core::w!("DataObject"));
        IsClipboardFormatAvailable(data_object).is_ok() && CountClipboardFormats() <= 2
    }
}

/// Run `f` on a short-lived OLE (single-threaded apartment) thread.
fn with_ole<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    std::thread::Builder::new()
        .name("nya-clipboard-read".into())
        .spawn(move || unsafe {
            let ok = windows::Win32::System::Ole::OleInitialize(None).is_ok();
            let r = f();
            if ok {
                windows::Win32::System::Ole::OleUninitialize();
            }
            r
        })
        .ok()?
        .join()
        .ok()
}

fn ole_get(obj: &windows::Win32::System::Com::IDataObject, cf: u32) -> Option<windows::Win32::System::Com::STGMEDIUM> {
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    let fmt = FORMATETC { cfFormat: cf as u16, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let m = unsafe { obj.GetData(&fmt) }.ok()?;
    (m.tymed == TYMED_HGLOBAL.0 as u32).then_some(m)
}

/// What the copying program's data object holds, read through OLE (when the
/// Win32 clipboard shows only its marker, see [`only_ole_object`]).
pub enum OleContent {
    Files(Vec<std::path::PathBuf>),
    Text(String),
    /// Neither; its formats, for logs.
    Other(String),
}

pub fn read_ole_clipboard() -> Result<OleContent> {
    let r = with_ole(|| -> Result<OleContent> {
        use windows::Win32::System::Com::{DATADIR_GET, FORMATETC};
        use windows::Win32::System::Ole::{OleGetClipboard, ReleaseStgMedium};
        let obj = unsafe { OleGetClipboard() }?;
        if let Some(mut m) = ole_get(&obj, CF_HDROP) {
            let files = hdrop_paths(windows::Win32::UI::Shell::HDROP(unsafe { m.u.hGlobal }.0));
            unsafe { ReleaseStgMedium(&mut m) };
            if !files.is_empty() {
                return Ok(OleContent::Files(files));
            }
        }
        if let Some(mut m) = ole_get(&obj, shell_idlist_format()) {
            let files = files_from_idlist(unsafe { m.u.hGlobal });
            unsafe { ReleaseStgMedium(&mut m) };
            if !files.is_empty() {
                return Ok(OleContent::Files(files));
            }
        }
        if let Some(mut m) = ole_get(&obj, CF_UNICODETEXT.0 as u32) {
            let g = unsafe { m.u.hGlobal };
            let p = unsafe { GlobalLock(g) } as *const u16;
            let mut text = None;
            if !p.is_null() {
                let mut len = 0;
                while unsafe { *p.add(len) } != 0 {
                    len += 1;
                }
                text = Some(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) }));
                unsafe {
                    let _ = GlobalUnlock(g);
                }
            }
            unsafe { ReleaseStgMedium(&mut m) };
            if let Some(t) = text {
                return Ok(OleContent::Text(t));
            }
        }
        // For the log: what it does offer.
        let mut names = Vec::new();
        if let Ok(e) = unsafe { obj.EnumFormatEtc(DATADIR_GET.0 as u32) } {
            let mut f = [FORMATETC::default(); 1];
            while names.len() < 40 && unsafe { e.Next(&mut f, None) }.is_ok() && f[0].cfFormat != 0 {
                let mut name = [0u16; 128];
                let n = unsafe { windows::Win32::System::DataExchange::GetClipboardFormatNameW(f[0].cfFormat as u32, &mut name) };
                names.push(if n > 0 { String::from_utf16_lossy(&name[..n as usize]) } else { f[0].cfFormat.to_string() });
                f[0] = FORMATETC::default();
            }
        }
        Ok(OleContent::Other(names.join(", ")))
    });
    r.unwrap_or_else(|| bail!("clipboard read thread failed"))
}
