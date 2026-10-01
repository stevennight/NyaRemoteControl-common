//! Installers for optional third-party components: pinned download (system
//! proxy via WinINet, SHA-256 verified), zip extraction and running setup
//! programs hidden or elevated.

use std::ffi::c_void;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::Networking::WinInet::{
    HttpQueryInfoW, InternetCloseHandle, InternetOpenUrlW, InternetOpenW, InternetReadFile, InternetSetOptionW,
    HTTP_QUERY_CONTENT_LENGTH, HTTP_QUERY_STATUS_CODE, INTERNET_FLAG_NO_CACHE_WRITE, INTERNET_FLAG_RELOAD,
    INTERNET_OPEN_TYPE_PRECONFIG, INTERNET_OPTION_CONNECT_TIMEOUT, INTERNET_OPTION_RECEIVE_TIMEOUT,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

/// A pinned download.
#[derive(Debug, Clone, Copy)]
pub struct Package {
    /// File name, also looked up in the offline `drivers` folders.
    pub file: &'static str,
    pub url: &'static str,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: &'static str,
}

/// Progress callback: (bytes done, total if known).
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// Lower-case hex SHA-256 of a file.
pub fn sha256_file(p: &Path) -> Result<String> {
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `drivers` next to the executable: offline copies bundled by the package scripts.
pub fn bundled_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join("drivers"))
}

/// Where downloads are kept.
pub fn cache_dir() -> PathBuf {
    std::env::temp_dir().join("nya-components")
}

/// The package file: a bundled or cached copy if its hash matches, else downloaded.
pub fn obtain(pkg: &Package, progress: Progress) -> Result<PathBuf> {
    for dir in bundled_dir().into_iter().chain([cache_dir()]) {
        let p = dir.join(pkg.file);
        if p.exists() {
            if sha256_file(&p).ok().as_deref() == Some(pkg.sha256) {
                tracing::info!("using {}", p.display());
                return Ok(p);
            }
            tracing::warn!("{} does not match the expected hash; ignoring it", p.display());
        }
    }
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    let part = dir.join(format!("{}.part", pkg.file));
    download(pkg.url, &part, progress).with_context(|| format!("下载 {}", pkg.url))?;
    let got = sha256_file(&part)?;
    if got != pkg.sha256 {
        let _ = std::fs::remove_file(&part);
        bail!("{} 校验失败（SHA-256 {got}），已丢弃", pkg.file);
    }
    let dest = dir.join(pkg.file);
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

struct Inet(*mut c_void);

impl Drop for Inet {
    fn drop(&mut self) {
        unsafe {
            let _ = InternetCloseHandle(self.0);
        }
    }
}

fn query_u64(h: &Inet, what: u32) -> Option<u64> {
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * 2) as u32;
    unsafe { HttpQueryInfoW(h.0, what, Some(buf.as_mut_ptr() as *mut c_void), &mut len, None).ok()? };
    String::from_utf16_lossy(&buf[..len as usize / 2]).trim().parse().ok()
}

/// HTTP(S) GET to a file, following redirects, using the system proxy settings.
pub fn download(url: &str, dest: &Path, progress: Progress) -> Result<()> {
    let mut f = std::fs::File::create(dest)?;
    get(url, &mut |chunk| f.write_all(chunk).map_err(Into::into), progress)
}

/// A small HTTP(S) resource (API answer, checksum file) in memory, at most `limit` bytes.
pub fn fetch(url: &str, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    get(
        url,
        &mut |chunk| {
            if out.len() + chunk.len() > limit {
                bail!("{url} 太大（超过 {limit} 字节）");
            }
            out.extend_from_slice(chunk);
            Ok(())
        },
        &mut |_, _| {},
    )?;
    Ok(out)
}

/// GET `url` (system proxy, 30 s timeouts), handing the body to `sink` piece by piece.
fn get(url: &str, sink: &mut dyn FnMut(&[u8]) -> Result<()>, progress: Progress) -> Result<()> {
    unsafe {
        let session = InternetOpenW(w!("NyaRemoteControl"), INTERNET_OPEN_TYPE_PRECONFIG.0, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            bail!("InternetOpen: {}", windows::core::Error::from_win32());
        }
        let session = Inet(session);
        for opt in [INTERNET_OPTION_CONNECT_TIMEOUT, INTERNET_OPTION_RECEIVE_TIMEOUT] {
            let ms: u32 = 30_000;
            let _ = InternetSetOptionW(Some(session.0), opt, Some(&ms as *const u32 as *const c_void), 4);
        }
        let req = InternetOpenUrlW(
            session.0,
            &HSTRING::from(url),
            None,
            INTERNET_FLAG_RELOAD | INTERNET_FLAG_NO_CACHE_WRITE,
            0,
        );
        if req.is_null() {
            bail!("连接失败: {}", windows::core::Error::from_win32());
        }
        let req = Inet(req);
        if let Some(code) = query_u64(&req, HTTP_QUERY_STATUS_CODE) {
            if code != 200 {
                bail!("HTTP {code}");
            }
        }
        let total = query_u64(&req, HTTP_QUERY_CONTENT_LENGTH);
        let mut buf = vec![0u8; 1 << 16];
        let mut done = 0u64;
        loop {
            let mut n = 0u32;
            InternetReadFile(req.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut n)
                .map_err(|e| anyhow!("读取失败: {e}"))?;
            if n == 0 {
                break;
            }
            sink(&buf[..n as usize])?;
            done += n as u64;
            progress(done, total);
        }
        if let Some(t) = total {
            if done != t {
                bail!("下载不完整（{done}/{t} 字节）");
            }
        }
    }
    Ok(())
}

fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000) // CREATE_NO_WINDOW
}

/// Extract a zip with the system's tar.exe into a fresh `dest`.
pub fn unzip(zip: &Path, dest: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest)?;
    let tar = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| "C:\\Windows".into()).join("System32\\tar.exe");
    let st = no_window(&mut Command::new(tar)).arg("-xf").arg(zip).arg("-C").arg(dest).status().context("运行 tar")?;
    if !st.success() {
        bail!("解压 {} 失败", zip.display());
    }
    Ok(())
}

/// Run a program without a window and return its exit code (caller already elevated).
pub fn run_hidden(exe: &Path, args: &[&str]) -> Result<u32> {
    let st = no_window(&mut Command::new(exe)).args(args).status().with_context(|| format!("运行 {}", exe.display()))?;
    Ok(st.code().unwrap_or(-1) as u32)
}

/// Run a program elevated (UAC prompt), hidden, and wait for its exit code.
pub fn run_elevated(exe: &Path, args: &str) -> Result<u32> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 0, // SW_HIDE
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| anyhow!("没有获得管理员权限"))?;
        if info.hProcess.is_invalid() {
            bail!("无法启动 {}", exe.display());
        }
        let mut code = 0u32;
        if WaitForSingleObject(info.hProcess, INFINITE) == WAIT_OBJECT_0 {
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
        }
        let _ = CloseHandle(info.hProcess);
        Ok(code)
    }
}

/// Start a program elevated (UAC prompt) without waiting for it; Err when
/// the user declines.
pub fn start_elevated(exe: &Path, args: &str) -> Result<()> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: 1, // SW_SHOWNORMAL
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| anyhow!("没有获得管理员权限"))?;
        if !info.hProcess.is_invalid() {
            let _ = CloseHandle(info.hProcess);
        }
    }
    Ok(())
}

/// Windows Installer / setup exit codes that mean success (3010 = reboot required).
pub fn setup_ok(code: u32) -> Option<bool> {
    match code {
        0 => Some(false),
        3010 | 1641 => Some(true),
        _ => None,
    }
}
