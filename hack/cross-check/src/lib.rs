//! The Windows and macOS code of crates that build C, compiled on its own so
//! `cargo clippy --target x86_64-pc-windows-gnu` / `x86_64-apple-darwin` reaches it from Linux
//! (`hack/cross-check.sh`). Each module is the crate's own file; the few items they reach
//! through `super::` and `crate::` have stand-ins here, which is why every module sits at the
//! root.
#![allow(dead_code)]

/// `muxus_store::vault::key_store`: the OS credential store (Keychain, Credential Manager).
#[path = "../../../crates/muxus-store/src/vault/key_store.rs"]
pub mod key_store;

/// `muxus_store::vault::crypto`, for `key_store`.
pub mod crypto {
    pub const KEY_BYTES: usize = 32;
}

/// `muxus_store::ids`, for `key_store`'s tests.
pub mod ids {
    pub fn random_bytes(bytes: &mut [u8]) {
        bytes.fill(7);
    }
}

/// `muxus_store::history::disk`: free space of the history root (`GetDiskFreeSpaceExW`).
#[path = "../../../crates/muxus-store/src/history/disk.rs"]
pub mod disk;

/// `muxus_app::features::desktop::events`, which `single_instance` delivers.
#[path = "../../../crates/muxus-app/src/features/desktop/events.rs"]
pub mod events;

/// `muxus_app::features::desktop::single_instance`: one instance per data folder (a named pipe
/// on Windows).
#[path = "../../../crates/muxus-app/src/features/desktop/single_instance.rs"]
pub mod single_instance;

/// `muxus_app::features::desktop::launch`, named in `events`' documentation.
pub mod launch {
    pub fn plan_launch() {}
}

/// `muxus_session::stats`: the local host stats (the Windows reading, the macOS CPU time).
#[path = "../../../crates/muxus-session/src/stats/mod.rs"]
pub mod stats;

/// `muxus_session::diagnostics`: connection diagnostics (the Windows ping and agent pipe probes).
#[path = "../../../crates/muxus-session/src/diagnostics/mod.rs"]
pub mod diagnostics;

/// `muxus_session::updater::install`: the installer hand-off (detached on Windows).
#[path = "../../../crates/muxus-session/src/updater/install.rs"]
pub mod install;

/// `muxus_session::links`, for `install`.
pub mod links {
    use std::collections::HashMap;

    pub fn without_app_image_paths(env: &HashMap<String, String>) -> HashMap<String, String> {
        env.clone()
    }
}
