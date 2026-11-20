//! What every password manager offers Muxus (`provider.ts`).

use async_trait::async_trait;
use muxus_core::api::{PasswordManagerEntry, PasswordManagerKind, PasswordManagerState};
use muxus_core::collate::base_numeric_compare;

/// Why a password manager could not do what was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PasswordManagerProblem {
    /// The database, store or program is missing.
    Unavailable,
    /// Needs the manager's own password, which Muxus can ask for.
    Locked,
    /// The program is not signed in; that happens outside Muxus.
    SignedOut,
    /// The password given to unlock it was wrong.
    InvalidUnlock,
    /// The entry is no longer in the password manager.
    EntryMissing,
    /// The program did not answer in time, usually an approval prompt left open.
    Timeout,
    Failed,
}

impl PasswordManagerProblem {
    /// The name the routes report (`password-manager-<name>`).
    pub fn as_str(self) -> &'static str {
        match self {
            PasswordManagerProblem::Unavailable => "unavailable",
            PasswordManagerProblem::Locked => "locked",
            PasswordManagerProblem::SignedOut => "signed-out",
            PasswordManagerProblem::InvalidUnlock => "invalid-unlock",
            PasswordManagerProblem::EntryMissing => "entry-missing",
            PasswordManagerProblem::Timeout => "timeout",
            PasswordManagerProblem::Failed => "failed",
        }
    }
}

/// A problem with words for the person.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PasswordManagerError {
    pub problem: PasswordManagerProblem,
    pub message: String,
}

impl PasswordManagerError {
    pub fn new(problem: PasswordManagerProblem, message: impl Into<String>) -> Self {
        Self {
            problem,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(PasswordManagerProblem::Unavailable, message)
    }

    pub fn locked(message: impl Into<String>) -> Self {
        Self::new(PasswordManagerProblem::Locked, message)
    }

    pub fn signed_out(message: impl Into<String>) -> Self {
        Self::new(PasswordManagerProblem::SignedOut, message)
    }

    pub fn invalid_unlock(message: impl Into<String>) -> Self {
        Self::new(PasswordManagerProblem::InvalidUnlock, message)
    }

    pub fn entry_missing(message: impl Into<String>) -> Self {
        Self::new(PasswordManagerProblem::EntryMissing, message)
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(PasswordManagerProblem::Failed, message)
    }
}

pub type PasswordManagerResult<T> = Result<T, PasswordManagerError>;

/// Where a password manager stands, and why when it is not ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub state: PasswordManagerState,
    pub message: Option<String>,
}

impl ProviderStatus {
    pub fn ready() -> Self {
        Self {
            state: PasswordManagerState::Ready,
            message: None,
        }
    }

    pub fn new(state: PasswordManagerState, message: impl Into<String>) -> Self {
        Self {
            state,
            message: Some(message.into()),
        }
    }
}

/// One password manager. A provider only reads: it never writes to the manager, and a
/// password it returns is handed straight to the login that asked for it. Providers keep
/// whatever unlocks the manager in memory only.
#[async_trait]
pub trait PasswordManagerProvider: Send + Sync {
    fn kind(&self) -> PasswordManagerKind;

    /// Cheap and silent: must never raise an approval or unlock prompt.
    async fn status(&self) -> PasswordManagerResult<ProviderStatus>;

    /// Whether Muxus can unlock the manager with a password it collects.
    fn can_unlock(&self) -> bool {
        false
    }

    /// Unlock with the manager's own password. Only where [`Self::can_unlock`] says so.
    async fn unlock(&self, _password: &str) -> PasswordManagerResult<()> {
        Err(PasswordManagerError::failed(
            "This password manager is unlocked in its own app, not in Muxus.",
        ))
    }

    /// Forget whatever unlocked the manager.
    async fn lock(&self) -> PasswordManagerResult<()> {
        Ok(())
    }

    /// Pull changes from the manager's server before the next listing.
    async fn sync(&self) -> PasswordManagerResult<()> {
        Ok(())
    }

    /// Every entry that can hold a login password.
    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>>;

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String>;

    /// The provider is going away: drop what it holds in memory.
    fn dispose(&self) {}
}

/// Case- and accent-insensitive order for picker entries: group, then title, then user name.
pub fn sort_entries(mut entries: Vec<PasswordManagerEntry>) -> Vec<PasswordManagerEntry> {
    entries.sort_by(|a, b| {
        base_numeric_compare(
            a.group.as_deref().unwrap_or(""),
            b.group.as_deref().unwrap_or(""),
        )
        .then_with(|| base_numeric_compare(&a.title, &b.title))
        .then_with(|| {
            base_numeric_compare(
                a.username.as_deref().unwrap_or(""),
                b.username.as_deref().unwrap_or(""),
            )
        })
    });
    entries
}

/// A password as the programs print it: without the line ending they add.
pub fn without_line_ending(output: &str) -> &str {
    let Some(text) = output.strip_suffix('\n') else {
        return output;
    };
    text.strip_suffix('\r').unwrap_or(text)
}

/// A non-empty string, else nothing (`value || undefined`).
pub(crate) fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_owned)
}

/// A trimmed, non-empty string, else nothing (`value?.trim() || undefined`).
pub(crate) fn trimmed(value: Option<&str>) -> Option<String> {
    non_empty(value.map(muxus_core::js::js_trim))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(group: Option<&str>, title: &str, username: Option<&str>) -> PasswordManagerEntry {
        PasswordManagerEntry {
            id: title.to_owned(),
            title: title.to_owned(),
            username: username.map(str::to_owned),
            group: group.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn orders_entries_by_group_title_and_user_name_without_case_or_accents() {
        let sorted = sort_entries(vec![
            entry(Some("Servers / Lab"), "web10", None),
            entry(Some("servers / lab"), "Web2", None),
            entry(None, "zeta", None),
            entry(Some("Network"), "core", Some("ops")),
            entry(Some("Network"), "Core", Some("admin")),
        ]);
        let titles: Vec<(&str, Option<&str>)> = sorted
            .iter()
            .map(|entry| (entry.title.as_str(), entry.username.as_deref()))
            .collect();
        assert_eq!(
            titles,
            [
                ("zeta", None),
                ("Core", Some("admin")),
                ("core", Some("ops")),
                ("Web2", None),
                ("web10", None),
            ]
        );
    }

    #[test]
    fn drops_one_line_ending_and_nothing_else() {
        assert_eq!(without_line_ending("secret\n"), "secret");
        assert_eq!(without_line_ending("secret\r\n"), "secret");
        assert_eq!(without_line_ending("secret \n\n"), "secret \n");
        assert_eq!(without_line_ending(" secret "), " secret ");
        assert_eq!(without_line_ending("secret\r"), "secret\r");
    }

    #[test]
    fn names_problems_as_the_routes_report_them() {
        assert_eq!(PasswordManagerProblem::SignedOut.as_str(), "signed-out");
        assert_eq!(
            PasswordManagerProblem::InvalidUnlock.as_str(),
            "invalid-unlock"
        );
        assert_eq!(
            PasswordManagerError::entry_missing("gone").to_string(),
            "gone"
        );
    }
}
