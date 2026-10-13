//! Typing a named vault secret into terminal sessions (port of
//! `server/src/security/vault-secret-input.ts`).
//!
//! The value goes from the vault straight to each writer; it is never
//! returned, logged or given to a session recorder, so only what the remote
//! side echoes can reach session history or a log file.

use zeroize::Zeroizing;

use super::{PasswordVault, VaultResult};

/// A secret as keystrokes: its text, then Enter (a carriage return, as the
/// key sends) when asked.
pub fn secret_keystrokes(value: &str, enter: bool) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(value.len() + 1));
    bytes.extend_from_slice(value.as_bytes());
    if enter {
        bytes.push(b'\r');
    }
    bytes
}

/// Type a named vault secret into terminal sessions as if it were typed.
/// Each writer returns whether its session took the input. The vault's own
/// errors (locked, wrong master password) pass through so callers can ask
/// for the master password and try again.
///
/// Returns the number of sessions that took the secret, or `None` when the
/// secret is no longer in the vault.
pub fn type_vault_secret<W>(
    vault: &PasswordVault,
    secret_id: &str,
    writers: &mut [W],
    enter: bool,
    master_password: Option<&str>,
) -> VaultResult<Option<usize>>
where
    W: FnMut(&[u8]) -> bool,
{
    let Some(value) = vault.secret_value(secret_id, master_password)? else {
        return Ok(None);
    };
    let mut typed = 0;
    for write in writers.iter_mut() {
        if write(&secret_keystrokes(&value, enter)) {
            typed += 1;
        }
    }
    Ok(Some(typed))
}
