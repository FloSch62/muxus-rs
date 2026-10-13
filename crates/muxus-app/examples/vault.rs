//! The vault screens around terminals on a sandboxed backend: the secret
//! dialog, the send-secret menu and the startup unlock prompt, with the
//! secrets and vault state the reference capture serves. Nothing touches the
//! real home directory or the keyring (the sandbox keeps the vault key in
//! memory).
//!
//! ```text
//! hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=send out.png -- \
//!     target-ui-b/debug/examples/vault
//! ```
//!
//! `MUXUS_SCENE`: `secret` (default, Add secret), `send` (the menu at
//! `MUXUS_AT=x,y`, CSS px), `unlock` (a startup-policy vault, locked).
//! `MUXUS_THEME=dark` for the dark variant.

#[path = "support/scene.rs"]
mod scene;

use muxus_app::features::vault::secret_dialog::VaultSecretDialog;
use muxus_app::features::vault::send_menu::SendSecretMenu;
use muxus_app::features::vault::startup_unlock::VaultStartupUnlock;
use muxus_core::api::{PasswordVaultSecret, PasswordVaultStatus, PasswordVaultUnlockPolicy};
use muxus_ui::gpui_kit::{AppContext, point, px};

fn secret(id: &str, name: &str, username: Option<&str>, day: u32) -> PasswordVaultSecret {
    PasswordVaultSecret {
        id: id.into(),
        name: name.into(),
        username: username.map(Into::into),
        created_at: format!("2026-07-{day}T09:00:00.000Z"),
        updated_at: format!("2026-07-{day}T09:00:00.000Z"),
    }
}

fn status(
    policy: PasswordVaultUnlockPolicy,
    locked: bool,
    secrets: Vec<PasswordVaultSecret>,
) -> PasswordVaultStatus {
    PasswordVaultStatus {
        configured: true,
        unlock_policy: Some(policy),
        locked,
        os_key_store_available: true,
        credential_count: if locked { 2 } else { 0 },
        credentials: Vec::new(),
        secrets,
    }
}

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let (backend, runtime) = scene::demo_backend(scratch.path());
    let which = scene::scene("secret");
    muxus_ui::gpui_kit::application()
        .with_assets(muxus_ui::assets::Assets)
        .run(move |cx| {
            muxus_ui::init(scene::mode(), cx);
            let backend = backend.clone();
            let which = which.clone();
            scene::open(cx, move |window, cx| match which.as_str() {
                "send" => {
                    let (x, y) = scene::at().unwrap_or((368., 125.));
                    let secrets = vec![
                        secret("secret-core", "Core enable", None, 20),
                        secret("secret-fw", "Firewall PIN", None, 21),
                        secret("secret-lab", "Lab sudo", Some("admin"), 22),
                    ];
                    cx.new(|cx| {
                        let mut menu = SendSecretMenu::new(
                            backend,
                            Some("tab-local".into()),
                            true,
                            point(px(x), px(y)),
                            window,
                            cx,
                        );
                        menu.set_status(
                            status(PasswordVaultUnlockPolicy::Never, false, secrets),
                            cx,
                        );
                        menu
                    })
                    .into()
                }
                "unlock" => cx
                    .new(|cx| {
                        let mut prompt = VaultStartupUnlock::new(backend, window, cx);
                        prompt.set_status(
                            status(PasswordVaultUnlockPolicy::Startup, true, Vec::new()),
                            cx,
                        );
                        prompt
                    })
                    .into(),
                _ => cx
                    .new(|cx| VaultSecretDialog::new(backend, None, "", window, cx))
                    .into(),
            });
        });
    drop(runtime);
}
