//! `cargo xtask vcxsrv`: prepares VcXsrv, the X server Windows builds ship for X11 forwarding
//! (`electron/scripts/vcxsrv.mjs`). Downloads a pinned upstream installer, verifies it, unpacks
//! it with 7-Zip and keeps only what `vcxsrv.exe -multiwindow` needs (about 31 of 102 MB; the
//! rest is legacy bitmap fonts, demo clients and tools) in `vendor/vcxsrv`, where a development
//! build of the app finds it. A Windows package puts that folder next to `muxus.exe`.
//!
//! `cargo xtask vcxsrv --source DIR` saves the GPL corresponding source to
//! `DIR/vcxsrv-<version>-source.tar.gz` instead.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};

pub const VCXSRV_VERSION: &str = "21.1.16.1";

struct Pinned {
    url: String,
    sha256: &'static str,
}

fn installer() -> Pinned {
    Pinned {
        url: format!(
            "https://github.com/marchaesen/vcxsrv/releases/download/{VCXSRV_VERSION}/vcxsrv-64.{VCXSRV_VERSION}.installer.exe"
        ),
        sha256: "df7fed8f49665d0592528ab6be9d07111ea73c6848283d128b77690e05b8f90b",
    }
}

fn license() -> Pinned {
    Pinned {
        url: format!(
            "https://raw.githubusercontent.com/marchaesen/vcxsrv/{VCXSRV_VERSION}/COPYING"
        ),
        sha256: "0b383d5a63da644f628d99c33976ea6487ed89aaa59f0b3257992deac1171e6b",
    }
}

fn source_url() -> String {
    format!("https://github.com/marchaesen/vcxsrv/archive/refs/tags/{VCXSRV_VERSION}.tar.gz")
}

/// What the server loads at startup: its DLL imports, xkbcomp for the keymap, the keyboard
/// data, and the misc fonts that hold the required "fixed" and "cursor" fonts. GLX uses the
/// native WGL driver (-wgl), so Mesa's software renderer stays out.
const VCXSRV_FILES: &[&str] = &[
    "vcxsrv.exe",
    "xkbcomp.exe",
    "freetype.dll",
    "libX11.dll",
    "libXau.dll",
    "libxcb.dll",
    "libcrypto-3-x64.dll",
    "zlib1.dll",
    "vcruntime140.dll",
    "xkbdata",
    "fonts/misc",
    "fonts/fonts.conf",
    "font-dirs",
    "locale",
    "XErrorDB",
    "xkeysymdb",
    "protocol.txt",
    "system.XWinrc",
];

/// Build `vendor/vcxsrv` unless it already holds this version.
pub fn prepare(root: &Path) -> Result<PathBuf, String> {
    let target = root.join("vendor").join("vcxsrv");
    let stamp = target.join(".version");
    if std::fs::read_to_string(&stamp).is_ok_and(|text| text.trim() == VCXSRV_VERSION) {
        return Ok(target);
    }

    let cache = root.join("vendor").join(".cache");
    std::fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    let installer = download(
        &installer(),
        &cache.join(format!("vcxsrv-{VCXSRV_VERSION}.installer.exe")),
    )?;
    let license = download(
        &license(),
        &cache.join(format!("vcxsrv-{VCXSRV_VERSION}.COPYING")),
    )?;
    let seven_zip = find_seven_zip()?;
    let unpacked = cache.join(format!("vcxsrv-{VCXSRV_VERSION}-unpacked"));
    let _ = std::fs::remove_dir_all(&unpacked);
    let result = (|| {
        let output = Command::new(&seven_zip)
            .arg("x")
            .arg("-y")
            .arg(format!("-o{}", unpacked.display()))
            .arg(&installer)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .map_err(|error| format!("{seven_zip}: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = match stderr.trim() {
                "" => format!("exit {}", output.status),
                text => text.to_string(),
            };
            return Err(format!(
                "7-Zip could not unpack the VcXsrv installer: {reason}"
            ));
        }
        let _ = std::fs::remove_dir_all(&target);
        for entry in VCXSRV_FILES {
            let from = unpacked.join(entry);
            if !from.exists() {
                return Err(format!(
                    "VcXsrv {VCXSRV_VERSION} has no {entry}; update VCXSRV_FILES"
                ));
            }
            copy_recursive(&from, &target.join(entry))
                .map_err(|error| format!("{}: {error}", from.display()))?;
        }
        let write = |name: &str, contents: &[u8]| {
            std::fs::write(target.join(name), contents).map_err(|error| format!("{name}: {error}"))
        };
        std::fs::copy(&license, target.join("COPYING")).map_err(|error| error.to_string())?;
        write("NOTICE.txt", notice().as_bytes())?;
        write(".version", VCXSRV_VERSION.as_bytes())
    })();
    let _ = std::fs::remove_dir_all(&unpacked);
    result.map(|()| target)
}

/// Save the upstream source archive for this version into `directory`: the GPL corresponding
/// source that travels with every package that ships the server.
pub fn save_source(directory: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    let file = directory.join(format!("vcxsrv-{VCXSRV_VERSION}-source.tar.gz"));
    fetch(&source_url(), &file)?;
    Ok(file)
}

fn notice() -> String {
    let installer = installer().url;
    let source = source_url();
    format!(
        "VcXsrv {VCXSRV_VERSION} — the X server Muxus uses for X11 forwarding on Windows.

VcXsrv is free software, distributed under the GNU General Public License
version 3 (see COPYING). Muxus runs it as a separate program and ships an
unmodified subset of the upstream build:

  {installer}

The complete corresponding source code is available from:

  {source}

and is attached to every Muxus release on GitHub.

Bundled components keep their own licenses: the X.Org server, libraries,
keyboard data and fonts (MIT/X11-style licenses), FreeType (FreeType License),
OpenSSL libcrypto (Apache License 2.0), zlib (zlib License) and the Microsoft
Visual C++ runtime (redistributable under the Visual Studio license terms).
"
    )
}

/// `file`, downloaded from `pinned.url` unless a copy with the pinned checksum is cached.
fn download(pinned: &Pinned, file: &Path) -> Result<PathBuf, String> {
    if std::fs::read(file).is_ok_and(|data| sha256(&data) == pinned.sha256) {
        return Ok(file.to_path_buf());
    }
    let partial = file.with_extension("partial");
    fetch(&pinned.url, &partial)?;
    let data = std::fs::read(&partial).map_err(|error| error.to_string())?;
    let actual = sha256(&data);
    if actual != pinned.sha256 {
        let _ = std::fs::remove_file(&partial);
        return Err(format!(
            "Checksum mismatch for {}: expected {}, got {actual}",
            pinned.url, pinned.sha256
        ));
    }
    std::fs::rename(&partial, file).map_err(|error| error.to_string())?;
    Ok(file.to_path_buf())
}

/// Download `url` into `file`. curl ships with Windows 10 and later, as with every Linux and
/// macOS; GitHub's release downloads fail now and then, so it retries.
fn fetch(url: &str, file: &Path) -> Result<(), String> {
    eprintln!("downloading {url}");
    let status = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error"])
        .args(["--retry", "3", "--retry-all-errors", "--output"])
        .arg(file)
        .arg(url)
        .status()
        .map_err(|error| format!("curl: {error}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(file);
        return Err(format!("Download failed: {url} ({status})"));
    }
    Ok(())
}

/// VcXsrv ships as an NSIS installer, which only full 7-Zip builds unpack.
fn find_seven_zip() -> Result<String, String> {
    let candidates = [
        std::env::var("MUXUS_7ZIP").ok(),
        Some("7z".into()),
        Some("7zz".into()),
        cfg!(windows).then(|| r"C:\Program Files\7-Zip\7z.exe".into()),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|candidate| {
            Command::new(candidate)
                .arg("i")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
        .ok_or_else(|| {
            "7-Zip is required to unpack VcXsrv: put 7z or 7zz on PATH, or set MUXUS_7ZIP.".into()
        })
}

fn copy_recursive(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(from, to).map(drop)
    }
}

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
