//! Following the input desktop (Default ↔ Winlogon/secure desktop).
//! Threads that capture or inject must call [`DesktopTracker::sync`] before
//! (re)creating the duplicator and before injecting input.

use anyhow::Result;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetUserObjectInformationW, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS,
    DESKTOP_CONTROL_FLAGS, HDESK, UOI_NAME,
};

/// All specific desktop rights (READOBJECTS … SWITCHDESKTOP).
const DESKTOP_ALL: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(0x01ff);

pub struct DesktopTracker {
    current: Option<HDESK>,
    name: String,
}

// HDESK is a plain handle; the tracker is only used by its owning thread.
unsafe impl Send for DesktopTracker {}

pub fn desktop_name(h: HDESK) -> String {
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    unsafe {
        let _ = GetUserObjectInformationW(
            HANDLE(h.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut _),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        );
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

impl DesktopTracker {
    pub fn new() -> Self {
        Self { current: None, name: String::new() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Attach the calling thread to the current input desktop.
    /// Returns true if the desktop changed.
    pub fn sync(&mut self) -> Result<bool> {
        let h = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ALL)? };
        let name = desktop_name(h);
        if name == self.name && self.current.is_some() {
            unsafe {
                let _ = CloseDesktop(h);
            }
            return Ok(false);
        }
        if let Err(e) = unsafe { SetThreadDesktop(h) } {
            unsafe {
                let _ = CloseDesktop(h);
            }
            return Err(e.into());
        }
        if let Some(old) = self.current.replace(h) {
            unsafe {
                let _ = CloseDesktop(old);
            }
        }
        tracing::info!("input desktop: {name}");
        self.name = name;
        Ok(true)
    }
}

impl Default for DesktopTracker {
    fn default() -> Self {
        Self::new()
    }
}
