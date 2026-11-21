//! Writing a sidebar folder as a session list for others to follow
//! (`client/src/shared-sessions-export.ts`, `client/src/shared-sessions-format.ts` and
//! `client/src/folder-defaults.ts`).

use std::collections::HashMap;

use muxus_core::api::{
    FolderAuthSettings, FolderSettingsRecord, SavedHostProfile, SshHostEntry, saved_host_hop,
    saved_host_hop_id,
};
use muxus_core::folder_paths::{
    folder_label, folder_path_key, folder_path_segments, is_descendant_folder_path,
};
use muxus_core::js;
use muxus_core::protocol::SavedHostSessionProfile;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use serde_with::skip_serializing_none;

use crate::launch::ManagedHost;
use crate::shared_sessions::{SHARED_SESSIONS_FORMAT, SHARED_SESSIONS_VERSION};

/// Where the jump hosts a list needs from outside its folder are put.
pub const SHARED_JUMP_HOST_FOLDER: &str = "Jump hosts";

/// Settings a list never carries: they run on, or weaken a check of, whoever follows it.
const NEVER_SHARED: [&str; 5] = [
    "profileId",
    "proxyCommand",
    "forwardAgent",
    "strictHostKeyChecking",
    "environment",
];
const USER_FIELDS: [&str; 2] = ["user", "username"];
const KEY_PATH_FIELDS: [&str; 7] = [
    "identityFiles",
    "certificateFiles",
    "identitiesOnly",
    "identityAgent",
    "caFile",
    "certFile",
    "keyFile",
];

/// One host of a list. Nothing in it is a secret.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSessionsDocumentHost {
    /// Unique inside the list; jump hosts and gateways are named by it.
    pub id: String,
    pub name: String,
    /// Folder inside the folder the list is followed into.
    pub folder: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub profile: Map<String, Value>,
}

/// A session list Muxus writes for others to follow.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedSessionsDocument {
    pub format: String,
    pub version: u32,
    pub name: String,
    pub created_at: String,
    pub hosts: Vec<SharedSessionsDocumentHost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareFolderOptions {
    /// Keep the user each host logs in as; without it everyone is asked for theirs.
    pub include_users: bool,
    /// Keep where keys and certificates are on this computer.
    pub include_key_paths: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedFolder {
    pub document: SharedSessionsDocument,
    /// Hosts in the folder and its subfolders.
    pub host_count: usize,
    /// Hosts from outside the folder that those are reached through, included with them.
    pub jump_host_count: usize,
}

/// Materialize the non-secret defaults a saved profile inherits in Muxus, nearest folder
/// first, and whether any of those folders has a password (`savedProfileFolderDefaults`).
pub fn saved_profile_folder_defaults(
    group: Option<&str>,
    folders: &[FolderSettingsRecord],
) -> (FolderAuthSettings, bool) {
    let normalized = group.unwrap_or_default().replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .map(js::js_trim)
        .filter(|part| !part.is_empty())
        .collect();
    let mut auth = FolderAuthSettings::default();
    let mut has_password = false;
    if parts.is_empty() {
        return (auth, has_password);
    }
    let by_path: HashMap<String, &FolderSettingsRecord> = folders
        .iter()
        .map(|folder| (folder.path.to_lowercase(), folder))
        .collect();
    for length in (1..=parts.len()).rev() {
        let Some(folder) = by_path.get(&parts[..length].join("/").to_lowercase()) else {
            continue;
        };
        has_password |= folder.has_password;
        let from = &folder.auth;
        auth.user = auth.user.or_else(|| from.user.clone());
        auth.port = auth.port.or(from.port);
        auth.identity_files = auth.identity_files.or_else(|| from.identity_files.clone());
        auth.identities_only = auth.identities_only.or(from.identities_only);
        auth.identity_agent = auth.identity_agent.or_else(|| from.identity_agent.clone());
        auth.forward_agent = auth.forward_agent.or(from.forward_agent);
    }
    (auth, has_password)
}

/// A folder of the sidebar as a session list for others: its hosts and those of its
/// subfolders, with the folders they sit in. A host from elsewhere that one of them jumps
/// through comes along, or the list would not connect. ssh_config hosts are written with
/// the settings they resolve to, since a follower has no such config. `created_at` is the
/// time the list says it was written at.
pub fn share_folder<'a>(
    folder: &str,
    hosts: &[ManagedHost<'a>],
    folder_settings: &[FolderSettingsRecord],
    options: ShareFolderOptions,
    created_at: &str,
) -> SharedFolder {
    let mut by_alias: HashMap<&str, ManagedHost<'a>> = HashMap::new();
    let mut by_profile_id: HashMap<&str, ManagedHost<'a>> = HashMap::new();
    for host in hosts {
        match host {
            ManagedHost::Profile(entry) => {
                by_profile_id.insert(entry.id.as_str(), *host);
            }
            ManagedHost::Ssh(entry) => {
                for alias in &entry.aliases {
                    by_alias.entry(alias.as_str()).or_insert(*host);
                }
            }
        }
    }

    // Hosts to write, in order; `true` marks one that only comes along as a jump host.
    let mut pending: Vec<(ManagedHost<'a>, bool)> = hosts
        .iter()
        .filter(|host| is_in_folder(host_group(host), folder))
        .map(|host| (*host, false))
        .collect();
    let host_count = pending.len();
    let mut listed: Vec<SharedSessionsDocumentHost> = Vec::new();

    let mut index = 0;
    while index < pending.len() {
        let (host, jump_host) = pending[index];
        index += 1;
        let mut profile = shared_profile(&host, folder_settings);

        // The list's id for a host another one goes through, queueing it when it is not
        // listed yet.
        let mut through = |host: Option<&ManagedHost<'a>>| -> Option<String> {
            let host = host?;
            let id = host_id(host);
            if !pending.iter().any(|(queued, _)| host_id(queued) == id) {
                pending.push((*host, true));
            }
            Some(id)
        };
        let mut hop = |value: &Value| -> Value {
            let Some(text) = value.as_str() else {
                return value.clone();
            };
            let host = match saved_host_hop_id(text) {
                Some(profile_id) => by_profile_id.get(profile_id),
                None => by_alias.get(text),
            };
            match through(host) {
                Some(id) => Value::from(saved_host_hop(&id)),
                None => value.clone(),
            }
        };
        if let Some(route) = profile.get("proxyJump").and_then(Value::as_array) {
            let placed = route.iter().map(&mut hop).collect();
            profile.insert("proxyJump".into(), Value::Array(placed));
        }
        if let Some(routes) = profile.get("proxyJumpFallbacks").and_then(Value::as_array) {
            let placed = routes
                .iter()
                .map(|route| match route.as_array() {
                    Some(route) => Value::Array(route.iter().map(&mut hop).collect()),
                    None => route.clone(),
                })
                .collect();
            profile.insert("proxyJumpFallbacks".into(), Value::Array(placed));
        }
        let mut gateway = |value: &Value| -> Value {
            let Some(target) = value.get("target").and_then(Value::as_str) else {
                return value.clone();
            };
            let host = match value.get("profileId").and_then(Value::as_str) {
                Some(profile_id) => by_profile_id.get(profile_id),
                None => by_alias.get(target),
            };
            let (Some(id), Some(host)) = (through(host), host) else {
                return value.clone();
            };
            let mut placed = Map::new();
            placed.insert("target".into(), Value::from(host_address(host)));
            placed.insert("profileId".into(), Value::from(id));
            Value::Object(placed)
        };
        if let Some(value) = profile.get("sshGateway") {
            let placed = gateway(value);
            profile.insert("sshGateway".into(), placed);
        }
        if let Some(gateways) = profile.get("sshGatewayFallbacks").and_then(Value::as_array) {
            let placed = gateways.iter().map(&mut gateway).collect();
            profile.insert("sshGatewayFallbacks".into(), Value::Array(placed));
        }
        for field in NEVER_SHARED {
            profile.shift_remove(field);
        }
        if !options.include_users {
            for field in USER_FIELDS {
                profile.shift_remove(field);
            }
        }
        if !options.include_key_paths {
            for field in KEY_PATH_FIELDS {
                profile.shift_remove(field);
            }
        }

        let inside = if jump_host {
            SHARED_JUMP_HOST_FOLDER.to_owned()
        } else {
            let group = host_group(&host).unwrap_or_default();
            folder_path_segments(group)
                .get(folder_path_segments(folder).len()..)
                .unwrap_or_default()
                .join("/")
        };
        let non_empty =
            |value: Option<&str>| value.filter(|value| !value.is_empty()).map(str::to_owned);
        listed.push(SharedSessionsDocumentHost {
            id: host_id(&host),
            name: host.display_name().to_owned(),
            folder: (!inside.is_empty()).then_some(inside),
            color: non_empty(host.color()),
            icon: non_empty(host_icon(&host)),
            profile,
        });
    }

    SharedFolder {
        host_count,
        jump_host_count: pending.len() - host_count,
        document: SharedSessionsDocument {
            format: SHARED_SESSIONS_FORMAT.to_owned(),
            version: SHARED_SESSIONS_VERSION,
            name: folder_label(folder),
            created_at: created_at.to_owned(),
            hosts: listed,
        },
    }
}

/// "Team lab" → `team-lab.muxus-sessions.json`.
pub fn shared_sessions_filename(folder: &str) -> String {
    let mut slug = String::new();
    let mut in_run = false;
    for c in folder_label(folder).to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
            in_run = false;
        } else if !in_run {
            slug.push('-');
            in_run = true;
        }
    }
    let slug = js::utf16_prefix(slug.trim_matches('-'), 60);
    format!(
        "{}.muxus-sessions.json",
        if slug.is_empty() { "shared" } else { slug }
    )
}

fn is_in_folder(group: Option<&str>, folder: &str) -> bool {
    group.is_some_and(|group| {
        !group.is_empty()
            && (folder_path_key(group) == folder_path_key(folder)
                || is_descendant_folder_path(group, folder))
    })
}

fn host_group<'a>(host: &ManagedHost<'a>) -> Option<&'a str> {
    match host {
        ManagedHost::Ssh(entry) => entry.metadata.as_ref()?.group.as_deref(),
        ManagedHost::Profile(entry) => entry.metadata.group.as_deref(),
    }
}

fn host_icon<'a>(host: &ManagedHost<'a>) -> Option<&'a str> {
    match host {
        ManagedHost::Ssh(entry) => entry.metadata.as_ref()?.icon.as_deref(),
        ManagedHost::Profile(entry) => entry.metadata.icon.as_deref(),
    }
}

fn host_id(host: &ManagedHost<'_>) -> String {
    match host {
        ManagedHost::Profile(entry) => entry.id.clone(),
        ManagedHost::Ssh(entry) => format!("ssh:{}", entry.alias),
    }
}

/// What a gateway dials: the host's address, not a name only this computer knows.
fn host_address(host: &ManagedHost<'_>) -> String {
    match host {
        ManagedHost::Ssh(entry) => resolved_hostname(entry).to_owned(),
        ManagedHost::Profile(entry) => match &entry.profile {
            SavedHostSessionProfile::Ssh(profile) => profile.target.clone(),
            _ => entry.name.clone(),
        },
    }
}

fn resolved_hostname(entry: &SshHostEntry) -> &str {
    if entry.resolved.hostname.is_empty() {
        &entry.alias
    } else {
        &entry.resolved.hostname
    }
}

fn set<T: Serialize>(profile: &mut Map<String, Value>, key: &str, value: Option<T>) {
    if let Some(value) = value
        && let Ok(value) = serde_json::to_value(value)
    {
        profile.insert(key.to_owned(), value);
    }
}

/// A host's connection settings as they apply here, folder defaults included.
fn shared_profile(
    host: &ManagedHost<'_>,
    folder_settings: &[FolderSettingsRecord],
) -> Map<String, Value> {
    let (inherited, _) = saved_profile_folder_defaults(host_group(host), folder_settings);
    match host {
        ManagedHost::Profile(entry) => saved_profile(entry, &inherited),
        ManagedHost::Ssh(entry) => {
            let resolved = &entry.resolved;
            let mut profile = Map::new();
            profile.insert("kind".into(), Value::from("ssh"));
            profile.insert("target".into(), Value::from(resolved_hostname(entry)));
            profile.insert("useConfig".into(), Value::from(false));
            set(
                &mut profile,
                "user",
                resolved.user.clone().or(inherited.user),
            );
            set(
                &mut profile,
                "port",
                if resolved.port == 22 {
                    inherited.port
                } else {
                    Some(resolved.port)
                },
            );
            set(
                &mut profile,
                "identityFiles",
                if resolved.identity_files.is_empty() {
                    inherited.identity_files
                } else {
                    Some(resolved.identity_files.clone())
                },
            );
            set(
                &mut profile,
                "certificateFiles",
                (!resolved.certificate_files.is_empty()).then_some(&resolved.certificate_files),
            );
            set(
                &mut profile,
                "identitiesOnly",
                (resolved.identities_only || inherited.identities_only == Some(true))
                    .then_some(true),
            );
            set(
                &mut profile,
                "identityAgent",
                resolved.identity_agent.clone().or(inherited.identity_agent),
            );
            set(&mut profile, "forwardX11", resolved.forward_x11);
            set(
                &mut profile,
                "proxyJump",
                (!resolved.proxy_jump.is_empty()).then_some(&resolved.proxy_jump),
            );
            set(
                &mut profile,
                "forwards",
                (!resolved.forwards.is_empty()).then_some(&resolved.forwards),
            );
            set(
                &mut profile,
                "passwordOnly",
                resolved.password_only.then_some(true),
            );
            set(
                &mut profile,
                "remoteCommand",
                entry.options.remote_command.as_ref(),
            );
            set(
                &mut profile,
                "requestTty",
                entry.options.request_tty.as_ref(),
            );
            profile
        }
    }
}

fn saved_profile(entry: &SavedHostProfile, inherited: &FolderAuthSettings) -> Map<String, Value> {
    let mut profile = match serde_json::to_value(&entry.profile) {
        Ok(Value::Object(profile)) => profile,
        _ => Map::new(),
    };
    if matches!(entry.profile, SavedHostSessionProfile::Ssh(_)) {
        let mut inherit = |key: &str, value: Option<Value>| {
            let unset = profile.get(key).is_none_or(Value::is_null);
            if let (true, Some(value)) = (unset, value) {
                profile.insert(key.to_owned(), value);
            }
        };
        inherit("user", inherited.user.clone().map(Value::from));
        inherit("port", inherited.port.map(Value::from));
        inherit(
            "identityFiles",
            inherited.identity_files.clone().map(Value::from),
        );
        inherit("identitiesOnly", inherited.identities_only.map(Value::from));
        inherit(
            "identityAgent",
            inherited.identity_agent.clone().map(Value::from),
        );
    }
    profile
}
