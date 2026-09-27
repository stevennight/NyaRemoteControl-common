//! Known folders.

use std::path::PathBuf;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};

/// The user's Downloads folder. `token` selects another user (e.g. the
/// console user from a SYSTEM service); `None` = the calling user.
pub fn downloads_dir(token: Option<HANDLE>) -> Option<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KNOWN_FOLDER_FLAG(0), token.unwrap_or_default()).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.map(PathBuf::from)
    }
}

/// Where received files go: `<Downloads>\NyaRemoteControl`.
pub fn receive_dir(token: Option<HANDLE>) -> Option<PathBuf> {
    downloads_dir(token).map(|d| d.join("NyaRemoteControl"))
}
