//! Files on the clipboard that are not here yet ("copy on one computer, paste
//! on the other"). An OLE data object advertises CF_HDROP; only when an
//! application actually asks for the files (Explorer's Ctrl+V, a drop target,
//! GetClipboardData) does it call the provider, which fetches them over the
//! network into a local folder and returns their paths. Nothing is transferred
//! for a copy that is never pasted.
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
    STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{OleInitialize, OleIsCurrentClipboard, OleSetClipboard, OleUninitialize};
use windows::Win32::UI::Shell::{SHCreateStdEnumFmtEtc, DROPFILES};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG, PM_REMOVE, QS_ALLINPUT};

const CF_HDROP: u16 = 15;
const DV_E_FORMATETC: HRESULT = HRESULT(0x8004_0064_u32 as i32);
const OLE_E_ADVISENOTSUPPORTED: HRESULT = HRESULT(0x8004_0003_u32 as i32);
const DROPEFFECT_COPY: u32 = 1;

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

fn preferred_drop_effect() -> u16 {
    unsafe { RegisterClipboardFormatW(windows::core::w!("Preferred DropEffect")) as u16 }
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
    drop_effect: u16,
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
    fn formats(&self) -> [FORMATETC; 2] {
        let f = |cf| FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        [f(CF_HDROP), f(self.drop_effect)]
    }

    fn supports(&self, f: &FORMATETC) -> bool {
        (f.cfFormat == CF_HDROP || f.cfFormat == self.drop_effect) && f.tymed & TYMED_HGLOBAL.0 as u32 != 0
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
        if !self.supports(f) {
            return Err(DV_E_FORMATETC.into());
        }
        let g = if f.cfFormat == CF_HDROP {
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
    Offer(Provider),
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

    /// Put files on the clipboard that `provider` fetches when they are pasted.
    pub fn offer(&self, provider: Provider) {
        let _ = self.tx.send(Cmd::Offer(provider));
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
    let drop_effect = preferred_drop_effect();
    let mut current: Option<IDataObject> = None;
    let still_ours = |c: &Option<IDataObject>| c.as_ref().is_some_and(|o| unsafe { OleIsCurrentClipboard(o) }.is_ok());
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
                Cmd::Offer(provider) => {
                    let obj: IDataObject = DataObject { provider, files: Mutex::new(Files::Idle), drop_effect }.into();
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
            let obj: IDataObject = DataObject { provider, files: Mutex::new(Files::Idle), drop_effect: preferred_drop_effect() }.into();
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
        vc.offer(Arc::new(move || Ok(e2.clone())));

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
