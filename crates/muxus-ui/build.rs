//! Link-time workaround for Linux machines that have the xkbcommon-x11 runtime library but not its
//! development package: GPUI's X11 backend links `-lxkbcommon-x11`, which needs the unversioned
//! `libxkbcommon-x11.so`. When only `libxkbcommon-x11.so.0` exists, point the linker at a symlink in
//! OUT_DIR. Search paths from a build script reach every binary that depends on this crate.

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let dirs = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib64",
        "/usr/lib",
    ];
    for name in ["xkbcommon-x11", "xkbcommon"] {
        let unversioned = format!("lib{name}.so");
        if dirs
            .iter()
            .any(|d| PathBuf::from(d).join(&unversioned).exists())
        {
            continue;
        }
        let Some(versioned) = dirs
            .iter()
            .map(|d| PathBuf::from(d).join(format!("{unversioned}.0")))
            .find(|p| p.exists())
        else {
            continue;
        };
        let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("syslibs");
        std::fs::create_dir_all(&out).expect("create syslibs dir");
        let link = out.join(&unversioned);
        let _ = std::fs::remove_file(&link);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&versioned, &link).expect("symlink system library");
        println!("cargo:rustc-link-search=native={}", out.display());
    }
}
