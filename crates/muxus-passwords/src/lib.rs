//! Login passwords read from external password managers (port of
//! `server/src/password-managers`).
//!
//! KeePass databases and the `pass` store are read from their files; 1Password, Bitwarden,
//! Proton Pass, LastPass and Dashlane through the command-line program each ships, which
//! holds the sign-in. Nothing here writes to a manager, and a password is handed straight
//! to the login that asked for it: hosts and folders store a reference to an entry only.

mod bitwarden;
mod cli;
mod dashlane;
mod host;
mod keepass;
mod lastpass;
mod managers;
mod onepassword;
mod pass;
mod protonpass;
mod provider;

pub use bitwarden::BitwardenProvider;
pub use cli::{
    APPROVAL_TIMEOUT, CliOptions, CliResult, LIST_TIMEOUT, STATUS_TIMEOUT, cli_message,
    program_installed, resolve_program, run_cli,
};
pub use dashlane::DashlaneProvider;
pub use host::HostEnv;
pub use keepass::KeePassProvider;
pub use lastpass::LastPassProvider;
pub use managers::{
    PasswordManagerIo, PasswordManagers, PasswordManagersError, PromptCancelled, ProviderFactory,
    SourceStore, StoredSource, default_provider,
};
pub use onepassword::OnePasswordProvider;
pub use pass::PassProvider;
pub use protonpass::ProtonPassProvider;
pub use provider::{
    PasswordManagerError, PasswordManagerProblem, PasswordManagerProvider, PasswordManagerResult,
    ProviderStatus, sort_entries, without_line_ending,
};
