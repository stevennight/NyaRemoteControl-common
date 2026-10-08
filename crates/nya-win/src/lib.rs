//! Windows platform layer shared by the host and the client.

pub mod audio;
pub mod clipboard;
pub mod clipboard_files;
pub mod convert;
pub mod d3d;
pub mod desktop;
pub mod devnode;
pub mod display_config;
pub mod dpi;
pub mod duplication;
pub mod gdi;
pub mod input;
pub mod input_block;
pub mod package;
pub mod print;
pub mod shell;
pub mod topology;
pub mod transfer;
pub mod transfer12;
pub mod update;

pub use windows;

/// Initialise COM (multithreaded) on the current thread; safe to call repeatedly.
pub fn com_init() {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// Raise the current thread's scheduling priority via MMCSS ("Games", "Pro Audio", "Capture").
pub fn mmcss_boost(task: &str) {
    use windows::core::HSTRING;
    use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    let mut idx = 0u32;
    unsafe {
        if AvSetMmThreadCharacteristicsW(&HSTRING::from(task), &mut idx).is_err() {
            tracing::debug!("MMCSS task {task} unavailable");
        }
    }
}
