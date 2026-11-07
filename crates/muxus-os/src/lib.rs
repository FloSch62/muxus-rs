//! Calls into Windows and macOS that the rest of the workspace cannot check from Linux.
//!
//! Most crates build C (SQLite, zstd, ring, the GPU shaders) and so cannot be checked for
//! another target without its C toolchain. This one only binds system libraries
//! (`windows`, `windows-sys`, `libc`), so `cargo check --target x86_64-pc-windows-gnu` and
//! `--target x86_64-apple-darwin` cover its platform code from any host.
//!
//! - [`jump_list`]: the Windows taskbar jump list (`app.setJumpList`,
//!   `app.getJumpListSettings` in Electron).
//! - [`machine`]: readings of this computer that Node's `os` module gave the TypeScript
//!   (`os.cpus()`, and on Windows the whole status bar sample).
//!
//! Pure helpers are compiled everywhere and tested on every host; only the system calls are
//! behind `cfg`.

pub mod jump_list;
pub mod machine;
