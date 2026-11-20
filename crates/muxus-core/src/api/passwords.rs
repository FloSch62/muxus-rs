//! External password managers: the kinds Muxus reads from, how each is reached, and the
//! reference a host or folder keeps to one entry.

use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::validate::{self, Validate, ValidationError, join};

/// Password managers Muxus can read passwords from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PasswordManagerKind {
    Keepass,
    Onepassword,
    Bitwarden,
    Protonpass,
    Lastpass,
    Dashlane,
    Pass,
}

/// Every kind, in the order the connect dialog lists them (`PASSWORD_MANAGER_KINDS`).
pub const PASSWORD_MANAGER_KINDS: [PasswordManagerKind; 7] = [
    PasswordManagerKind::Keepass,
    PasswordManagerKind::Onepassword,
    PasswordManagerKind::Bitwarden,
    PasswordManagerKind::Protonpass,
    PasswordManagerKind::Lastpass,
    PasswordManagerKind::Dashlane,
    PasswordManagerKind::Pass,
];

impl PasswordManagerKind {
    /// The name the database and the JSON use.
    pub fn as_str(self) -> &'static str {
        match self {
            PasswordManagerKind::Keepass => "keepass",
            PasswordManagerKind::Onepassword => "onepassword",
            PasswordManagerKind::Bitwarden => "bitwarden",
            PasswordManagerKind::Protonpass => "protonpass",
            PasswordManagerKind::Lastpass => "lastpass",
            PasswordManagerKind::Dashlane => "dashlane",
            PasswordManagerKind::Pass => "pass",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        PASSWORD_MANAGER_KINDS
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }

    /// What the password manager is called (`PASSWORD_MANAGER_LABELS`).
    pub fn label(self) -> &'static str {
        match self {
            PasswordManagerKind::Keepass => "KeePass / KeePassXC",
            PasswordManagerKind::Onepassword => "1Password",
            PasswordManagerKind::Bitwarden => "Bitwarden",
            PasswordManagerKind::Protonpass => "Proton Pass",
            PasswordManagerKind::Lastpass => "LastPass",
            PasswordManagerKind::Dashlane => "Dashlane",
            PasswordManagerKind::Pass => "pass / gopass",
        }
    }
}

/// How one password manager is reached. KeePass and pass read files; the others run the
/// manager's own command-line program, which holds the sign-in.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordManagerConfig {
    /// KeePass: the .kdbx database. pass: the store directory; empty uses the default.
    pub path: Option<String>,
    /// KeePass: an optional key file used together with the database password.
    pub key_file: Option<String>,
    /// Command-line program to run when it is not on PATH. pass: the gpg program.
    pub program: Option<String>,
    /// 1Password: sign-in address or account ID when several accounts are added.
    pub account: Option<String>,
}

impl PasswordManagerConfig {
    /// Trimmed settings without the empty ones (`cleanConfig`).
    pub fn cleaned(&self) -> Self {
        let clean = |value: &Option<String>| {
            value
                .as_deref()
                .map(crate::js::js_trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Self {
            path: clean(&self.path),
            key_file: clean(&self.key_file),
            program: clean(&self.program),
            account: clean(&self.account),
        }
    }
}

/// Where a password manager stands right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PasswordManagerState {
    /// Entries can be read.
    Ready,
    /// Muxus can unlock it with the password the manager itself asks for.
    Locked,
    /// Its command-line program is not signed in; that happens outside Muxus.
    SignedOut,
    /// The database, store or program is missing.
    Unavailable,
}

/// One connected password manager and its current state.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordManagerSource {
    pub id: String,
    pub kind: PasswordManagerKind,
    pub name: String,
    pub config: PasswordManagerConfig,
    pub state: PasswordManagerState,
    /// Why it is not ready, in words for the person.
    pub message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordManagersResponse {
    pub sources: Vec<PasswordManagerSource>,
    /// Kinds whose command-line program was found on this computer.
    pub installed: Vec<PasswordManagerKind>,
}

/// What connects a password manager, or changes how a connected one is reached
/// (the body of `POST /api/password-managers` and `PUT /api/password-managers/:id`).
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordManagerInput {
    pub kind: PasswordManagerKind,
    pub name: Option<String>,
    pub config: PasswordManagerConfig,
}

/// One entry offered in the picker. The password itself never leaves the backend.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordManagerEntry {
    /// Stable within its password manager: a KeePass UUID, an item ID, a pass path.
    pub id: String,
    pub title: String,
    pub username: Option<String>,
    pub url: Option<String>,
    /// Group, vault or folder the entry sits in.
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordManagerEntriesResponse {
    pub entries: Vec<PasswordManagerEntry>,
}

/// Points a host or folder at one password-manager entry. Only the reference is stored; the
/// password is read from the manager each time it is needed.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPasswordRef {
    pub source_id: String,
    pub entry_id: String,
    /// The entry's title and user name when it was picked, for display.
    #[serde(default)]
    pub title: String,
    pub username: Option<String>,
}

impl ExternalPasswordRef {
    /// Exactly the reference fields, so nothing else rides along into the database
    /// (`cleanPasswordRef`): an empty user name is no user name.
    pub fn cleaned(&self) -> Self {
        Self {
            source_id: self.source_id.clone(),
            entry_id: self.entry_id.clone(),
            title: self.title.clone(),
            username: self.username.clone().filter(|name| !name.is_empty()),
        }
    }
}

/// `passwordRefSchema`: a password-manager entry a host or folder logs in with, never the
/// password itself.
impl Validate for ExternalPasswordRef {
    fn validate_at(&mut self, path: &str) -> Result<(), ValidationError> {
        validate::string(&join(path, "sourceId"), &self.source_id, 1, 200)?;
        validate::string(&join(path, "entryId"), &self.entry_id, 1, 2048)?;
        validate::string(&join(path, "title"), &self.title, 0, 500)?;
        validate::opt_string(&join(path, "username"), &self.username, 0, 500)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kinds_keep_the_names_the_typescript_build_stores() {
        let names: Vec<&str> = PASSWORD_MANAGER_KINDS
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "keepass",
                "onepassword",
                "bitwarden",
                "protonpass",
                "lastpass",
                "dashlane",
                "pass"
            ]
        );
        for kind in PASSWORD_MANAGER_KINDS {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(kind.as_str()));
            assert_eq!(PasswordManagerKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(PasswordManagerKind::parse("enpass"), None);
        assert_eq!(PasswordManagerKind::Keepass.label(), "KeePass / KeePassXC");
        assert_eq!(PasswordManagerKind::Pass.label(), "pass / gopass");
    }

    #[test]
    fn states_and_settings_serialize_as_the_routes_answered() {
        assert_eq!(
            serde_json::to_value(PasswordManagerState::SignedOut).unwrap(),
            json!("signed-out")
        );
        let config = PasswordManagerConfig {
            path: Some("  ~/Passwords.kdbx ".into()),
            key_file: Some("".into()),
            program: Some("   ".into()),
            account: None,
        };
        assert_eq!(
            serde_json::to_value(config.cleaned()).unwrap(),
            json!({ "path": "~/Passwords.kdbx" })
        );
        let with_key: PasswordManagerConfig =
            serde_json::from_value(json!({ "path": "a.kdbx", "keyFile": "a.keyx" })).unwrap();
        assert_eq!(with_key.key_file.as_deref(), Some("a.keyx"));
    }

    #[test]
    fn a_reference_carries_only_its_own_fields() {
        let stored: ExternalPasswordRef = serde_json::from_value(json!({
            "sourceId": "source-1",
            "entryId": "entry-1",
            "title": "web1",
            "username": "",
            "password": "must not be stored"
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(stored.cleaned()).unwrap(),
            json!({ "sourceId": "source-1", "entryId": "entry-1", "title": "web1" })
        );
        // A reference without a title, as an older row may hold, reads with an empty one.
        let untitled: ExternalPasswordRef =
            serde_json::from_value(json!({ "sourceId": "s", "entryId": "e" })).unwrap();
        assert_eq!(untitled.title, "");
        assert!(untitled.clone().validate_at("passwordRef").is_ok());
        let long = ExternalPasswordRef {
            entry_id: "x".repeat(2049),
            ..untitled.clone()
        }
        .validate_at("passwordRef")
        .unwrap_err();
        assert_eq!(long.path, "passwordRef.entryId");
        assert!(
            ExternalPasswordRef {
                source_id: String::new(),
                ..untitled
            }
            .validate_at("passwordRef")
            .is_err()
        );
    }
}
