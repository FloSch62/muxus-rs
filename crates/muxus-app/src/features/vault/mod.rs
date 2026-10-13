//! The password vault around terminals: typing a saved secret into a session
//! (`vault-secret-send.ts`, `SendSecretMenu.tsx`), the stand-alone secret
//! dialog (`VaultSecretDialog.tsx`) and the startup unlock prompt
//! (`PasswordVaultStartupUnlock.tsx`).
//!
//! The vault questions a session asks while it connects (`vault-unlock`,
//! `vault-create`, `vault-repair` auth prompts) are answered by the window's
//! session prompt dialog (`ui/prompts.rs`), which already handles them.

pub mod secret_dialog;
pub mod send;
pub mod send_menu;
pub mod startup_unlock;
