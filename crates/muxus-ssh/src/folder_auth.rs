//! Folder-inherited SSH defaults (Termius-style shared credentials). A host's folder chain
//! collapses into one set of ssh_config option lines that resolve as the lowest-priority layer,
//! below everything the user wrote in ssh_config.

use std::collections::HashMap;

use base64::Engine;

use crate::config::OptionLine;
use crate::folder_paths::folder_chain;
use crate::types::FolderAuthSettings;

/// A folder password candidate in the encrypted vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPasswordRef {
    pub account: String,
    pub label: String,
}

/// Folder-inherited connection defaults for one dial target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderAuthDefaults {
    /// Lowest-priority ssh_config option lines; see `resolve_host`'s `fallback`.
    pub option_lines: Vec<OptionLine>,
    /// Vault password candidates, nearest folder first.
    pub passwords: Vec<FolderPasswordRef>,
}

/// Per-alias (or per saved-profile id) folder defaults.
pub type FolderAuthLookup = Box<dyn Fn(&str) -> Option<FolderAuthDefaults> + Send + Sync>;

/// One folder's stored settings row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSettingsRow {
    pub id: String,
    pub path: String,
    pub auth: FolderAuthSettings,
}

/// What the resolver needs to know; the store implements it.
pub trait FolderAuthSource: Send + Sync {
    fn group_for_alias(&self, alias: &str) -> Option<String>;
    fn folder_settings_for_path(&self, path: &str) -> Option<FolderSettingsRow>;
    /// The folder of a Muxus-owned profile, by its database ID.
    fn group_for_saved_host(&self, _id: &str) -> Option<String> {
        None
    }
}

/// Vault account of a folder's shared password (`password-vault.ts`): base64url of the JSON
/// array `["folder", id]`.
pub fn folder_password_account(folder_settings_id: &str) -> String {
    let json = serde_json::to_string(&["folder", folder_settings_id]).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub fn folder_password_label(path: &str) -> String {
    format!("Folder {path}")
}

/// Vault account of a host-saved SSH password: base64url of `[user, host lowercased, port]`.
pub fn ssh_password_account(user: &str, host: &str, port: u16) -> String {
    let json = serde_json::to_string(&(user, host.to_lowercase(), port)).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub fn ssh_password_label(user: &str, host: &str, port: u16) -> String {
    format!("{user}@{host}:{port}")
}

/// Per-alias folder defaults for the dial path: the host's folder chain (nearest first)
/// collapses to one set of option lines, and every folder with settings contributes a vault
/// password candidate.
pub fn folder_auth_resolver<S: FolderAuthSource + 'static>(
    source: std::sync::Arc<S>,
) -> FolderAuthLookup {
    Box::new(move |alias| folder_auth_for_group(&*source, source.group_for_alias(alias).as_deref()))
}

/// [`folder_auth_resolver`] for resolving many aliases at once, as the host list does: hosts
/// share folders, so each folder chain is read once. It caches, so make one per listing; folder
/// settings can change between requests.
pub fn batch_folder_auth_resolver<'a, S: FolderAuthSource + ?Sized>(
    source: &'a S,
) -> impl FnMut(&str) -> Option<FolderAuthDefaults> + 'a {
    let mut by_group: HashMap<String, Option<FolderAuthDefaults>> = HashMap::new();
    move |alias| {
        let group = source.group_for_alias(alias)?;
        by_group
            .entry(group.clone())
            .or_insert_with(|| folder_auth_for_group(source, Some(&group)))
            .clone()
    }
}

/// Folder defaults for a Muxus-owned profile, keyed by its stable database ID.
pub fn saved_profile_folder_auth_resolver<S: FolderAuthSource + 'static>(
    source: std::sync::Arc<S>,
) -> FolderAuthLookup {
    Box::new(move |id| folder_auth_for_group(&*source, source.group_for_saved_host(id).as_deref()))
}

/// The collapsed defaults for one folder path.
pub fn folder_auth_for_group<S: FolderAuthSource + ?Sized>(
    source: &S,
    group: Option<&str>,
) -> Option<FolderAuthDefaults> {
    let group = group.filter(|g| !g.is_empty())?;
    let rows: Vec<FolderSettingsRow> = folder_chain(group)
        .iter()
        .filter_map(|path| source.folder_settings_for_path(path))
        .collect();
    if rows.is_empty() {
        return None;
    }
    let merged = merge_folder_auth(rows.iter().map(|row| &row.auth));
    Some(FolderAuthDefaults {
        option_lines: folder_auth_option_lines(&merged),
        passwords: rows
            .iter()
            .map(|row| FolderPasswordRef {
                account: folder_password_account(&row.id),
                label: folder_password_label(&row.path),
            })
            .collect(),
    })
}

/// Collapse a folder chain (nearest first) so the nearest folder wins per field.
pub fn merge_folder_auth<'a>(
    chain: impl IntoIterator<Item = &'a FolderAuthSettings>,
) -> FolderAuthSettings {
    let mut merged = FolderAuthSettings::default();
    for auth in chain {
        if merged.user.is_none() {
            merged.user = auth.user.clone();
        }
        if merged.port.is_none() {
            merged.port = auth.port;
        }
        if merged.identity_files.is_none() {
            merged.identity_files = auth.identity_files.clone();
        }
        if merged.identities_only.is_none() {
            merged.identities_only = auth.identities_only;
        }
        if merged.identity_agent.is_none() {
            merged.identity_agent = auth.identity_agent.clone();
        }
        if merged.forward_agent.is_none() {
            merged.forward_agent = auth.forward_agent;
        }
    }
    merged
}

/// Render folder defaults as the option lines `resolve_host` consumes.
pub fn folder_auth_option_lines(auth: &FolderAuthSettings) -> Vec<OptionLine> {
    let mut lines = Vec::new();
    let mut push = |keyword: &str, raw: String| {
        // Whitespace and apostrophes need quoting to remain one OpenSSH token.
        let value = if raw.chars().any(|c| c.is_whitespace() || c == '\'') {
            format!("\"{raw}\"")
        } else {
            raw.clone()
        };
        lines.push(OptionLine {
            keyword: keyword.to_string(),
            key: keyword.to_ascii_lowercase(),
            value,
            args: vec![raw],
            line: None,
        });
    };
    if let Some(user) = auth.user.as_ref().filter(|u| !u.is_empty()) {
        push("User", user.clone());
    }
    if let Some(port) = auth.port {
        push("Port", port.to_string());
    }
    for file in auth.identity_files.iter().flatten() {
        push("IdentityFile", file.clone());
    }
    if let Some(only) = auth.identities_only {
        push(
            "IdentitiesOnly",
            if only { "yes" } else { "no" }.to_string(),
        );
    }
    if let Some(agent) = auth.identity_agent.as_ref().filter(|a| !a.is_empty()) {
        push("IdentityAgent", agent.clone());
    }
    if let Some(forward) = auth.forward_agent {
        push(
            "ForwardAgent",
            if forward { "yes" } else { "no" }.to_string(),
        );
    }
    lines
}

#[cfg(test)]
mod tests;
