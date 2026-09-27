//! Link against the FFmpeg import libraries and copy the runtime DLLs into the
//! cargo profile directory (next to the executables) so `cargo run` works.
//!
//! FFmpeg location: `NYA_FFMPEG_DIR`, or `<repo parent>/third_party/ffmpeg`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=NYA_FFMPEG_DIR");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dir = std::env::var_os("NYA_FFMPEG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../../third_party/ffmpeg"));
    let lib = dir.join("lib");
    let bin = dir.join("bin");
    if !lib.join("avcodec.lib").exists() {
        panic!(
            "FFmpeg not found at {} — run common/scripts/fetch-ffmpeg.ps1 or set NYA_FFMPEG_DIR",
            dir.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=avcodec");
    println!("cargo:rustc-link-lib=dylib=avutil");
    println!("cargo:bin_dir={}", bin.display());

    // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    if let Some(profile_dir) = out.ancestors().nth(3) {
        copy_dlls(&bin, profile_dir);
        copy_dlls(&bin, &profile_dir.join("deps"));
    }
}

fn copy_dlls(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else { return };
    let _ = std::fs::create_dir_all(to);
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")) {
            let dst = to.join(p.file_name().unwrap());
            let fresh = std::fs::metadata(&dst)
                .and_then(|d| Ok(d.len() == e.metadata()?.len()))
                .unwrap_or(false);
            if !fresh {
                let _ = std::fs::copy(&p, &dst);
            }
        }
    }
}
