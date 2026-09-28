//! Display configuration (CCD API: `QueryDisplayConfig` / `SetDisplayConfig`)
//! and display modes: which targets are active, their positions, GDI names,
//! HDR white level, per-monitor scaling. Used to put a virtual display in
//! front of (or instead of) the physical ones and to undo it afterwards.
//!
//! Must run in the console session (the helper process), not in session 0.

use anyhow::{anyhow, bail, Result};
use windows::core::PCWSTR;
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    SetDisplayConfig, DISPLAYCONFIG_ADAPTER_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS, QUERY_DISPLAY_CONFIG_FLAGS,
    SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_USE_DATABASE_CURRENT, SDC_USE_SUPPLIED_DISPLAY_CONFIG,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND, LUID};
use windows::Win32::Graphics::Gdi::{
    ChangeDisplaySettingsExW, EnumDisplaySettingsW, CDS_TYPE, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_DISPLAY_SETTINGS_MODE,
};

pub const PATH_ACTIVE: u32 = 0x1;
pub const MODE_IDX_INVALID: u32 = 0xffff_ffff;

fn wide(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn luid_eq(a: LUID, b: LUID) -> bool {
    a.LowPart == b.LowPart && a.HighPart == b.HighPart
}

/// A snapshot of the display configuration.
#[derive(Clone)]
pub struct Config {
    pub paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    pub modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

impl Config {
    /// Active paths only, or every possible source/target combination.
    pub fn query(all: bool) -> Result<Self> {
        let flags: QUERY_DISPLAY_CONFIG_FLAGS = if all { QDC_ALL_PATHS } else { QDC_ONLY_ACTIVE_PATHS };
        for _ in 0..5 {
            let (mut np, mut nm) = (0u32, 0u32);
            let r = unsafe { GetDisplayConfigBufferSizes(flags, &mut np, &mut nm) };
            if r != ERROR_SUCCESS {
                bail!("GetDisplayConfigBufferSizes: {r:?}");
            }
            let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
            let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
            let r = unsafe { QueryDisplayConfig(flags, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) };
            if r == ERROR_INSUFFICIENT_BUFFER {
                continue; // changed in between
            }
            if r != ERROR_SUCCESS {
                bail!("QueryDisplayConfig: {r:?}");
            }
            paths.truncate(np as usize);
            modes.truncate(nm as usize);
            return Ok(Self { paths, modes });
        }
        bail!("QueryDisplayConfig: configuration keeps changing")
    }

    pub fn active(&self) -> impl Iterator<Item = &DISPLAYCONFIG_PATH_INFO> {
        self.paths.iter().filter(|p| p.flags & PATH_ACTIVE != 0)
    }

    /// Source mode (resolution + desktop position) of an active path.
    pub fn source_mode(&self, p: &DISPLAYCONFIG_PATH_INFO) -> Option<(u32, u32, i32, i32)> {
        let i = unsafe { p.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let m = self.modes.get(i)?;
        (m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE).then(|| {
            let s = unsafe { m.Anonymous.sourceMode };
            (s.width, s.height, s.position.x, s.position.y)
        })
    }
}

/// Apply exactly these paths (every other path becomes inactive). Mode
/// entries not referenced by `paths` are dropped and indices renumbered.
/// `save` stores the layout for the current set of monitors, so Windows does
/// not fall back to an older one on the next display change.
pub fn apply(paths: &[DISPLAYCONFIG_PATH_INFO], modes: &[DISPLAYCONFIG_MODE_INFO], save: bool) -> Result<()> {
    let mut out_modes: Vec<DISPLAYCONFIG_MODE_INFO> = Vec::new();
    let mut remap = |idx: u32| -> u32 {
        match modes.get(idx as usize) {
            Some(m) if idx != MODE_IDX_INVALID => {
                out_modes.push(*m);
                (out_modes.len() - 1) as u32
            }
            _ => MODE_IDX_INVALID,
        }
    };
    let mut out_paths = paths.to_vec();
    for p in &mut out_paths {
        unsafe {
            p.sourceInfo.Anonymous.modeInfoIdx = remap(p.sourceInfo.Anonymous.modeInfoIdx);
            p.targetInfo.Anonymous.modeInfoIdx = remap(p.targetInfo.Anonymous.modeInfoIdx);
        }
    }
    let mut flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
    if save {
        flags |= SDC_SAVE_TO_DATABASE;
    }
    let r = unsafe {
        SetDisplayConfig(Some(&out_paths), (!out_modes.is_empty()).then_some(out_modes.as_slice()), flags)
    };
    if r != 0 {
        bail!("SetDisplayConfig: error {r}");
    }
    Ok(())
}

/// Re-apply the configuration Windows has stored for the monitors that are
/// connected now (undoes our changes once the virtual display is gone).
pub fn restore_database() -> Result<()> {
    let r = unsafe { SetDisplayConfig(None, None, SDC_APPLY | SDC_USE_DATABASE_CURRENT) };
    if r != 0 {
        bail!("SetDisplayConfig(USE_DATABASE_CURRENT): error {r}");
    }
    Ok(())
}

fn header(ty: DISPLAYCONFIG_DEVICE_INFO_TYPE, size: usize, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: ty, size: size as u32, adapterId: adapter, id }
}

/// `\\.\DISPLAYn` of a path's source.
pub fn source_gdi_name(p: &DISPLAYCONFIG_PATH_INFO) -> Option<String> {
    let mut n = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
            p.sourceInfo.adapterId,
            p.sourceInfo.id,
        ),
        ..Default::default()
    };
    (unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0).then(|| wide(&n.viewGdiDeviceName))
}

/// Device interface path of an adapter, e.g. `\\?\ROOT#DISPLAY#0000#{…}`.
pub fn adapter_path(adapter: LUID) -> Option<String> {
    let mut n = DISPLAYCONFIG_ADAPTER_NAME {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADAPTER_NAME,
            std::mem::size_of::<DISPLAYCONFIG_ADAPTER_NAME>(),
            adapter,
            0,
        ),
        ..Default::default()
    };
    (unsafe { DisplayConfigGetDeviceInfo(&mut n.header) } == 0).then(|| wide(&n.adapterDevicePath))
}

/// Does this adapter device path belong to the device instance `instance_id`
/// (`ROOT\DISPLAY\0000` ↔ `\\?\ROOT#DISPLAY#0000#{…}`)?
pub fn adapter_path_matches(path: &str, instance_id: &str) -> bool {
    let want = instance_id.replace('\\', "#").to_ascii_uppercase();
    let path = path.to_ascii_uppercase();
    path.strip_prefix(r"\\?\").unwrap_or(&path).starts_with(&format!("{want}#"))
}

/// Active path whose source is this GDI device.
pub fn active_path_for(gdi_name: &str) -> Option<DISPLAYCONFIG_PATH_INFO> {
    let cfg = Config::query(false).ok()?;
    let found = cfg.active().find(|p| source_gdi_name(p).is_some_and(|n| n.eq_ignore_ascii_case(gdi_name))).copied();
    found
}

/// Brightness of SDR white on an HDR display, in nits ("SDR content
/// brightness" in Windows settings). `None` if unknown.
pub fn sdr_white_nits(gdi_name: &str) -> Option<f32> {
    let p = active_path_for(gdi_name)?;
    let mut w = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(),
            p.targetInfo.adapterId,
            p.targetInfo.id,
        ),
        SDRWhiteLevel: 0,
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut w.header) } != 0 || w.SDRWhiteLevel == 0 {
        return None;
    }
    // SDRWhiteLevel is a multiplier of 80 nits, scaled by 1000.
    Some(w.SDRWhiteLevel as f32 / 1000.0 * 80.0)
}

/// Is `path` on the given adapter (source side)?
pub fn on_adapter(p: &DISPLAYCONFIG_PATH_INFO, adapter: LUID) -> bool {
    luid_eq(p.sourceInfo.adapterId, adapter) || luid_eq(p.targetInfo.adapterId, adapter)
}

// ------------------------------------------------------------------ modes

/// Display modes (width, height, refresh) the display offers.
pub fn modes(gdi_name: &str) -> Vec<(u32, u32, u32)> {
    let name: Vec<u16> = gdi_name.encode_utf16().chain([0]).collect();
    let mut out = Vec::new();
    for i in 0.. {
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        if !unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }.as_bool() {
            break;
        }
        let m = (dm.dmPelsWidth, dm.dmPelsHeight, dm.dmDisplayFrequency);
        if !out.contains(&m) {
            out.push(m);
        }
    }
    out
}

/// Change the resolution (and refresh rate, when `hz` > 0) of a display.
/// Only this display's mode changes (nothing is written to the registry:
/// `CDS_UPDATEREGISTRY` would re-apply the stored settings of every display
/// and switch detached ones back on). Persist it with [`apply`] + `save`.
pub fn set_mode(gdi_name: &str, width: u32, height: u32, hz: u32) -> Result<()> {
    let name: Vec<u16> = gdi_name.encode_utf16().chain([0]).collect();
    let try_set = |hz: u32| {
        let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
        dm.dmPelsWidth = width;
        dm.dmPelsHeight = height;
        dm.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
        if hz > 0 {
            dm.dmDisplayFrequency = hz;
            dm.dmFields |= DM_DISPLAYFREQUENCY;
        }
        unsafe { ChangeDisplaySettingsExW(PCWSTR(name.as_ptr()), Some(&dm), HWND::default(), CDS_TYPE(0), None) }
    };
    let mut r = try_set(hz);
    if r != DISP_CHANGE_SUCCESSFUL && hz > 0 {
        r = try_set(0);
    }
    if r != DISP_CHANGE_SUCCESSFUL {
        bail!("ChangeDisplaySettingsEx {gdi_name} {width}x{height}: {}", r.0);
    }
    Ok(())
}

// ------------------------------------------------------------------ scaling

/// Scale factors Windows offers, in the order of its "relative" steps.
const DPI_STEPS: [u32; 12] = [100, 125, 150, 175, 200, 225, 250, 300, 350, 400, 450, 500];

// Undocumented request types used by Settings > Display > Scale.
const GET_DPI_SCALE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(-3);
const SET_DPI_SCALE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(-4);

#[repr(C)]
struct GetDpi {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    min_rel: i32,
    cur_rel: i32,
    max_rel: i32,
}

#[repr(C)]
struct SetDpi {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    rel: i32,
}

/// Closest supported scale step to `percent`.
pub fn nearest_scale(percent: u32) -> u32 {
    *DPI_STEPS.iter().min_by_key(|&&s| (s as i64 - percent as i64).abs()).unwrap()
}

/// Set a display's scaling (100 = 100 %), like Settings > Display > Scale.
pub fn set_scale(gdi_name: &str, percent: u32) -> Result<u32> {
    let p = active_path_for(gdi_name).ok_or_else(|| anyhow!("{gdi_name} is not active"))?;
    let (adapter, id) = (p.sourceInfo.adapterId, p.sourceInfo.id);
    let mut g = GetDpi {
        header: header(GET_DPI_SCALE, std::mem::size_of::<GetDpi>(), adapter, id),
        min_rel: 0,
        cur_rel: 0,
        max_rel: 0,
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut g.header) } != 0 {
        bail!("get DPI scale failed");
    }
    // Relative steps are counted from the recommended scale, which sits at -min_rel.
    let recommended = (-g.min_rel).clamp(0, DPI_STEPS.len() as i32 - 1);
    let want = DPI_STEPS.iter().position(|&s| s == nearest_scale(percent)).unwrap() as i32;
    let rel = (want - recommended).clamp(g.min_rel, g.max_rel);
    let s = SetDpi { header: header(SET_DPI_SCALE, std::mem::size_of::<SetDpi>(), adapter, id), rel };
    if unsafe { DisplayConfigSetDeviceInfo(&s.header) } != 0 {
        bail!("set DPI scale failed");
    }
    Ok(DPI_STEPS[(recommended + rel) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_paths() {
        let p = r"\\?\ROOT#DISPLAY#0000#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}";
        assert!(adapter_path_matches(p, r"ROOT\DISPLAY\0000"));
        assert!(adapter_path_matches(p, r"root\display\0000"));
        assert!(!adapter_path_matches(p, r"ROOT\DISPLAY\00001"));
        assert!(!adapter_path_matches(p, r"ROOT\DISPLAY\000"));
    }

    #[test]
    fn scales() {
        assert_eq!(nearest_scale(0), 100);
        assert_eq!(nearest_scale(150), 150);
        assert_eq!(nearest_scale(160), 150);
        assert_eq!(nearest_scale(290), 300);
        assert_eq!(nearest_scale(900), 500);
    }

    /// Read-only queries; safe on any machine with a display.
    #[test]
    fn query_active() {
        let Ok(cfg) = Config::query(false) else { return };
        for p in cfg.active() {
            let _ = source_gdi_name(p);
            let _ = cfg.source_mode(p);
        }
    }
}
