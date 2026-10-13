//! MobaXterm bookmarks (port of `client/src/mobaxterm-import.ts`).
//!
//! Session values use `#icon#protocol%host%port%...#terminal settings#...`;
//! SSH is protocol 0, RDP 4 and VNC 5. Private keys and SSH gateways are kept
//! as paths and hops, and an SSH session's "Execute command" as its startup
//! command. Only bookmark structure is read — never the P/C secret stores.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use muxus_core::js;

use crate::portable::PortableConnections;
use crate::session::{
    ImportError, ImportedAuthMode, ImportedDesktopSession, ImportedJumpHost, ImportedSession,
    ImportedSessionParseResult, ImportedSshSession, ImportedSshStorage, SkippedImportedSession,
    imported_connections,
};
use crate::text::{
    clean_import_name, normalize_import_folder, parse_port, stable_import_id,
    trim_trailing_separators, unique_import_alias,
};

pub const MAX_MOBAXTERM_IMPORT_BYTES: usize = 10 * 1024 * 1024;

/// Separates the hops of a multi-hop SSH gateway inside each gateway field.
const HOP_SEPARATOR: &str = "__PIPE__";
/// Characters MobaXterm spells out because they delimit its own format.
const ESCAPES: [(&str, &str); 4] = [
    ("__PTVIRG__", ";"),
    ("__PIPE__", "|"),
    ("__EQUAL__", "="),
    ("__DIEZE__", "#"),
];
const UNSUPPORTED_REASON: &str = "Only SSH, RDP and VNC sessions can be imported";

/// Field positions of the SSH gateway (jump host) settings of one protocol.
struct GatewayFields {
    host: usize,
    port: usize,
    username: usize,
    identity_file: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    Ssh,
    Rdp,
    Vnc,
}

/// "Execute command", and whether a shell stays open once it ends.
struct CommandFields {
    value: usize,
    keep_shell: usize,
}

struct ProtocolLayout {
    kind: LayoutKind,
    label: &'static str,
    default_port: u16,
    username: Option<usize>,
    identity_file: Option<usize>,
    command: Option<CommandFields>,
    gateway: GatewayFields,
}

/// Session fields per MobaXterm protocol. Index 1 is always the host and
/// index 2 the port; every protocol can go through an SSH gateway.
fn protocol_layout(protocol: u32) -> Option<ProtocolLayout> {
    match protocol {
        0 => Some(ProtocolLayout {
            kind: LayoutKind::Ssh,
            label: "SSH",
            default_port: 22,
            username: Some(3),
            identity_file: Some(14),
            command: Some(CommandFields {
                value: 7,
                keep_shell: 11,
            }),
            gateway: GatewayFields {
                host: 8,
                port: 9,
                username: 10,
                identity_file: 15,
            },
        }),
        4 => Some(ProtocolLayout {
            kind: LayoutKind::Rdp,
            label: "RDP",
            default_port: 3389,
            username: Some(3),
            identity_file: None,
            command: None,
            gateway: GatewayFields {
                host: 13,
                port: 14,
                username: 15,
                identity_file: 18,
            },
        }),
        5 => Some(ProtocolLayout {
            kind: LayoutKind::Vnc,
            label: "VNC",
            default_port: 5900,
            username: None,
            identity_file: None,
            command: None,
            gateway: GatewayFields {
                host: 5,
                port: 6,
                username: 7,
                identity_file: 8,
            },
        }),
        _ => None,
    }
}

/// Parse SSH, RDP and VNC bookmarks from MobaXterm.ini or an .mxtsessions
/// export. The error message is meant for the user.
pub fn parse_mobaxterm_sessions(text: &str) -> Result<ImportedSessionParseResult, ImportError> {
    if text.len() > MAX_MOBAXTERM_IMPORT_BYTES {
        return Err(ImportError::new(
            "That MobaXterm file is larger than 10 MB.",
        ));
    }

    let mut sessions = Vec::new();
    let mut aliases: HashSet<String> = HashSet::new();
    let mut desktop_ids: HashSet<String> = HashSet::new();
    let mut jump_hosts = JumpHostRegistry::default();
    let mut in_bookmarks = false;
    let mut current_folder: Option<String> = None;
    let mut skipped_sessions: Vec<SkippedImportedSession> = Vec::new();

    let skip = |skipped: &mut Vec<SkippedImportedSession>,
                folder: &Option<String>,
                line_index: usize,
                name: &str,
                fallback: Option<&str>,
                reason: String| {
        let cleaned = clean_import_name(name);
        let display = if !cleaned.is_empty() {
            cleaned
        } else {
            let cleaned_fallback = clean_import_name(fallback.unwrap_or(""));
            if cleaned_fallback.is_empty() {
                "Unnamed session".to_owned()
            } else {
                cleaned_fallback
            }
        };
        skipped.push(SkippedImportedSession {
            id: format!("mobaxterm-skipped-{line_index}"),
            name: display,
            folder: folder.clone(),
            reason,
        });
    };

    for (line_index, raw_line) in split_lines(text).enumerate() {
        let line = js::js_trim(raw_line);
        if line.is_empty() || line.starts_with(';') {
            continue;
        }

        if let Some(section) = section_name(line) {
            in_bookmarks = is_bookmark_section(js::js_trim(section));
            current_folder = None;
            continue;
        }
        if !in_bookmarks {
            continue;
        }

        let Some(separator) = line.find('=') else {
            continue;
        };
        let name = js::js_trim(&line[..separator]);
        let value = js::js_trim(&line[separator + 1..]);

        if name.to_lowercase() == "subrep" {
            current_folder = normalize_import_folder(value);
            continue;
        }

        let Some((header_len, protocol)) = session_header(value) else {
            continue;
        };
        let Some(layout) = protocol.and_then(protocol_layout) else {
            skip(
                &mut skipped_sessions,
                &current_folder,
                line_index,
                name,
                None,
                UNSUPPORTED_REASON.to_owned(),
            );
            continue;
        };

        // Only the connection block: terminal and display settings follow the next '#'.
        let rest = &value[header_len..];
        let connection = rest.split('#').next().unwrap_or("");
        let fields: Vec<&str> = connection.split('%').collect();
        let field = |index: usize| fields.get(index).copied();
        let host = field(1).map(js::js_trim).unwrap_or("");
        if name.is_empty() {
            skip(
                &mut skipped_sessions,
                &current_folder,
                line_index,
                name,
                Some(host),
                format!("{} session has no name", layout.label),
            );
            continue;
        }
        if host.is_empty() {
            skip(
                &mut skipped_sessions,
                &current_folder,
                line_index,
                name,
                None,
                format!("{} session has no hostname", layout.label),
            );
            continue;
        }
        let port = parse_port(field(2), layout.default_port);
        let username = layout
            .username
            .and_then(field)
            .map(js::js_trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let gateway = parse_gateway(&fields, &layout.gateway, &mut jump_hosts, &mut aliases);

        if layout.kind == LayoutKind::Ssh {
            let identity_file = layout
                .identity_file
                .and_then(|index| mobaxterm_path(field(index)));
            let remote_command = layout
                .command
                .as_ref()
                .and_then(|command| startup_command(&fields, command));
            let alias = unique_import_alias(name, host, &aliases, "mobaxterm-host");
            aliases.insert(alias.clone());
            let key_auth = identity_file.is_some() || field(4).map(js::js_trim) == Some("3");
            sessions.push(ImportedSession::Ssh(ImportedSshSession {
                id: stable_import_id(
                    "mobaxterm-session",
                    &format!("{}\0{alias}", current_folder.as_deref().unwrap_or("")),
                ),
                name: name.to_owned(),
                alias,
                host: host.to_owned(),
                port,
                username,
                folder: current_folder.clone(),
                auth_mode: if key_auth {
                    ImportedAuthMode::Key
                } else {
                    ImportedAuthMode::Password
                },
                identity_file,
                remote_command,
                jump_host: gateway,
            }));
            continue;
        }

        let key = format!("{}\0{name}", current_folder.as_deref().unwrap_or(""));
        let mut id_key = key.clone();
        let mut suffix = 2;
        while desktop_ids.contains(&id_key) {
            id_key = format!("{key}\0{suffix}");
            suffix += 1;
        }
        desktop_ids.insert(id_key.clone());
        let (prefix, wrap): (&str, fn(ImportedDesktopSession) -> ImportedSession) =
            if layout.kind == LayoutKind::Rdp {
                ("mobaxterm-rdp", ImportedSession::Rdp)
            } else {
                ("mobaxterm-vnc", ImportedSession::Vnc)
            };
        sessions.push(wrap(ImportedDesktopSession {
            id: stable_import_id(prefix, &id_key),
            name: name.to_owned(),
            host: host.to_owned(),
            port,
            username,
            folder: current_folder.clone(),
            jump_host: gateway,
        }));
    }

    if sessions.is_empty() {
        return Err(ImportError::new(
            "No SSH, RDP or VNC sessions were found in this MobaXterm file.",
        ));
    }
    Ok(ImportedSessionParseResult {
        sessions,
        ignored_count: skipped_sessions.len(),
        skipped_sessions,
    })
}

/// Convert reviewed MobaXterm rows into the portable restore pipeline.
pub fn mobaxterm_connections<'a>(
    sessions: impl IntoIterator<Item = &'a ImportedSession>,
    ssh_storage: ImportedSshStorage,
) -> PortableConnections {
    imported_connections(sessions, "MobaXterm", ssh_storage)
}

/// Resolve a MobaXterm key path to one Muxus and OpenSSH can read.
///
/// MobaXterm stores paths relative to placeholders so a portable copy keeps
/// working. `_CurrentDrive_` is the drive MobaXterm runs from — C: for an
/// installed copy.
pub fn mobaxterm_path(value: Option<&str>) -> Option<String> {
    let path = js::js_trim(value?);
    if path.is_empty() {
        return None;
    }
    let mut expanded = path.to_owned();
    for (placeholder, needs_boundary, replacement) in [
        ("_ProfileDir_", true, "~"),
        ("_MyDocuments_", true, "~/Documents"),
        ("_CurrentDrive_:", false, "C:"),
    ] {
        let Some(head) = expanded.get(..placeholder.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(placeholder) {
            continue;
        }
        let tail = &expanded[placeholder.len()..];
        if needs_boundary && !(tail.is_empty() || tail.starts_with('\\')) {
            continue;
        }
        expanded = format!("{replacement}{tail}");
    }
    Some(expanded.replace('\\', "/"))
}

/// `text.split(/\r?\n/)`.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
}

/// `/^\[([^\]]+)\]$/` on a trimmed line.
fn section_name(line: &str) -> Option<&str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    (!inner.is_empty() && !inner.contains(']')).then_some(inner)
}

/// `/^Bookmarks(?:_\d+)?$/i`.
fn is_bookmark_section(name: &str) -> bool {
    const PREFIX: &str = "Bookmarks";
    let Some(head) = name.get(..PREFIX.len()) else {
        return false;
    };
    if !head.eq_ignore_ascii_case(PREFIX) {
        return false;
    }
    let tail = &name[PREFIX.len()..];
    tail.is_empty()
        || tail
            .strip_prefix('_')
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// `/^#\d+#(\d+)(?=%)/`: the header's length and its protocol number, `None`
/// for a protocol number too large to be one Muxus knows.
fn session_header(value: &str) -> Option<(usize, Option<u32>)> {
    let bytes = value.as_bytes();
    if bytes.first() != Some(&b'#') {
        return None;
    }
    let icon_end = 1 + bytes[1..].iter().take_while(|b| b.is_ascii_digit()).count();
    if icon_end == 1 || bytes.get(icon_end) != Some(&b'#') {
        return None;
    }
    let start = icon_end + 1;
    let end = start
        + bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
    if end == start || bytes.get(end) != Some(&b'%') {
        return None;
    }
    let digits = value[start..end].trim_start_matches('0');
    let protocol = if digits.is_empty() {
        Some(0)
    } else {
        digits.parse::<u32>().ok()
    };
    Some((end, protocol))
}

/// The session's "Execute command", run as the session's startup command.
/// MobaXterm ends the session with the command unless "Do not exit after
/// command ends" is set; then the login shell takes over afterwards.
fn startup_command(fields: &[&str], layout: &CommandFields) -> Option<String> {
    let mut command = fields.get(layout.value).copied().unwrap_or("").to_owned();
    for (token, character) in ESCAPES {
        command = command.replace(token, character);
    }
    let command = trim_trailing_separators(js::js_trim(&command));
    if command.is_empty() {
        return None;
    }
    let keep_shell = fields
        .get(layout.keep_shell)
        .map(|value| js::js_trim(value))
        == Some("-1");
    Some(if keep_shell {
        format!("{command}; exec \"$SHELL\" -l")
    } else {
        command.to_owned()
    })
}

/// The last hop of a session's SSH gateway chain; earlier hops hang off `via`.
fn parse_gateway(
    fields: &[&str],
    layout: &GatewayFields,
    registry: &mut JumpHostRegistry,
    aliases: &mut HashSet<String>,
) -> Option<Arc<ImportedJumpHost>> {
    let column = |index: usize| -> Vec<&str> {
        fields
            .get(index)
            .copied()
            .unwrap_or("")
            .split(HOP_SEPARATOR)
            .collect()
    };
    let ports = column(layout.port);
    let usernames = column(layout.username);
    let identity_files = column(layout.identity_file);
    let mut last: Option<Arc<ImportedJumpHost>> = None;
    for (index, raw_host) in column(layout.host).into_iter().enumerate() {
        let host = js::js_trim(raw_host);
        if host.is_empty() {
            continue;
        }
        let username = usernames
            .get(index)
            .map(|value| js::js_trim(value))
            .filter(|value| !value.is_empty());
        let identity_file = mobaxterm_path(identity_files.get(index).copied());
        last = Some(registry.hop(
            host,
            parse_port(ports.get(index).copied(), 22),
            username,
            identity_file,
            last,
            aliases,
        ));
    }
    last
}

/// One shared entry per distinct hop (including the hops before it), with a
/// readable name and an ssh_config alias that cannot collide with sessions.
#[derive(Default)]
struct JumpHostRegistry {
    hops: HashMap<String, Arc<ImportedJumpHost>>,
    names: HashSet<String>,
}

impl JumpHostRegistry {
    fn hop(
        &mut self,
        host: &str,
        port: u16,
        username: Option<&str>,
        identity_file: Option<String>,
        via: Option<Arc<ImportedJumpHost>>,
        aliases: &mut HashSet<String>,
    ) -> Arc<ImportedJumpHost> {
        let id = stable_import_id(
            "mobaxterm-jump",
            &[
                via.as_ref().map_or("", |via| via.id.as_str()),
                host,
                &port.to_string(),
                username.unwrap_or(""),
                identity_file.as_deref().unwrap_or(""),
            ]
            .join("\0"),
        );
        if let Some(existing) = self.hops.get(&id) {
            return Arc::clone(existing);
        }

        let address = format!(
            "{}{host}{}",
            username.map(|user| format!("{user}@")).unwrap_or_default(),
            if port == 22 {
                String::new()
            } else {
                format!(":{port}")
            }
        );
        let base_name = match &via {
            Some(via) => format!("{address} via {}", via.name),
            None => address.clone(),
        };
        let mut name = base_name.clone();
        let mut suffix = 2;
        while self.names.contains(&name) {
            name = format!("{base_name} ({suffix})");
            suffix += 1;
        }
        self.names.insert(name.clone());
        // '@' and ':' would make ProxyJump read the alias as user@host:port.
        let alias = unique_import_alias(
            &format!("jump-{}", replace_address_separators(&address)),
            host,
            aliases,
            "jump-host",
        );
        aliases.insert(alias.clone());

        let jump = Arc::new(ImportedJumpHost {
            id: id.clone(),
            alias,
            name,
            host: host.to_owned(),
            port,
            username: username.map(str::to_owned),
            identity_file,
            via,
        });
        self.hops.insert(id, Arc::clone(&jump));
        jump
    }
}

/// `address.replace(/[@:[\]]+/g, '-')`.
fn replace_address_separators(address: &str) -> String {
    let mut out = String::with_capacity(address.len());
    let mut in_run = false;
    for c in address.chars() {
        if matches!(c, '@' | ':' | '[' | ']') {
            if !in_run {
                out.push('-');
                in_run = true;
            }
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_follow_the_typescript_pattern() {
        assert_eq!(session_header("#109#0%host"), Some((6, Some(0))));
        assert_eq!(session_header("#91#4%"), Some((5, Some(4))));
        assert_eq!(session_header("#1#0004%"), Some((7, Some(4))));
        assert_eq!(session_header("#109#0"), None);
        assert_eq!(session_header("##0%"), None);
        assert_eq!(session_header("#1#%"), None);
        assert_eq!(session_header("x#1#0%"), None);
        assert_eq!(
            session_header(&format!("#1#{}%", "9".repeat(40))).map(|h| h.1),
            Some(None)
        );
    }

    #[test]
    fn sections_and_paths() {
        assert_eq!(section_name("[Bookmarks_1]"), Some("Bookmarks_1"));
        assert_eq!(section_name("[]"), None);
        assert_eq!(section_name("[a]b]"), None);
        assert!(is_bookmark_section("bookmarks"));
        assert!(is_bookmark_section("BOOKMARKS_12"));
        assert!(!is_bookmark_section("Bookmarks_"));
        assert!(!is_bookmark_section("Bookmarks_x"));
        assert_eq!(
            mobaxterm_path(Some(" _profiledir_\\keys\\a ")).as_deref(),
            Some("~/keys/a")
        );
        assert_eq!(mobaxterm_path(Some("_ProfileDir_")).as_deref(), Some("~"));
        assert_eq!(
            mobaxterm_path(Some("_ProfileDirX\\a")).as_deref(),
            Some("_ProfileDirX/a")
        );
        assert_eq!(
            mobaxterm_path(Some("_MyDocuments_\\k")).as_deref(),
            Some("~/Documents/k")
        );
        assert_eq!(
            mobaxterm_path(Some("_CurrentDrive_:\\keys\\b.ppk")).as_deref(),
            Some("C:/keys/b.ppk")
        );
        assert_eq!(mobaxterm_path(Some("  ")), None);
        assert_eq!(mobaxterm_path(None), None);
    }
}
