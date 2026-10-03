//! Files on the clipboard that are not here yet ("copy on one computer, paste
//! on the other"). An OLE data object advertises them as virtual files
//! (`FileGroupDescriptorW` + `FileContents`, as Outlook attachments or zip
//! folders do): the list costs nothing to read; only when an application asks
//! for a file's contents (Explorer pasting) does it call the provider, which
//! fetches them over the network into a local folder. Nothing is transferred
//! for a copy that is never pasted.
//!
//! Not CF_HDROP: programs that read every clipboard change (the cloud
//! desktop's clipboard redirector, clipboard managers) asked for it at once,
//! started the whole transfer and held the clipboard open meanwhile, so that
//! real pastes failed ("clipboard busy"); Windows also gives up on a CF_HDROP
//! that takes more than 30 s. Contents are asked for through COM by index,
//! which such readers don't do. CF_HDROP is used only when the list cannot be
//! described (paths of 260 characters or more).
//!
//! OLE needs a single-threaded apartment with a message loop: [`VirtualClipboard`]
//! runs its own thread for that. Explorer calls the data object from its own
//! process through COM; when the object lives in a SYSTEM process (the host's
//! helper) that needs [`allow_interactive_callers`] first.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use windows::core::{implement, Result as WinResult, HRESULT};
use windows::Win32::Foundation::{BOOL, E_NOTIMPL, HGLOBAL, S_OK};
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, DATADIR_GET, DVASPECT_CONTENT, FORMATETC,
    STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{OleInitialize, OleSetClipboard, OleUninitialize};
use windows::Win32::UI::Shell::{
    SHCreateStdEnumFmtEtc, SHCreateStreamOnFileEx, DROPFILES, FD_ATTRIBUTES, FD_FILESIZE, FD_PROGRESSUI, FD_UNICODE,
    FILEDESCRIPTORW, FILEGROUPDESCRIPTORW,
};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT};

const CF_HDROP: u16 = 15;
const DV_E_FORMATETC: HRESULT = HRESULT(0x8004_0064_u32 as i32);
const DV_E_LINDEX: HRESULT = HRESULT(0x8004_0068_u32 as i32);
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
const STGM_READ: u32 = 0;
const STGM_SHARE_DENY_WRITE: u32 = 0x20;
/// `cFileName` of FILEDESCRIPTORW holds this many characters, NUL included.
const MAX_DESCRIBED_PATH: usize = 260;
const OLE_E_ADVISENOTSUPPORTED: HRESULT = HRESULT(0x8004_0003_u32 as i32);
const DROPEFFECT_COPY: u32 = 1;

/// One offered file or folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualFile {
    /// Relative path, '/' separated ("photos/2026/a.jpg").
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

/// The list as FileGroupDescriptorW wants it: safe relative paths, every
/// folder before what is in it, no duplicates. `None` if it can't be
/// described (empty, or a path too long for a descriptor).
fn describe(files: &[VirtualFile]) -> Option<Vec<VirtualFile>> {
    let mut out: Vec<VirtualFile> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in files {
        let parts: Vec<&str> = f.path.split(['/', '\\']).filter(|p| !p.is_empty() && *p != "." && *p != "..").collect();
        for depth in 1..=parts.len() {
            let last = depth == parts.len();
            let path = parts[..depth].join("/");
            if path.encode_utf16().count() >= MAX_DESCRIBED_PATH {
                return None;
            }
            if seen.insert(path.to_lowercase()) {
                let dir = !last || f.dir;
                out.push(VirtualFile { path, size: if dir { 0 } else { f.size }, dir });
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// FILEGROUPDESCRIPTORW for `entries` (from [`describe`]).
fn group_descriptor(entries: &[VirtualFile]) -> anyhow::Result<HGLOBAL> {
    let header = std::mem::offset_of!(FILEGROUPDESCRIPTORW, fgd);
    let size = header + entries.len() * std::mem::size_of::<FILEDESCRIPTORW>();
    unsafe {
        let g = GlobalAlloc(GMEM_MOVEABLE, size)?;
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        std::ptr::write_unaligned(p as *mut u32, entries.len() as u32);
        let list = p.add(header) as *mut FILEDESCRIPTORW;
        for (i, e) in entries.iter().enumerate() {
            // FILEDESCRIPTORW is packed: the name is filled in on the side.
            let mut name = [0u16; MAX_DESCRIBED_PATH];
            for (j, c) in e.path.replace('/', "\\").encode_utf16().take(MAX_DESCRIBED_PATH - 1).enumerate() {
                name[j] = c;
            }
            let d = FILEDESCRIPTORW {
                cFileName: name,
                dwFlags: (FD_ATTRIBUTES.0 | FD_FILESIZE.0 | FD_PROGRESSUI.0 | FD_UNICODE.0) as u32,
                dwFileAttributes: if e.dir { FILE_ATTRIBUTE_DIRECTORY } else { FILE_ATTRIBUTE_NORMAL },
                nFileSizeHigh: (e.size >> 32) as u32,
                nFileSizeLow: e.size as u32,
                ..Default::default()
            };
            std::ptr::write_unaligned(list.add(i), d);
        }
        let _ = GlobalUnlock(g);
        Ok(g)
    }
}

/// Fetches the offered files; blocks until they are local. Runs on a worker
/// thread (the clipboard thread keeps serving other requests meanwhile). A
/// success is kept; after a failure the next paste calls it again (files
/// that already arrived are not fetched twice: the provider's side knows).
pub type Provider = Arc<dyn Fn() -> std::result::Result<Vec<PathBuf>, String> + Send + Sync>;

/// `DROPFILES` + NUL-separated wide paths, as CF_HDROP expects.
pub fn dropfiles(paths: &[PathBuf]) -> anyhow::Result<HGLOBAL> {
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().to_string_lossy().encode_utf16());
        list.push(0);
    }
    list.push(0);
    let header = std::mem::size_of::<DROPFILES>();
    unsafe {
        let g = GlobalAlloc(GMEM_MOVEABLE, header + list.len() * 2)?;
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        let df = DROPFILES { pFiles: header as u32, fWide: true.into(), ..Default::default() };
        std::ptr::copy_nonoverlapping(&df as *const DROPFILES as *const u8, p, header);
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, p.add(header), list.len() * 2);
        let _ = GlobalUnlock(g);
        Ok(g)
    }
}

fn hglobal_u32(v: u32) -> anyhow::Result<HGLOBAL> {
    unsafe {
        let g = GlobalAlloc(GMEM_MOVEABLE, 4)?;
        let p = GlobalLock(g) as *mut u32;
        if p.is_null() {
            anyhow::bail!("GlobalLock failed");
        }
        *p = v;
        let _ = GlobalUnlock(g);
        Ok(g)
    }
}

/// Clipboard formats registered by name.
#[derive(Clone, Copy)]
struct Formats {
    drop_effect: u16,
    descriptor: u16,
    contents: u16,
}

impl Formats {
    fn get() -> Self {
        unsafe {
            Self {
                drop_effect: RegisterClipboardFormatW(windows::core::w!("Preferred DropEffect")) as u16,
                descriptor: RegisterClipboardFormatW(windows::core::w!("FileGroupDescriptorW")) as u16,
                contents: RegisterClipboardFormatW(windows::core::w!("FileContents")) as u16,
            }
        }
    }
}

/// Where an offer's files are.
type Slot = Arc<Mutex<Option<std::result::Result<Vec<PathBuf>, String>>>>;

enum Files {
    /// Not asked for yet, or the last attempt failed.
    Idle,
    /// Being fetched; the worker fills the slot.
    Fetching(Slot),
    Ready(Vec<PathBuf>),
}

#[implement(IDataObject)]
struct DataObject {
    provider: Provider,
    files: Mutex<Files>,
    /// Described as virtual files (else CF_HDROP).
    entries: Option<Vec<VirtualFile>>,
    cf: Formats,
}

/// Dispatch window messages (and with them COM calls into this apartment) for up to `ms`.
fn pump(ms: u32) {
    unsafe {
        MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT);
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

impl DataObject {
    fn new(files: &[VirtualFile], provider: Provider, cf: Formats) -> Self {
        Self { provider, files: Mutex::new(Files::Idle), entries: describe(files), cf }
    }

    fn formats(&self) -> Vec<FORMATETC> {
        let f = |cf, tymed: i32| FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: tymed as u32 };
        match &self.entries {
            Some(_) => vec![
                f(self.cf.descriptor, TYMED_HGLOBAL.0),
                // One per file, by index; listed for the first (as is usual).
                FORMATETC { lindex: 0, ..f(self.cf.contents, TYMED_ISTREAM.0) },
                f(self.cf.drop_effect, TYMED_HGLOBAL.0),
            ],
            None => vec![f(CF_HDROP, TYMED_HGLOBAL.0), f(self.cf.drop_effect, TYMED_HGLOBAL.0)],
        }
    }

    fn supports(&self, f: &FORMATETC) -> bool {
        self.formats().iter().any(|o| o.cfFormat == f.cfFormat && o.tymed & f.tymed != 0)
    }

    /// Stream on the fetched copy of entry `index` (fetching the files first).
    fn contents(&self, index: i32) -> WinResult<STGMEDIUM> {
        let entry = usize::try_from(index).ok().and_then(|i| self.entries.as_ref()?.get(i)).filter(|e| !e.dir);
        // Index -1 is what a program reading every clipboard change gets
        // (GetClipboardData): no transfer for that.
        let Some(entry) = entry else { return Err(DV_E_LINDEX.into()) };
        let paths = self.paths().map_err(|e| {
            tracing::warn!("clipboard files: {e}");
            windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e)
        })?;
        // The provider returns the top-level items of the folder it fetched into.
        let root = paths.first().and_then(|p| p.parent()).ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        let local = entry.path.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
        let stream = unsafe {
            SHCreateStreamOnFileEx(&windows::core::HSTRING::from(local.as_os_str()), STGM_READ | STGM_SHARE_DENY_WRITE, 0, false, None)
        }
        .inspect_err(|e| tracing::warn!("clipboard files: open {}: {e}", local.display()))?;
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 { pstm: std::mem::ManuallyDrop::new(Some(stream)) },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }

    /// The local paths, fetching them the first time. Waits without blocking
    /// the apartment: another paste or a clipboard viewer asking meanwhile is
    /// served (and waits for the same fetch) instead of stalling behind it.
    fn paths(&self) -> std::result::Result<Vec<PathBuf>, String> {
        let slot = {
            let mut g = self.files.lock().unwrap();
            match &*g {
                Files::Ready(p) => return Ok(p.clone()),
                Files::Fetching(s) => s.clone(),
                Files::Idle => {
                    let s: Slot = Arc::default();
                    let (provider, out) = (self.provider.clone(), s.clone());
                    let spawned = std::thread::Builder::new().name("nya-clipboard-fetch".into()).spawn(move || {
                        let r = provider();
                        *out.lock().unwrap() = Some(r);
                    });
                    if let Err(e) = spawned {
                        return Err(format!("fetch thread: {e}"));
                    }
                    *g = Files::Fetching(s.clone());
                    s
                }
            }
        };
        loop {
            let done = slot.lock().unwrap().clone();
            if let Some(r) = done {
                let mut g = self.files.lock().unwrap();
                match &r {
                    Ok(p) => *g = Files::Ready(p.clone()),
                    // Try again on the next paste.
                    Err(_) => {
                        if matches!(&*g, Files::Fetching(s) if Arc::ptr_eq(s, &slot)) {
                            *g = Files::Idle;
                        }
                    }
                }
                return r;
            }
            pump(50);
        }
    }
}

fn medium(g: HGLOBAL) -> STGMEDIUM {
    STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: g }, pUnkForRelease: std::mem::ManuallyDrop::new(None) }
}

impl IDataObject_Impl for DataObject_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        let f = unsafe { pformatetcin.as_ref() }.ok_or(windows::core::Error::from(DV_E_FORMATETC))?;
        tracing::debug!("clipboard files: GetData cf={} lindex={} tymed={:#x}", f.cfFormat, f.lindex, f.tymed);
        if !self.supports(f) {
            return Err(DV_E_FORMATETC.into());
        }
        if f.cfFormat == self.cf.contents {
            return self.contents(f.lindex);
        }
        let g = if f.cfFormat == self.cf.descriptor {
            group_descriptor(self.entries.as_deref().unwrap_or_default())
        } else if f.cfFormat == CF_HDROP {
            let paths = self.paths().map_err(|e| {
                tracing::warn!("clipboard files: {e}");
                windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e)
            })?;
            dropfiles(&paths)
        } else {
            hglobal_u32(DROPEFFECT_COPY)
        };
        g.map(medium).map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_OUTOFMEMORY, e.to_string()))
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if let Some(f) = unsafe { pformatetc.as_ref() } {
            tracing::trace!("clipboard files: QueryGetData cf={} lindex={} tymed={:#x}", f.cfFormat, f.lindex, f.tymed);
        }
        match unsafe { pformatetc.as_ref() } {
            Some(f) if self.supports(f) => S_OK,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, pformatetcout: *mut FORMATETC) -> HRESULT {
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        windows::Win32::Foundation::E_NOTIMPL
    }

    fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> WinResult<()> {
        // Explorer reports "Performed DropEffect" / "Paste Succeeded" here; not needed.
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        unsafe { SHCreateStdEnumFmtEtc(&self.formats()) }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Option<&IAdviseSink>) -> WinResult<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> WinResult<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

enum Cmd {
    Offer(Vec<VirtualFile>, Provider),
    Clear,
    Stop,
}

/// Owner of the virtual clipboard content (one OLE thread).
pub struct VirtualClipboard {
    tx: Sender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Is our data object on the clipboard right now?
    ours: Arc<std::sync::atomic::AtomicBool>,
    /// Thread id of the OLE clipboard thread.
    tid: Arc<std::sync::atomic::AtomicU32>,
}

impl VirtualClipboard {
    pub fn start() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let ours = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let o = ours.clone();
        let tid = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let t = tid.clone();
        let thread = std::thread::Builder::new().name("nya-clipboard-ole".into()).spawn(move || {
            t.store(unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }, std::sync::atomic::Ordering::SeqCst);
            run(rx, o)
        })
        .ok();
        Self { tx, thread, ours, tid }
    }

    /// Put `files` on the clipboard; `provider` fetches them when they are pasted.
    pub fn offer(&self, files: Vec<VirtualFile>, provider: Provider) {
        let _ = self.tx.send(Cmd::Offer(files, provider));
    }

    /// Remove our offer if it is still on the clipboard (the other side went away).
    pub fn clear(&self) {
        let _ = self.tx.send(Cmd::Clear);
    }

    /// True while the clipboard holds our (virtual) files: then the files on
    /// the clipboard came from the other computer and must not be offered back.
    pub fn is_ours(&self) -> bool {
        use windows::Win32::System::DataExchange::GetClipboardOwner;
        use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
        if !self.ours.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        // Ask Windows directly: the flag above is refreshed only every ~100 ms,
        // and a copy made right after our offer must not be mistaken for it.
        // OLE's clipboard window belongs to our clipboard thread.
        let tid = self.tid.load(std::sync::atomic::Ordering::SeqCst);
        match unsafe { GetClipboardOwner() } {
            Ok(owner) if !owner.is_invalid() => (unsafe { GetWindowThreadProcessId(owner, None) }) == tid,
            _ => false,
        }
    }
}

impl Drop for VirtualClipboard {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(rx: Receiver<Cmd>, ours: Arc<std::sync::atomic::AtomicBool>) {
    if let Err(e) = unsafe { OleInitialize(None) } {
        tracing::warn!("clipboard files unavailable: OleInitialize: {e}");
        return;
    }
    let cf = Formats::get();
    let mut current: Option<IDataObject> = None;
    // OLE's clipboard window belongs to this thread. (Not OleIsCurrentClipboard:
    // the bindings turn its S_FALSE into success, so a later copy by the user
    // was taken for ours and emptied by Clear / Stop.)
    let me = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    let still_ours = |c: &Option<IDataObject>| {
        use windows::Win32::System::DataExchange::GetClipboardOwner;
        use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
        c.is_some()
            && match unsafe { GetClipboardOwner() } {
                Ok(owner) if !owner.is_invalid() => (unsafe { GetWindowThreadProcessId(owner, None) }) == me,
                _ => false,
            }
    };
    loop {
        // Pump messages (OLE marshals calls into this thread), wake up for commands.
        pump(100);
        ours.store(still_ours(&current), std::sync::atomic::Ordering::SeqCst);
        loop {
            let cmd = match rx.try_recv() {
                Ok(c) => c,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => Cmd::Stop,
            };
            match cmd {
                Cmd::Offer(files, provider) => {
                    let obj = DataObject::new(&files, provider, cf);
                    if obj.entries.is_none() {
                        tracing::info!("clipboard files: {} item(s) offered as CF_HDROP (paths too long to describe)", files.len());
                    }
                    let obj: IDataObject = obj.into();
                    // Our clipboard watchers must see this before the change becomes visible.
                    ours.store(true, std::sync::atomic::Ordering::SeqCst);
                    let mut set = false;
                    for _ in 0..10 {
                        if unsafe { OleSetClipboard(&obj) }.is_ok() {
                            set = true;
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    if set {
                        current = Some(obj);
                    } else {
                        tracing::warn!("OleSetClipboard failed: clipboard busy");
                        ours.store(still_ours(&current), std::sync::atomic::Ordering::SeqCst);
                    }
                }
                Cmd::Clear => {
                    if still_ours(&current) {
                        // Empty rather than flush: flushing would fetch the files.
                        let _ = unsafe { OleSetClipboard(None) };
                    }
                    current = None;
                    ours.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                Cmd::Stop => {
                    if still_ours(&current) {
                        let _ = unsafe { OleSetClipboard(None) };
                    }
                    unsafe { OleUninitialize() };
                    return;
                }
            }
        }
    }
}

/// Let processes of the logged-on user (Explorer pasting our files) call COM
/// objects of this process. Needed in the host's helper, which runs as SYSTEM:
/// COM's default only admits the process's own account. Must run before the
/// process uses COM for anything that marshals.
pub fn allow_interactive_callers() -> anyhow::Result<()> {
    use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{MakeAbsoluteSD, ACL, PSECURITY_DESCRIPTOR, PSID};
    use windows::Win32::System::Com::{CoInitializeSecurity, EOAC_NONE, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IDENTIFY};
    crate::com_init();
    // COM_RIGHTS_EXECUTE | EXECUTE_LOCAL for SYSTEM, administrators and
    // interactive users; medium integrity callers may call up.
    let sddl = windows::core::w!("O:SYG:SYD:(A;;0x3;;;SY)(A;;0x3;;;BA)(A;;0x3;;;IU)S:(ML;;NX;;;ME)");
    unsafe {
        let mut rel = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &mut rel, None)?;
        // CoInitializeSecurity wants an absolute security descriptor.
        let (mut sd_len, mut dacl_len, mut sacl_len, mut owner_len, mut group_len) = (0u32, 0u32, 0u32, 0u32, 0u32);
        let _ = MakeAbsoluteSD(rel, PSECURITY_DESCRIPTOR::default(), &mut sd_len, None, &mut dacl_len, None, &mut sacl_len, PSID::default(), &mut owner_len, PSID::default(), &mut group_len);
        // Leaked on purpose: COM keeps using the descriptor for the process lifetime.
        let sd = Box::leak(vec![0u8; sd_len as usize].into_boxed_slice());
        let dacl = Box::leak(vec![0u8; dacl_len.max(8) as usize].into_boxed_slice());
        let sacl = Box::leak(vec![0u8; sacl_len.max(8) as usize].into_boxed_slice());
        let owner = Box::leak(vec![0u8; owner_len.max(4) as usize].into_boxed_slice());
        let group = Box::leak(vec![0u8; group_len.max(4) as usize].into_boxed_slice());
        MakeAbsoluteSD(
            rel,
            PSECURITY_DESCRIPTOR(sd.as_mut_ptr() as _),
            &mut sd_len,
            Some(dacl.as_mut_ptr() as *mut ACL),
            &mut dacl_len,
            Some(sacl.as_mut_ptr() as *mut ACL),
            &mut sacl_len,
            PSID(owner.as_mut_ptr() as _),
            &mut owner_len,
            PSID(group.as_mut_ptr() as _),
            &mut group_len,
        )?;
        let _ = windows::Win32::Foundation::LocalFree(windows::Win32::Foundation::HLOCAL(rel.0));
        CoInitializeSecurity(
            PSECURITY_DESCRIPTOR(sd.as_mut_ptr() as _),
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IDENTIFY,
            None,
            EOAC_NONE,
            None,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropfiles_layout() {
        let g = dropfiles(&[PathBuf::from(r"C:\a\b.txt"), PathBuf::from(r"C:\c")]).unwrap();
        unsafe {
            let p = GlobalLock(g) as *const u8;
            let df = &*(p as *const DROPFILES);
            assert!(df.fWide.as_bool());
            let list = p.add(df.pFiles as usize) as *const u16;
            let text: Vec<u16> = (0..17).map(|i| *list.add(i)).collect();
            let s = String::from_utf16_lossy(&text);
            assert!(s.starts_with("C:\\a\\b.txt\0C:\\c\0\0"));
            let _ = GlobalUnlock(g);
            let _ = windows::Win32::Foundation::GlobalFree(g);
        }
    }

    #[test]
    fn descriptors_list_folders_first() {
        let f = |path: &str, size, dir| VirtualFile { path: path.into(), size, dir };
        let d = describe(&[f("a/b/c.txt", 3, false), f("a/b", 0, true), f("x.bin", 9, false), f("../evil/./y", 1, false), f("A/B/d.txt", 4, false)]).unwrap();
        let got: Vec<(&str, u64, bool)> = d.iter().map(|e| (e.path.as_str(), e.size, e.dir)).collect();
        assert_eq!(
            got,
            [("a", 0, true), ("a/b", 0, true), ("a/b/c.txt", 3, false), ("x.bin", 9, false), ("evil", 0, true), ("evil/y", 1, false), ("A/B/d.txt", 4, false)]
        );
        assert!(describe(&[]).is_none());
        assert!(describe(&[f(&"x".repeat(300), 1, false)]).is_none(), "too long for a descriptor: CF_HDROP");

        let g = group_descriptor(&d[..3]).unwrap();
        unsafe {
            let p = GlobalLock(g) as *const u8;
            assert_eq!(std::ptr::read_unaligned(p as *const u32), 3);
            let list = p.add(std::mem::offset_of!(FILEGROUPDESCRIPTORW, fgd)) as *const FILEDESCRIPTORW;
            let third = std::ptr::read_unaligned(list.add(2));
            let chars = third.cFileName;
            let name = String::from_utf16_lossy(&chars[..9]);
            assert_eq!(name, "a\\b\\c.txt");
            assert_eq!((third.nFileSizeLow, third.dwFileAttributes), (3, FILE_ATTRIBUTE_NORMAL));
            let first = std::ptr::read_unaligned(list);
            let attrs = first.dwFileAttributes;
            assert_eq!(attrs, FILE_ATTRIBUTE_DIRECTORY);
            let _ = GlobalUnlock(g);
            let _ = windows::Win32::Foundation::GlobalFree(g);
        }
    }

    /// Virtual files, asked for from another apartment (as Explorer does):
    /// the list and a whole-clipboard read (index -1) transfer nothing; the
    /// first file's contents fetch once, then every file streams from the
    /// fetched copy.
    #[test]
    fn contents_fetch_only_when_a_file_is_read() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use windows::core::Interface;
        use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
        use windows::Win32::System::Com::StructuredStorage::CoGetInterfaceAndReleaseStream;
        use windows::Win32::System::Com::{IStream, STREAM_SEEK_SET};

        let root = std::env::temp_dir().join(format!("nya-virtual-files-{}", std::process::id()));
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("dir").join("a.txt"), b"hello").unwrap();
        std::fs::write(root.join("b.txt"), b"xyz").unwrap();
        let files = vec![
            VirtualFile { path: "dir/a.txt".into(), size: 5, dir: false },
            VirtualFile { path: "b.txt".into(), size: 3, dir: false },
        ];
        let calls = Arc::new(AtomicU32::new(0));
        let (c, r) = (calls.clone(), root.clone());
        let provider: Provider = Arc::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(vec![r.join("dir"), r.join("b.txt")])
        });
        let cf = Formats::get();
        let (tx, rx) = std::sync::mpsc::channel::<usize>();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        let owner = std::thread::spawn(move || unsafe {
            OleInitialize(None).unwrap();
            let obj: IDataObject = DataObject::new(&files, provider, cf).into();
            tx.send(CoMarshalInterThreadInterfaceInStream(&IDataObject::IID, &obj).unwrap().into_raw() as usize).unwrap();
            while !s.load(Ordering::SeqCst) {
                pump(20);
            }
            drop(obj);
            OleUninitialize();
        });
        let stream = rx.recv().unwrap();
        let read_all = |m: &STGMEDIUM| -> Vec<u8> {
            let st: &IStream = unsafe { m.u.pstm.as_ref() }.unwrap();
            unsafe { st.Seek(0, STREAM_SEEK_SET, None).unwrap() };
            let mut buf = vec![0u8; 64];
            let mut n = 0u32;
            let _ = unsafe { st.Read(buf.as_mut_ptr().cast(), 64, Some(&mut n)) };
            buf.truncate(n as usize);
            buf
        };
        unsafe {
            OleInitialize(None).unwrap();
            let obj: IDataObject = CoGetInterfaceAndReleaseStream(&IStream::from_raw(stream as *mut _)).unwrap();
            let fmt = |cf: u16, lindex: i32, tymed: i32| FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex, tymed: tymed as u32 };
            assert!(obj.QueryGetData(&fmt(CF_HDROP, -1, TYMED_HGLOBAL.0)).is_err(), "no CF_HDROP for described files");
            let list = obj.GetData(&fmt(cf.descriptor, -1, TYMED_HGLOBAL.0)).unwrap();
            let p = GlobalLock(list.u.hGlobal) as *const u32;
            assert_eq!(*p, 3, "dir, dir/a.txt, b.txt");
            let _ = GlobalUnlock(list.u.hGlobal);
            assert!(obj.GetData(&fmt(cf.contents, -1, TYMED_ISTREAM.0)).is_err());
            assert!(obj.GetData(&fmt(cf.contents, 0, TYMED_ISTREAM.0)).is_err(), "a folder has no contents");
            assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing fetched yet");
            let a = obj.GetData(&fmt(cf.contents, 1, TYMED_ISTREAM.0 | TYMED_HGLOBAL.0)).unwrap();
            assert_eq!(a.tymed, TYMED_ISTREAM.0 as u32);
            assert_eq!(read_all(&a), b"hello");
            let b = obj.GetData(&fmt(cf.contents, 2, TYMED_ISTREAM.0)).unwrap();
            assert_eq!(read_all(&b), b"xyz");
            assert_eq!(calls.load(Ordering::SeqCst), 1, "one fetch for all files");
            drop((a, b, list));
            drop(obj);
            OleUninitialize();
        }
        stop.store(true, Ordering::SeqCst);
        owner.join().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The data object lives in one apartment (as on the clipboard thread),
    /// callers in others (as Explorer and clipboard viewers do through COM).
    /// While one paste waits for a slow fetch, another caller is answered at
    /// once and the fetch runs only once; a failed fetch is retried next time.
    /// No system clipboard involved.
    #[test]
    fn slow_fetch_does_not_block_other_callers() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Instant;
        use windows::core::Interface;
        use windows::Win32::System::Com::Marshal::CoMarshalInterThreadInterfaceInStream;
        use windows::Win32::System::Com::StructuredStorage::CoGetInterfaceAndReleaseStream;
        use windows::Win32::System::Com::IStream;

        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let provider: Provider = Arc::new(move || {
            let n = c.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(600));
            if n == 0 {
                Err("first attempt fails".into())
            } else {
                Ok(vec![PathBuf::from("C:\\x.txt")])
            }
        });
        // Owner apartment: create, marshal twice, keep pumping.
        let (tx, rx) = std::sync::mpsc::channel::<(usize, usize)>();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        let owner = std::thread::spawn(move || unsafe {
            OleInitialize(None).unwrap();
            // No list: the CF_HDROP way.
            let obj: IDataObject = DataObject::new(&[], provider, Formats::get()).into();
            let a = CoMarshalInterThreadInterfaceInStream(&IDataObject::IID, &obj).unwrap();
            let b = CoMarshalInterThreadInterfaceInStream(&IDataObject::IID, &obj).unwrap();
            tx.send((a.into_raw() as usize, b.into_raw() as usize)).unwrap();
            while !s.load(Ordering::SeqCst) {
                pump(20);
            }
            drop(obj);
            OleUninitialize();
        });
        let (a, b) = rx.recv().unwrap();
        let caller = move |stream: usize, delay_ms: u64| {
            std::thread::spawn(move || unsafe {
                let fmt = FORMATETC { cfFormat: CF_HDROP, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
                OleInitialize(None).unwrap();
                let obj: IDataObject = CoGetInterfaceAndReleaseStream(&IStream::from_raw(stream as *mut _)).unwrap();
                std::thread::sleep(Duration::from_millis(delay_ms));
                let t = Instant::now();
                let q = obj.QueryGetData(&fmt);
                let quick = t.elapsed();
                let first = obj.GetData(&fmt).is_ok();
                let second = obj.GetData(&fmt).is_ok();
                drop(obj);
                OleUninitialize();
                (q.is_ok(), quick, first, second)
            })
        };
        let slow = caller(a, 0);
        let other = caller(b, 150);
        let (q1, _, first1, second1) = slow.join().unwrap();
        let (q2, quick2, first2, second2) = other.join().unwrap();
        stop.store(true, Ordering::SeqCst);
        owner.join().unwrap();
        assert!(q1 && q2);
        assert!(quick2 < Duration::from_millis(300), "answered while the fetch runs ({quick2:?})");
        // The first fetch fails for both waiting on it; the next one succeeds and is kept.
        assert!(!first1 && !first2);
        assert!(second1 && second2);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one fetch per attempt, shared by both callers");
    }

    /// Offer, read back through the real clipboard (this process owns it),

    /// clear. Uses the desktop clipboard, so it only runs when asked for.
    #[test]
    #[ignore]
    fn offer_and_paste_roundtrip() {
        // Put back the text the user had copied.
        let saved = crate::clipboard::get_text().ok().flatten();
        let vc = VirtualClipboard::start();
        let dir = std::env::temp_dir();
        let expected = vec![dir.join("nya-virtual-a.txt")];
        let e2 = expected.clone();
        vc.offer(Vec::new(), Arc::new(move || Ok(e2.clone())));

        std::thread::sleep(Duration::from_millis(400));
        assert!(vc.is_ours());
        assert!(crate::clipboard::has_files());
        let got = crate::clipboard::get_files().unwrap().unwrap();
        assert_eq!(got, expected);
        // Something else is copied right after: no longer ours, at once.
        crate::clipboard::set_text("nya test").unwrap();
        assert!(!vc.is_ours());
        vc.clear();
        std::thread::sleep(Duration::from_millis(400));
        assert!(!vc.is_ours());
        if let Some(t) = saved {
            let _ = crate::clipboard::set_text(&t);
        }
    }
}
