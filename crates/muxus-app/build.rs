//! Windows: the app icon as icon resource 1 of `muxus.exe`. Explorer and the taskbar show it for
//! the file, GPUI gives it to the app's windows (it loads resource 1), and the page and topology
//! windows (`muxus --web-window`, see muxus-webview) load it the same way. The `.ico` is put
//! together from the PNGs the Linux packages install, so the icon has one source.

use std::{env, path::PathBuf};

/// The sizes Windows picks from: title bars, the taskbar, Alt+Tab and Explorer's views, at
/// 100% to 200% scaling.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let icons = manifest_dir.join("../../packaging/linux/icons");
    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| {
            let path = icons.join(format!("{size}x{size}.png"));
            println!("cargo:rerun-if-changed={}", path.display());
            let png =
                std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            (size, png)
        })
        .collect();

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let ico = out.join("muxus.ico");
    std::fs::write(&ico, ico_file(&images)).expect("the icon in OUT_DIR");
    let rc = out.join("muxus.rc");
    let quoted = ico.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{quoted}\"\n")).expect("the resource script in OUT_DIR");
    embed_resource::compile_for(&rc, ["muxus"], embed_resource::NONE)
        .manifest_optional()
        .expect("the icon resource");
}

/// An `.ico` holding each image as PNG, which Windows reads at every size since Vista.
fn ico_file(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    const HEADER: usize = 6;
    const ENTRY: usize = 16;
    let count = u16::try_from(images.len()).expect("a handful of sizes");
    let mut file = Vec::new();
    file.extend_from_slice(&0u16.to_le_bytes()); // reserved
    file.extend_from_slice(&1u16.to_le_bytes()); // an icon, not a cursor
    file.extend_from_slice(&count.to_le_bytes());
    let mut offset = HEADER + ENTRY * images.len();
    for (size, png) in images {
        // 0 stands for 256 pixels.
        let side = u8::try_from(*size).unwrap_or(0);
        file.extend_from_slice(&[side, side, 0, 0]); // width, height, no palette, reserved
        file.extend_from_slice(&1u16.to_le_bytes()); // color planes
        file.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        file.extend_from_slice(&u32::try_from(png.len()).unwrap().to_le_bytes());
        file.extend_from_slice(&u32::try_from(offset).unwrap().to_le_bytes());
        offset += png.len();
    }
    for (_, png) in images {
        file.extend_from_slice(png);
    }
    file
}
