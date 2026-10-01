//! Updates from GitHub Releases: the latest release of a repository, version
//! comparison, and downloading its installer checked against the `.sha256`
//! file published next to it (scripts/release-lib.ps1 writes both).
//!
//! Only regular releases count (GitHub's "latest" skips pre-releases and
//! drafts); installers are the `*_x64-setup.exe` assets.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::package;

pub const SERVER_REPO: &str = "stevennight/NyaRemoteControl-server";
pub const CLIENT_REPO: &str = "stevennight/NyaRemoteControl-client";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Without the leading `v`.
    pub version: String,
    /// Release notes (Markdown).
    pub notes: String,
    /// The release's web page.
    pub page: String,
    pub installer: Asset,
    /// The `<installer>.sha256` asset.
    pub sha256: Asset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// The latest regular release of `repo` ("owner/name").
pub fn latest(repo: &str) -> Result<Release> {
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let body = package::fetch(&url, 4 << 20).context("查询最新版本")?;
    parse_release(&body)
}

fn parse_release(json: &[u8]) -> Result<Release> {
    let r: GhRelease = serde_json::from_slice(json).context("解析版本信息")?;
    if r.draft || r.prerelease {
        bail!("最新版本是草稿或预发布版");
    }
    let asset = |pred: &dyn Fn(&str) -> bool| {
        r.assets.iter().find(|a| pred(&a.name)).map(|a| Asset { name: a.name.clone(), url: a.browser_download_url.clone(), size: a.size })
    };
    let installer = asset(&|n| n.ends_with("_x64-setup.exe")).ok_or_else(|| anyhow!("{} 没有安装包", r.tag_name))?;
    let sha_name = format!("{}.sha256", installer.name);
    let sha256 = asset(&|n| n == sha_name).ok_or_else(|| anyhow!("{} 没有安装包的校验文件", r.tag_name))?;
    Ok(Release {
        version: r.tag_name.trim_start_matches('v').to_owned(),
        notes: r.body.unwrap_or_default(),
        page: r.html_url,
        installer,
        sha256,
    })
}

/// Is `candidate` a later version than `current`? (`MAJOR.MINOR.PATCH[-PRE]`;
/// a pre-release sorts before its release; unparsable versions never are.)
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => cmp_version(&a, &b) == std::cmp::Ordering::Greater,
        _ => false,
    }
}

type Version<'a> = ([u64; 3], Option<&'a str>);

fn parse_version(v: &str) -> Option<Version<'_>> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next()?; // build metadata does not count
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut n = [0u64; 3];
    let mut parts = core.split('.');
    for x in &mut n {
        *x = parts.next()?.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some((n, pre))
}

fn cmp_version(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match a.0.cmp(&b.0) {
        Equal => {}
        o => return o,
    }
    match (a.1, b.1) {
        (None, None) => Equal,
        (None, Some(_)) => Greater,
        (Some(_), None) => Less,
        (Some(x), Some(y)) => {
            // Dot-separated identifiers: numeric ones numerically, before alphanumeric ones.
            let (mut xs, mut ys) = (x.split('.'), y.split('.'));
            loop {
                match (xs.next(), ys.next()) {
                    (None, None) => return Equal,
                    (None, Some(_)) => return Less,
                    (Some(_), None) => return Greater,
                    (Some(p), Some(q)) => {
                        let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                            (Ok(m), Ok(n)) => m.cmp(&n),
                            (Ok(_), Err(_)) => Less,
                            (Err(_), Ok(_)) => Greater,
                            (Err(_), Err(_)) => p.cmp(q),
                        };
                        if o != Equal {
                            return o;
                        }
                    }
                }
            }
        }
    }
}

/// Download the release's installer into `dir` and check it against the
/// published SHA-256. A matching file already there is reused.
pub fn download_installer(r: &Release, dir: &Path, progress: package::Progress) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let sha = package::fetch(&r.sha256.url, 4096).context("下载校验文件")?;
    let want = parse_sha256_file(&String::from_utf8_lossy(&sha)).ok_or_else(|| anyhow!("校验文件格式不对"))?;
    let dest = dir.join(&r.installer.name);
    if dest.exists() && package::sha256_file(&dest).ok().as_deref() == Some(want.as_str()) {
        return Ok(dest);
    }
    let part = dir.join(format!("{}.part", r.installer.name));
    package::download(&r.installer.url, &part, progress).context("下载安装包")?;
    let got = package::sha256_file(&part)?;
    if got != want {
        let _ = std::fs::remove_file(&part);
        bail!("安装包校验失败（SHA-256 {got}，应为 {want}），已丢弃");
    }
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&part, &dest)?;
    Ok(dest)
}

/// `<64 hex digits>  <file name>` (sha256sum format).
fn parse_sha256_file(s: &str) -> Option<String> {
    let h = s.split_whitespace().next()?.to_ascii_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "1.0.0-beta.2"));
        assert!(is_newer("1.0.0-beta.10", "1.0.0-beta.2"));
        assert!(is_newer("1.0.0-rc.1", "1.0.0-beta.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0+abc"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("garbage", "0.2.0"));
        assert!(!is_newer("1.0", "0.2.0"));
    }

    #[test]
    fn release_from_github() {
        let json = br#"{
            "tag_name": "v0.2.1", "body": "fixes", "html_url": "https://github.com/x/y/releases/tag/v0.2.1",
            "draft": false, "prerelease": false,
            "assets": [
                {"name": "NyaRemoteControl-Server_0.2.1_windows_x64.zip", "browser_download_url": "https://e/zip", "size": 1},
                {"name": "NyaRemoteControl-Server_0.2.1_x64-setup.exe", "browser_download_url": "https://e/setup", "size": 2},
                {"name": "NyaRemoteControl-Server_0.2.1_x64-setup.exe.sha256", "browser_download_url": "https://e/sha", "size": 3}
            ]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "0.2.1");
        assert_eq!((r.installer.url.as_str(), r.sha256.url.as_str()), ("https://e/setup", "https://e/sha"));
        let no_sha = br#"{"tag_name": "v1.0.0", "html_url": "", "assets": [{"name": "a_x64-setup.exe", "browser_download_url": "u", "size": 1}]}"#;
        assert!(parse_release(no_sha).is_err());
    }

    #[test]
    fn sha256_file_format() {
        let h = "ab".repeat(32);
        assert_eq!(parse_sha256_file(&format!("{h}  file.exe\n")), Some(h.clone()));
        assert_eq!(parse_sha256_file(&h.to_uppercase()), Some(h));
        assert_eq!(parse_sha256_file("xyz  file"), None);
    }

    /// Talks to GitHub: `cargo test -p nya-win -- --ignored live`.
    #[test]
    #[ignore]
    fn live_latest_release() {
        for repo in [SERVER_REPO, CLIENT_REPO] {
            let r = latest(repo).unwrap();
            println!("{repo}: {} {} ({} bytes)", r.version, r.installer.name, r.installer.size);
            let sha = package::fetch(&r.sha256.url, 4096).unwrap();
            assert!(parse_sha256_file(&String::from_utf8_lossy(&sha)).is_some());
        }
    }
}
