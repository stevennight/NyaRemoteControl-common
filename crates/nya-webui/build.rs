//! Build the pages in `common/web` (npm + Vite) and embed `common/web/dist`.
//!
//! * `NYA_WEB_PREBUILT=1`: don't run npm, embed whatever `dist` holds.
//! * Otherwise npm runs when the sources are newer than `dist` (first build:
//!   `npm ci` too). Node.js must be installed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn newest(dir: &Path, skip: &[&str]) -> SystemTime {
    let mut t = SystemTime::UNIX_EPOCH;
    let Ok(rd) = std::fs::read_dir(dir) else { return t };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if skip.contains(&name.as_str()) {
            continue;
        }
        let p = e.path();
        let m = if p.is_dir() { newest(&p, skip) } else { e.metadata().and_then(|m| m.modified()).unwrap_or(t) };
        t = t.max(m);
    }
    t
}

fn npm(web: &Path, args: &[&str]) {
    let status = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(args)
        .current_dir(web)
        .status()
        .unwrap_or_else(|e| panic!("cannot run npm ({e}); install Node.js, or set NYA_WEB_PREBUILT=1 with a built common/web/dist"));
    assert!(status.success(), "npm {} failed in {}", args.join(" "), web.display());
}

fn files(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, base, out);
        } else {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, p));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let web = manifest.join("../../web").canonicalize().expect("common/web not found");
    let dist = web.join("dist");
    for f in ["src", "client.html", "package.json", "vite.config.ts", "dist"] {
        println!("cargo:rerun-if-changed={}", web.join(f).display());
    }
    println!("cargo:rerun-if-env-changed=NYA_WEB_PREBUILT");

    if std::env::var_os("NYA_WEB_PREBUILT").is_none() {
        if !web.join("node_modules").exists() {
            npm(&web, &["ci", "--no-audit", "--no-fund"]);
        }
        let src = newest(&web, &["node_modules", "dist"]);
        let built = if dist.join("client.html").exists() { newest(&dist, &[]) } else { SystemTime::UNIX_EPOCH };
        if src > built {
            npm(&web, &["run", "build"]);
        }
    }
    assert!(dist.join("client.html").exists(), "{} has no built pages", dist.display());

    let mut list = Vec::new();
    files(&dist, &dist, &mut list);
    list.sort();
    let mut code = String::from("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    for (rel, path) in list {
        code += &format!("    ({rel:?}, include_bytes!({:?})),\n", path.display().to_string());
    }
    code += "];\n";
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("assets.rs");
    std::fs::write(out, code).unwrap();
}
