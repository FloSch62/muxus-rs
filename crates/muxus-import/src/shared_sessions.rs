//! Reading the text behind a shared sessions link (`client/src/shared-sessions.ts`).
//!
//! Three formats are read: MobaXterm's session file (`.mxtsessions`, as its shared sessions
//! use), the session list Muxus writes when a folder is shared ([`crate::share_folder`]),
//! and a Muxus backup, of which only the hosts are used.
//!
//! Host IDs are derived from the link and the entry, never taken from the list: the same
//! entry keeps its host across checks, and a list cannot address a host it did not bring.
//! The hosts come out as the list wrote them. Whoever stores them checks every field and
//! drops what a list may not set.

use std::collections::{HashMap, HashSet};

use muxus_core::api::{MAX_SHARED_SESSION_HOSTS, saved_host_hop, saved_host_hop_id};
use muxus_core::js::{js_trim, utf16_prefix};
use serde::Serialize;
use serde_json::{Map, Value};
use serde_with::skip_serializing_none;

use crate::mobaxterm::parse_mobaxterm_sessions;
use crate::session::{ImportError, ImportedSshStorage, imported_connections};
use crate::text::{clean_import_name, normalize_import_folder, stable_import_id};

/// A session list Muxus writes for others to follow.
pub const SHARED_SESSIONS_FORMAT: &str = "muxus-shared-sessions";
pub const SHARED_SESSIONS_VERSION: u32 = 1;
/// The format of a Muxus backup (`BACKUP_FORMAT` of the data transfer).
const BACKUP_FORMAT: &str = "muxus-backup";

const MAX_FOLDER_LENGTH: usize = 300;
const SAVED_KINDS: [&str; 8] = [
    "ssh", "telnet", "serial", "rdp", "vnc", "gnmi", "netconf", "web",
];
/// Where the SSH gateways of a MobaXterm list go, inside the link's folder.
const JUMP_HOST_FOLDER: &str = "Jump hosts";
/// ssh_config options a Muxus-owned SSH host stores under the same name.
const SSH_OPTION_KEYS: [&str; 12] = [
    "user",
    "port",
    "identityFiles",
    "certificateFiles",
    "identitiesOnly",
    "identityAgent",
    "forwardX11",
    "proxyJump",
    "forwards",
    "passwordOnly",
    "remoteCommand",
    "requestTty",
];

/// What a list was written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedSessionsFormat {
    MobaXterm,
    Muxus,
}

/// One host of a list, placed in the link's folder: the body of one entry the backend
/// takes to apply.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedListHost {
    pub id: String,
    pub name: String,
    /// Connection settings as the list wrote them, with its references to other hosts of
    /// the list pointed at the hosts they became.
    pub profile: Map<String, Value>,
    /// Full sidebar folder path.
    pub group: Option<String>,
    pub color: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SharedSessionsList {
    pub format: SharedSessionsFormat,
    pub hosts: Vec<SharedListHost>,
    /// Entries left out: protocols Muxus does not import, or incomplete ones.
    pub skipped: usize,
}

/// A host as the list names it, before it gets its place in Muxus.
struct ListedHost {
    /// Identity inside the list; other entries refer to it as a jump host.
    key: String,
    name: String,
    profile: Map<String, Value>,
    look: Look,
}

/// Folder, color and icon are all a list decides about how a host looks.
#[derive(Default)]
struct Look {
    folder: Option<String>,
    color: Option<String>,
    icon: Option<String>,
}

/// An ssh_config alias of a list: the key of the host it became and the address it stood for.
struct Alias {
    key: String,
    target: String,
}

struct Listed {
    format: SharedSessionsFormat,
    hosts: Vec<ListedHost>,
    skipped: usize,
    aliases: HashMap<String, Alias>,
}

/// Turn the text behind a shared sessions link into the hosts it lists, for the link
/// `source_id` whose hosts live in `folder`. The error says what is wrong with the text,
/// in words for the user.
pub fn parse_shared_sessions(
    content: &str,
    source_id: &str,
    folder: &str,
) -> Result<SharedSessionsList, ImportError> {
    let text = js_trim(content.strip_prefix('\u{feff}').unwrap_or(content));
    if text.is_empty() {
        return Err(ImportError::new("The link returned nothing."));
    }
    if is_markup(text) {
        return Err(ImportError::new(
            "The link returned a web page instead of a session list. It may need a sign-in.",
        ));
    }
    let listed = if text.starts_with('{') {
        json_hosts(text)?
    } else {
        mobaxterm_hosts(text)?
    };
    if listed.hosts.is_empty() {
        return Err(ImportError::new("The shared sessions list has no hosts."));
    }
    if listed.hosts.len() > MAX_SHARED_SESSION_HOSTS {
        return Err(ImportError::new(format!(
            "The shared sessions list has more than {MAX_SHARED_SESSION_HOSTS} hosts."
        )));
    }

    let ids: HashMap<&str, String> = listed
        .hosts
        .iter()
        .map(|host| {
            (
                host.key.as_str(),
                stable_import_id("shared", &format!("{source_id}\0{}", host.key)),
            )
        })
        .collect();
    let hosts = listed
        .hosts
        .iter()
        .map(|host| {
            let group = [Some(folder), host.look.folder.as_deref()]
                .into_iter()
                .flatten()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            let group = utf16_prefix(&group, MAX_FOLDER_LENGTH);
            SharedListHost {
                id: ids[host.key.as_str()].clone(),
                name: host.name.clone(),
                profile: placed_profile(&host.profile, &ids, &listed.aliases),
                group: (!group.is_empty()).then(|| group.to_owned()),
                color: host.look.color.clone(),
                icon: host.look.icon.clone(),
            }
        })
        .collect();
    Ok(SharedSessionsList {
        format: listed.format,
        hosts,
        skipped: listed.skipped,
    })
}

/// `/^<(?:!doctype|html|\?xml)/i`: a sign-in page or an error page, not a list.
fn is_markup(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('<') else {
        return false;
    };
    ["!doctype", "html", "?xml"].iter().any(|tag| {
        rest.as_bytes()
            .get(..tag.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(tag.as_bytes()))
    })
}

/// `/^\s*\[Bookmarks(?:_\d+)?\]\s*$/im`: some line is a MobaXterm bookmarks section.
fn has_bookmarks_section(text: &str) -> bool {
    text.split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .any(|line| {
            let Some(inner) = js_trim(line)
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            else {
                return false;
            };
            let Some(name) = inner.get(..9) else {
                return false;
            };
            if !name.eq_ignore_ascii_case("bookmarks") {
                return false;
            }
            match inner[9..].strip_prefix('_') {
                None => inner.len() == 9,
                Some(digits) => !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
            }
        })
}

fn mobaxterm_hosts(text: &str) -> Result<Listed, ImportError> {
    if !has_bookmarks_section(text) {
        return Err(ImportError::new(
            "The link returned neither a MobaXterm session file nor a Muxus session list.",
        ));
    }
    let parsed = parse_mobaxterm_sessions(text)?;
    // Muxus-owned hosts: a list never writes into the user's ssh_config.
    let connections = imported_connections(
        parsed.sessions.iter(),
        "MobaXterm",
        ImportedSshStorage::Muxus,
    );
    let jump_folder = "MobaXterm jump hosts";
    let hosts = connections
        .saved_hosts
        .into_iter()
        .map(|host| {
            let folder = match host.metadata.group {
                Some(group) if group == jump_folder => Some(JUMP_HOST_FOLDER.to_owned()),
                group => group,
            };
            ListedHost {
                key: host.id,
                name: host.name,
                profile: object_of(&host.profile),
                look: Look {
                    folder,
                    ..Look::default()
                },
            }
        })
        .collect();
    Ok(Listed {
        format: SharedSessionsFormat::MobaXterm,
        hosts,
        skipped: parsed.ignored_count,
        aliases: HashMap::new(),
    })
}

fn object_of(value: &impl Serialize) -> Map<String, Value> {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

fn json_hosts(text: &str) -> Result<Listed, ImportError> {
    let document: Value = serde_json::from_str(text)
        .map_err(|_| ImportError::new("The link returned JSON that cannot be read."))?;
    let format = document.get("format").and_then(Value::as_str);
    if let Some(document) = document.as_object() {
        if format == Some(SHARED_SESSIONS_FORMAT) {
            return session_list_hosts(document);
        }
        if format == Some(BACKUP_FORMAT)
            && let Some(data) = document.get("data").and_then(Value::as_object)
        {
            return Ok(backup_hosts(data));
        }
    }
    Err(ImportError::new(
        "The link returned JSON that is neither a Muxus session list nor a Muxus backup.",
    ))
}

/// Entries of a list keep apart even when two share an id, or have none.
#[derive(Default)]
struct Collector {
    hosts: Vec<ListedHost>,
    keys: HashSet<String>,
}

impl Collector {
    /// Add a host under its key, or under the next free one; the key it got.
    fn add(&mut self, mut host: ListedHost) -> String {
        let wanted = host.key.clone();
        let mut suffix = 2;
        while self.keys.contains(&host.key) {
            host.key = format!("{wanted}\0{suffix}");
            suffix += 1;
        }
        self.keys.insert(host.key.clone());
        let key = host.key.clone();
        self.hosts.push(host);
        key
    }

    /// One `{ id?, name, profile }` entry; whether it was a host.
    fn add_entry(&mut self, entry: &Value, look: Look) -> bool {
        let Some(record) = entry.as_object() else {
            return false;
        };
        let name = record
            .get("name")
            .and_then(Value::as_str)
            .map(clean_import_name)
            .unwrap_or_default();
        let Some(profile) = record.get("profile").and_then(Value::as_object) else {
            return false;
        };
        let Some(kind) = profile
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| SAVED_KINDS.contains(kind))
        else {
            return false;
        };
        if name.is_empty() {
            return false;
        }
        let key = match record.get("id").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => format!(
                "{kind}\0{}\0{name}",
                look.folder.as_deref().unwrap_or_default()
            ),
        };
        self.add(ListedHost {
            key,
            name,
            profile: profile.clone(),
            look,
        });
        true
    }
}

fn array_of(value: Option<&Value>) -> &[Value] {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn non_empty_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn look_of(group: Option<&Value>, color: Option<&Value>, icon: Option<&Value>) -> Look {
    Look {
        folder: group
            .and_then(Value::as_str)
            .and_then(normalize_import_folder),
        color: non_empty_text(color),
        icon: non_empty_text(icon),
    }
}

/// How a host with this metadata looks (`listedLook`).
fn metadata_look(metadata: Option<&Value>) -> Look {
    match metadata.and_then(Value::as_object) {
        Some(metadata) => look_of(
            metadata.get("group"),
            metadata.get("color"),
            metadata.get("icon"),
        ),
        None => Look::default(),
    }
}

/// The list a shared folder is written as: its hosts, each with the folder it sits in.
fn session_list_hosts(document: &Map<String, Value>) -> Result<Listed, ImportError> {
    if document
        .get("version")
        .and_then(Value::as_f64)
        .is_some_and(|version| version > f64::from(SHARED_SESSIONS_VERSION))
    {
        return Err(ImportError::new(
            "This session list was written by a newer Muxus. Update Muxus to follow it.",
        ));
    }
    let mut collector = Collector::default();
    let mut skipped = 0;
    for entry in array_of(document.get("hosts")) {
        let look = look_of(entry.get("folder"), entry.get("color"), entry.get("icon"));
        if !collector.add_entry(entry, look) {
            skipped += 1;
        }
    }
    Ok(Listed {
        format: SharedSessionsFormat::Muxus,
        hosts: collector.hosts,
        skipped,
        aliases: HashMap::new(),
    })
}

/// A backup: only its hosts are used.
fn backup_hosts(data: &Map<String, Value>) -> Listed {
    let mut collector = Collector::default();
    let mut aliases: HashMap<String, Alias> = HashMap::new();
    let mut skipped = 0;

    for entry in array_of(data.get("savedHosts")) {
        if !collector.add_entry(entry, metadata_look(entry.get("metadata"))) {
            skipped += 1;
        }
    }

    // ssh_config hosts of a backup become Muxus-owned hosts with the same settings.
    for entry in array_of(data.get("sshHosts")) {
        let alias = entry
            .get("alias")
            .and_then(Value::as_str)
            .map(js_trim)
            .unwrap_or_default();
        if !entry.is_object() || alias.is_empty() {
            skipped += 1;
            continue;
        }
        let options = entry.get("options").and_then(Value::as_object);
        let target = options
            .and_then(|options| options.get("hostname"))
            .and_then(Value::as_str)
            .map(js_trim)
            .filter(|hostname| !hostname.is_empty())
            .unwrap_or(alias);
        let metadata = entry.get("metadata").filter(|value| value.is_object());
        let name = metadata
            .and_then(|metadata| metadata.get("displayName"))
            .and_then(Value::as_str)
            .map(clean_import_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| clean_import_name(alias));
        let mut profile = Map::new();
        profile.insert("kind".into(), Value::from("ssh"));
        profile.insert("target".into(), Value::from(target));
        if let Some(options) = options {
            for key in SSH_OPTION_KEYS {
                if let Some(value) = options.get(key).filter(|value| !value.is_null()) {
                    profile.insert(key.into(), value.clone());
                }
            }
        }
        let key = collector.add(ListedHost {
            key: format!("ssh\0{alias}"),
            name,
            profile,
            look: metadata_look(metadata),
        });
        let names = std::iter::once(alias).chain(
            array_of(entry.get("aliases"))
                .iter()
                .filter_map(Value::as_str),
        );
        for name in names {
            aliases.entry(name.to_owned()).or_insert_with(|| Alias {
                key: key.clone(),
                target: target.to_owned(),
            });
        }
    }
    Listed {
        format: SharedSessionsFormat::Muxus,
        hosts: collector.hosts,
        skipped,
        aliases,
    }
}

/// A listed profile with its references to other hosts of the list pointed at the hosts
/// they became.
fn placed_profile(
    listed: &Map<String, Value>,
    ids: &HashMap<&str, String>,
    aliases: &HashMap<String, Alias>,
) -> Map<String, Value> {
    let hop = |value: &Value| -> Value {
        let Some(text) = value.as_str() else {
            return value.clone();
        };
        let key = saved_host_hop_id(text).or_else(|| aliases.get(text).map(|a| a.key.as_str()));
        match key.and_then(|key| ids.get(key)) {
            Some(id) => Value::from(saved_host_hop(id)),
            None => value.clone(),
        }
    };
    let hops = |value: &Value| -> Value {
        match value.as_array() {
            Some(route) => Value::Array(route.iter().map(hop).collect()),
            None => value.clone(),
        }
    };
    let gateway = |value: &Value| -> Value {
        let Some(record) = value.as_object() else {
            return value.clone();
        };
        let alias = record
            .get("target")
            .and_then(Value::as_str)
            .and_then(|target| aliases.get(target));
        let named = record.get("profileId").and_then(Value::as_str);
        let key = named.or(alias.map(|alias| alias.key.as_str()));
        let Some(id) = key.and_then(|key| ids.get(key)) else {
            return value.clone();
        };
        let mut placed = record.clone();
        // A gateway named by alias is dialed at the address that alias stood for.
        if named.is_none()
            && let Some(alias) = alias
        {
            placed.insert("target".into(), Value::from(alias.target.clone()));
        }
        placed.insert("profileId".into(), Value::from(id.clone()));
        Value::Object(placed)
    };

    let mut placed = listed.clone();
    placed.shift_remove("profileId");
    if listed.get("kind").and_then(Value::as_str) == Some("ssh") {
        placed.insert("useConfig".into(), Value::from(false));
        if let Some(route) = listed.get("proxyJump").filter(|value| value.is_array()) {
            placed.insert("proxyJump".into(), hops(route));
        }
        if let Some(routes) = listed.get("proxyJumpFallbacks").and_then(Value::as_array) {
            placed.insert(
                "proxyJumpFallbacks".into(),
                Value::Array(routes.iter().map(hops).collect()),
            );
        }
    }
    if let Some(value) = listed.get("sshGateway") {
        placed.insert("sshGateway".into(), gateway(value));
    }
    if let Some(gateways) = listed.get("sshGatewayFallbacks").and_then(Value::as_array) {
        placed.insert(
            "sshGatewayFallbacks".into(),
            Value::Array(gateways.iter().map(gateway).collect()),
        );
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_a_web_page_from_a_list() {
        assert!(is_markup("<!DOCTYPE html><html>"));
        assert!(is_markup("<HTML lang=\"en\">"));
        assert!(is_markup("<?xml version=\"1.0\"?>"));
        assert!(!is_markup("[Bookmarks]"));
        assert!(!is_markup("<"));
        assert!(!is_markup("<body>"));
    }

    #[test]
    fn finds_a_bookmarks_section_on_any_line() {
        assert!(has_bookmarks_section("[Bookmarks]\nSubRep=\n"));
        assert!(has_bookmarks_section("; comment\r\n  [bookmarks_12]  \r\n"));
        assert!(!has_bookmarks_section("[Bookmarks_]\n"));
        assert!(!has_bookmarks_section("[Bookmarks_1a]\n"));
        assert!(!has_bookmarks_section("x [Bookmarks]\n"));
        assert!(!has_bookmarks_section("[Bookmark]\n"));
        assert!(!has_bookmarks_section("not found"));
    }
}
