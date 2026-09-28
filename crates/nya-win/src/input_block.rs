//! Block the host's own keyboard and mouse while a remote user works in
//! privacy mode. Low-level hooks swallow every event that was not injected
//! (`SendInput` from the input thread still gets through). Hooks only see the
//! desktop they were installed on, so the thread follows the input desktop
//! (Default ↔ Winlogon). Ctrl+Alt+Del cannot be blocked by design.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::StationsAndDesktops::{CloseDesktop, OpenInputDesktop, DESKTOP_CONTROL_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, SetWindowsHookExW, TranslateMessage,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_REMOVE,
    QS_ALLINPUT, WH_KEYBOARD_LL, WH_MOUSE_LL,
};

use crate::desktop::{desktop_name, DesktopTracker};

const LLMHF_INJECTED: u32 = 0x1;
const LLMHF_LOWER_IL_INJECTED: u32 = 0x2;

pub struct InputBlocker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl InputBlocker {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("nya-input-block".into())
            .spawn(move || run(s))
            .ok();
        tracing::info!("local keyboard and mouse blocked");
        Self { stop, thread }
    }
}

impl Drop for InputBlocker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        tracing::info!("local keyboard and mouse unblocked");
    }
}

unsafe extern "system" fn keyboard(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(l.0 as *const KBDLLHOOKSTRUCT);
        if k.flags.0 & LLKHF_INJECTED.0 == 0 {
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, w, l)
}

unsafe extern "system" fn mouse(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let m = &*(l.0 as *const MSLLHOOKSTRUCT);
        if m.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) == 0 {
            return LRESULT(1);
        }
    }
    CallNextHookEx(HHOOK::default(), code, w, l)
}

struct Hooks(Vec<HHOOK>);

impl Hooks {
    fn install() -> Self {
        let mut v = Vec::new();
        unsafe {
            match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), None, 0) {
                Ok(h) => v.push(h),
                Err(e) => tracing::warn!("keyboard block hook: {e}"),
            }
            match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), None, 0) {
                Ok(h) => v.push(h),
                Err(e) => tracing::warn!("mouse block hook: {e}"),
            }
        }
        Self(v)
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        for h in self.0.drain(..) {
            unsafe {
                let _ = UnhookWindowsHookEx(h);
            }
        }
    }
}

fn input_desktop_name() -> Option<String> {
    let h = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, windows::Win32::System::StationsAndDesktops::DESKTOP_ACCESS_FLAGS(0x0001)) }.ok()?;
    let n = desktop_name(h);
    unsafe {
        let _ = CloseDesktop(h);
    }
    Some(n)
}

fn run(stop: Arc<AtomicBool>) {
    let mut desktop = DesktopTracker::new();
    let _ = desktop.sync();
    let mut hooks = Some(Hooks::install());
    let mut last_check = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        unsafe {
            MsgWaitForMultipleObjects(None, false, 100, QS_ALLINPUT);
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if last_check.elapsed() > Duration::from_millis(300) {
            last_check = Instant::now();
            if input_desktop_name().is_some_and(|n| n != desktop.name()) {
                // A thread with hooks cannot change desktops.
                drop(hooks.take());
                if let Err(e) = desktop.sync() {
                    tracing::debug!("input block: desktop switch: {e:#}");
                }
                hooks = Some(Hooks::install());
            }
        }
    }
    drop(hooks);
}
