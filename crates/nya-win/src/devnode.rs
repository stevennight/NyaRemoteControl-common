//! Root-enumerated software devices (the virtual display driver): create the
//! device node, install its driver, enable / disable it. Needs administrator
//! rights.

use anyhow::{anyhow, Context, Result};
use windows::core::{GUID, HSTRING, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetClassInstallParamsW,
    SetupDiSetDeviceRegistryPropertyW, UpdateDriverForPlugAndPlayDevicesW, DICD_GENERATE_ID, DICS_DISABLE,
    DICS_ENABLE, DICS_FLAG_GLOBAL, DIF_PROPERTYCHANGE, DIF_REGISTERDEVICE, DIGCF_ALLCLASSES, HDEVINFO,
    INSTALLFLAG_FORCE, SPDRP_HARDWAREID, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_PROPCHANGE_PARAMS,
};
use windows::Win32::Foundation::{BOOL, HWND};

/// Display adapters.
pub const CLASS_DISPLAY: GUID = GUID::from_u128(0x4d36e968_e325_11ce_bfc1_08002be10318);

struct DevInfo(HDEVINFO);

impl Drop for DevInfo {
    fn drop(&mut self) {
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.0);
        }
    }
}

fn multi_sz(s: &str) -> Vec<u8> {
    let mut w: Vec<u16> = s.encode_utf16().collect();
    w.extend([0, 0]);
    w.iter().flat_map(|c| c.to_le_bytes()).collect()
}

fn hardware_ids(set: &DevInfo, dev: &SP_DEVINFO_DATA) -> Vec<String> {
    let mut buf = vec![0u8; 2048];
    let mut need = 0u32;
    let ok = unsafe { SetupDiGetDeviceRegistryPropertyW(set.0, dev, SPDRP_HARDWAREID, None, Some(&mut buf), Some(&mut need)) };
    if ok.is_err() {
        return Vec::new();
    }
    let w: Vec<u16> = buf[..need as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    w.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
}

/// Call `f` for every present-or-not device with this hardware id.
fn for_each(hwid: &str, mut f: impl FnMut(&DevInfo, &SP_DEVINFO_DATA) -> Result<()>) -> Result<usize> {
    let set = unsafe { SetupDiGetClassDevsW(None, PCWSTR::null(), HWND::default(), DIGCF_ALLCLASSES) }.context("SetupDiGetClassDevs")?;
    let set = DevInfo(set);
    let mut n = 0;
    for i in 0.. {
        let mut dev = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        if unsafe { SetupDiEnumDeviceInfo(set.0, i, &mut dev) }.is_err() {
            break;
        }
        if hardware_ids(&set, &dev).iter().any(|h| h.eq_ignore_ascii_case(hwid)) {
            f(&set, &dev)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Does a device node with this hardware id exist?
pub fn exists(hwid: &str) -> bool {
    for_each(hwid, |_, _| Ok(())).unwrap_or(0) > 0
}

/// Create a root device node (like `devcon install`) unless one exists, then
/// install / update the driver from `inf`. Returns whether a reboot is needed.
pub fn install_root_device(hwid: &str, class: &GUID, class_name: &str, inf: &std::path::Path) -> Result<bool> {
    if !exists(hwid) {
        unsafe {
            let set = DevInfo(SetupDiCreateDeviceInfoList(Some(class), HWND::default()).context("SetupDiCreateDeviceInfoList")?);
            let mut dev = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
            SetupDiCreateDeviceInfoW(set.0, &HSTRING::from(class_name), class, PCWSTR::null(), HWND::default(), DICD_GENERATE_ID, Some(&mut dev))
                .context("SetupDiCreateDeviceInfo")?;
            SetupDiSetDeviceRegistryPropertyW(set.0, &mut dev, SPDRP_HARDWAREID, Some(&multi_sz(hwid)))
                .context("设置硬件 ID")?;
            SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, Some(&dev)).context("注册设备")?;
        }
    }
    let mut reboot = BOOL(0);
    unsafe {
        UpdateDriverForPlugAndPlayDevicesW(HWND::default(), &HSTRING::from(hwid), &HSTRING::from(inf.as_os_str()), INSTALLFLAG_FORCE, Some(&mut reboot))
            .map_err(|e| anyhow!("安装驱动失败: {e}"))?;
    }
    Ok(reboot.as_bool())
}

/// Enable or disable every device with this hardware id.
pub fn set_enabled(hwid: &str, enabled: bool) -> Result<usize> {
    for_each(hwid, |set, dev| unsafe {
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: std::mem::size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: if enabled { DICS_ENABLE } else { DICS_DISABLE },
            Scope: DICS_FLAG_GLOBAL,
            HwProfile: 0,
        };
        SetupDiSetClassInstallParamsW(
            set.0,
            Some(dev),
            Some(&params.ClassInstallHeader),
            std::mem::size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        )
        .context("SetupDiSetClassInstallParams")?;
        SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set.0, Some(dev)).context(if enabled { "启用设备" } else { "禁用设备" })?;
        Ok(())
    })
}
