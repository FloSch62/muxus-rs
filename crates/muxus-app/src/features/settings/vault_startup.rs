//! Workspace restore and the startup vault prompt (`client/src/password-vault-startup.ts`).

use muxus_core::api::{PasswordVaultStatus, PasswordVaultUnlockPolicy};

/// Restored remote sessions must not dial while the startup-policy prompt is
/// unresolved. Otherwise the session and the app-level prompt both ask for the
/// same master password.
pub fn should_delay_workspace_restore_for_vault(
    status: Option<&PasswordVaultStatus>,
    pending: bool,
    failed: bool,
) -> bool {
    if failed {
        return false;
    }
    let Some(status) = status.filter(|_| !pending) else {
        return true;
    };
    status.configured
        && status.unlock_policy == Some(PasswordVaultUnlockPolicy::Startup)
        && status.locked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(patch: impl FnOnce(&mut PasswordVaultStatus)) -> PasswordVaultStatus {
        let mut status = PasswordVaultStatus {
            configured: true,
            unlock_policy: Some(PasswordVaultUnlockPolicy::Startup),
            locked: true,
            os_key_store_available: true,
            credential_count: 1,
            credentials: Vec::new(),
            secrets: Vec::new(),
        };
        patch(&mut status);
        status
    }

    #[test]
    fn delays_restored_sessions_while_the_startup_policy_vault_is_locked() {
        assert!(should_delay_workspace_restore_for_vault(
            Some(&status(|_| {})),
            false,
            false
        ));
    }

    #[test]
    fn waits_for_the_status_request_before_restoring_sessions() {
        assert!(should_delay_workspace_restore_for_vault(None, true, false));
    }

    #[test]
    fn continues_after_unlock_or_when_no_startup_prompt_applies() {
        for current in [
            status(|s| s.locked = false),
            status(|s| s.unlock_policy = Some(PasswordVaultUnlockPolicy::Credential)),
            status(|s| s.unlock_policy = Some(PasswordVaultUnlockPolicy::Never)),
            status(|s| {
                s.configured = false;
                s.unlock_policy = None;
                s.locked = false;
            }),
        ] {
            assert!(!should_delay_workspace_restore_for_vault(
                Some(&current),
                false,
                false
            ));
        }
    }

    #[test]
    fn does_not_let_a_failed_status_request_block_workspace_startup() {
        assert!(!should_delay_workspace_restore_for_vault(None, false, true));
    }
}
