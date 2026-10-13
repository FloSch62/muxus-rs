//! A stand-in for an installed Muxus under a scratch home, to try the development data folder
//! on: `~/.ssh/config` with the documentation's demo hosts, and `$XDG_CONFIG_HOME/Muxus` (the
//! installed app's data folder) with its saved hosts, folders, tunnels, a password vault
//! holding one folder password, and preferences.
//!
//! ```bash
//! cargo run -p muxus-app --example installed_fixture -- <scratch-root>
//! # then, with HOME=<scratch-root>/home and XDG_CONFIG_HOME=<scratch-root>/home/.config:
//! MUXUS_DEV=1 target/debug/muxus
//! ```
//!
//! The vault asks for its master password once per run ("correct horse battery"), so nothing
//! is written to an operating-system keyring.

use muxus_app::features::host_editor::demo::{DemoSandbox, seed};
use muxus_core::api::PasswordVaultUnlockPolicy;
use muxus_session::{Backend, BackendOptions};

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .expect("usage: installed_fixture <scratch-root>");
    let sandbox = DemoSandbox::create(&root).expect("create the scratch home");
    let installed = sandbox.home.join(".config").join("Muxus");
    let mut options = BackendOptions::sandbox(&installed, &sandbox.home);
    options.autostart_tunnels = false;
    let backend = Backend::start_blocking(options).expect("start the installed backend");
    futures_lite::future::block_on(async {
        seed(&backend).await.expect("seed the demo data");
        backend
            .create_vault(
                "correct horse battery",
                Some(PasswordVaultUnlockPolicy::Startup),
            )
            .await
            .expect("create the vault");
        backend
            .put_folder_password(
                "Production",
                "demo-folder-secret",
                Some("correct horse battery"),
            )
            .await
            .expect("save a folder password");
        let mut prefs = backend.prefs().await.expect("read the preferences");
        prefs.sidebar_width = 280.;
        backend
            .set_prefs(prefs)
            .await
            .expect("write the preferences");
        backend.shutdown().await;
    });
    // The demo data folder `DemoSandbox` lays out is not the installed one; drop it.
    let _ = std::fs::remove_dir_all(&sandbox.data);
    println!("{}", installed.display());
}
