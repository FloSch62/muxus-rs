//! SecureCRT "Tools → Export Settings" XML (port of
//! `client/src/securecrt-import.ts`). SSH2 and serial sessions are read;
//! passwords, keys, embedded files and global settings never are.

use std::collections::HashSet;

use muxus_core::js;
use muxus_core::protocol::{SerialFlowControl, SerialParity, StopBits};
use roxmltree::{Document, Node, ParsingOptions};

use crate::portable::PortableConnections;
use crate::session::{
    ImportError, ImportedAuthMode, ImportedSerialSession, ImportedSession,
    ImportedSessionParseResult, ImportedSshSession, ImportedSshStorage, SkippedImportedSession,
    imported_connections,
};
use crate::text::{
    clean_import_name, js_parse_int, normalize_import_folder, parse_int_in, stable_import_id,
    strip_control_characters, unique_import_alias,
};

pub const MAX_SECURECRT_IMPORT_BYTES: usize = 20 * 1024 * 1024;

/// Parse SSH and serial sessions from a SecureCRT XML export. The error
/// message is meant for the user.
pub fn parse_securecrt_sessions(text: &str) -> Result<ImportedSessionParseResult, ImportError> {
    if text.len() > MAX_SECURECRT_IMPORT_BYTES {
        return Err(ImportError::new(
            "That SecureCRT file is larger than 20 MB.",
        ));
    }
    // The exported configuration can contain encrypted secrets and embedded
    // files. DTDs/entities are never part of the format and are rejected before
    // an untrusted document reaches the XML parser.
    if has_declaration(text) {
        return Err(ImportError::new(
            "SecureCRT XML with declarations or entities is not supported.",
        ));
    }

    let options = ParsingOptions {
        allow_dtd: false,
        ..ParsingOptions::default()
    };
    let document = Document::parse_with_options(text, options)
        .map_err(|_| ImportError::new("That SecureCRT XML file is malformed."))?;
    let root = document.root_element();
    if tag_name(root) != "VanDyke"
        || root
            .descendants()
            .any(|node| node.is_element() && tag_name(node) == "parsererror")
    {
        return Err(ImportError::new(
            "That is not a valid SecureCRT XML export.",
        ));
    }
    let Some(sessions_root) =
        direct_elements(root, "key").find(|element| element.attribute("name") == Some("Sessions"))
    else {
        return Err(ImportError::new(
            "No SecureCRT Sessions section was found in this XML file.",
        ));
    };

    let mut parser = Parser::default();
    for key in direct_elements(sessions_root, "key") {
        parser.visit(key, &[], &[]);
    }
    if parser.sessions.is_empty() {
        return Err(ImportError::new(
            "No supported SSH or serial sessions were found in this SecureCRT file.",
        ));
    }
    Ok(ImportedSessionParseResult {
        ignored_count: parser.skipped_sessions.len(),
        sessions: parser.sessions,
        skipped_sessions: parser.skipped_sessions,
    })
}

pub fn securecrt_connections<'a>(
    sessions: impl IntoIterator<Item = &'a ImportedSession>,
    ssh_storage: ImportedSshStorage,
) -> PortableConnections {
    imported_connections(sessions, "SecureCRT", ssh_storage)
}

#[derive(Default)]
struct Parser {
    sessions: Vec<ImportedSession>,
    aliases: HashSet<String>,
    profile_ids: HashSet<String>,
    session_ids: HashSet<String>,
    skipped_sessions: Vec<SkippedImportedSession>,
}

impl Parser {
    fn visit(&mut self, key: Node<'_, '_>, folders: &[String], source_path: &[String]) {
        let raw_name = key.attribute("name").unwrap_or("");
        let name = clean_import_name(raw_name);
        let mut next_source_path = source_path.to_vec();
        next_source_path.push(raw_name.to_owned());
        let is_session = direct_value(key, "dword", &["Is Session"]) == "1";
        if !is_session {
            let mut next_folders = folders.to_vec();
            if !name.is_empty() {
                next_folders.push(name);
            }
            for child in direct_elements(key, "key") {
                self.visit(child, &next_folders, &next_source_path);
            }
            return;
        }

        let protocol = direct_value(key, "string", &["Protocol Name"]);
        let normalized_protocol = protocol.to_lowercase();
        let folder = normalize_import_folder(&folders.join("/"));
        let source_key = next_source_path.join("\0");
        let skip = |skipped: &mut Vec<SkippedImportedSession>,
                    reason: String,
                    fallback: Option<String>| {
            let display = if !name.is_empty() {
                name.clone()
            } else {
                fallback
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "Unnamed session".to_owned())
            };
            skipped.push(SkippedImportedSession {
                id: format!("securecrt-skipped-{}", skipped.len()),
                name: display,
                folder: folder.clone(),
                reason,
            });
        };
        if name.is_empty() {
            let fallback = clean_option_value(&direct_value(
                key,
                "string",
                &["Hostname", "Mac Com Port", "Com Port", "Port"],
            ));
            skip(
                &mut self.skipped_sessions,
                "Session has no name".to_owned(),
                Some(fallback),
            );
            return;
        }

        if normalized_protocol == "ssh2" {
            let host = clean_option_value(&direct_value(key, "string", &["Hostname"]));
            if host.is_empty() {
                skip(
                    &mut self.skipped_sessions,
                    "SSH session has no hostname".to_owned(),
                    None,
                );
                return;
            }
            let username = Some(clean_option_value(&direct_value(
                key,
                "string",
                &["Username"],
            )))
            .filter(|value| !value.is_empty());
            let port = parse_int_in(&direct_value(key, "dword", &["[SSH2] Port"]), 1, 65_535)
                .map_or(22, |port| port as u16);
            let authentications: HashSet<String> =
                direct_value(key, "string", &["SSH2 Authentications V2"])
                    .to_lowercase()
                    .split(',')
                    .map(|value| js::js_trim(value).to_owned())
                    .filter(|value| !value.is_empty())
                    .collect();
            let has_password = authentications.contains("password")
                || authentications.contains("keyboard-interactive");
            let auth_mode = if !authentications.contains("publickey") && has_password {
                ImportedAuthMode::Password
            } else {
                ImportedAuthMode::Key
            };
            let alias = unique_import_alias(&name, &host, &self.aliases, "securecrt-host");
            self.aliases.insert(alias.clone());
            let id = unique_stable_id(
                stable_import_id("securecrt-session", &source_key),
                &self.session_ids,
            );
            self.session_ids.insert(id.clone());
            self.sessions.push(ImportedSession::Ssh(ImportedSshSession {
                id,
                name,
                alias,
                host,
                port,
                username,
                folder,
                auth_mode,
                identity_file: None,
                remote_command: None,
                jump_host: None,
            }));
            return;
        }

        if normalized_protocol == "serial" {
            let path = clean_serial_path(&direct_value(
                key,
                "string",
                &["Mac Com Port", "Com Port", "Port"],
            ));
            if path.is_empty() {
                skip(
                    &mut self.skipped_sessions,
                    "Serial session has no port".to_owned(),
                    None,
                );
                return;
            }
            let baud_rate = parse_int_in(
                &direct_value(key, "dword", &["Mac Baud Rate", "Baud Rate"]),
                1,
                12_000_000,
            )
            .unwrap_or(115_200);
            let data_bits =
                serial_data_bits(&direct_value(key, "dword", &["Mac Data Bits", "Data Bits"]));
            let stop_bits =
                serial_stop_bits(&direct_value(key, "dword", &["Mac Stop Bits", "Stop Bits"]));
            let parity = serial_parity(&direct_value(key, "dword", &["Mac Parity", "Parity"]));
            let hardware_flow = enabled(key, &["Mac CTS Flow", "CTS Flow"])
                || enabled(key, &["Mac DSR Flow", "DSR Flow"])
                || direct_value(key, "dword", &["Mac DTR Flow Control", "DTR Flow Control"]) == "2"
                || direct_value(key, "dword", &["Mac RTS Flow Control", "RTS Flow Control"]) == "2";
            let software_flow = enabled(key, &["Mac XON Flow", "XON Flow"]);
            let profile_id = unique_stable_id(
                stable_import_id("securecrt-serial", &source_key),
                &self.profile_ids,
            );
            self.profile_ids.insert(profile_id.clone());
            self.sessions
                .push(ImportedSession::Serial(ImportedSerialSession {
                    id: profile_id.clone(),
                    name,
                    profile_id,
                    path,
                    baud_rate,
                    data_bits,
                    stop_bits,
                    parity,
                    flow_control: if hardware_flow {
                        SerialFlowControl::Hardware
                    } else if software_flow {
                        SerialFlowControl::Software
                    } else {
                        SerialFlowControl::None
                    },
                    folder,
                }));
            return;
        }

        let reason = if protocol.is_empty() {
            "Session does not specify a supported protocol".to_owned()
        } else {
            format!("Protocol “{protocol}” is not supported")
        };
        skip(&mut self.skipped_sessions, reason, None);
    }
}

/// `/<!\s*(?:DOCTYPE|ENTITY)\b/i`.
fn has_declaration(text: &str) -> bool {
    text.match_indices("<!").any(|(index, _)| {
        let rest = text[index + 2..].trim_start_matches(js::is_js_whitespace);
        ["DOCTYPE", "ENTITY"].iter().any(|keyword| {
            rest.get(..keyword.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(keyword))
                && !rest[keyword.len()..]
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
    })
}

/// The element's qualified name as written (`Element.tagName` in an XML document).
fn tag_name<'a>(node: Node<'a, '_>) -> &'a str {
    let source = &node.document().input_text()[node.range()];
    let name = source.strip_prefix('<').unwrap_or(source);
    let end = name
        .find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .unwrap_or(name.len());
    &name[..end]
}

fn direct_elements<'a, 'input: 'a>(
    parent: Node<'a, 'input>,
    tag: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    parent
        .children()
        .filter(move |child| child.is_element() && tag_name(*child) == tag)
}

/// The trimmed text of the first direct `<tag>` child named any of `names`,
/// in document order (not in the order of `names`).
fn direct_value(parent: Node<'_, '_>, tag: &str, names: &[&str]) -> String {
    for child in direct_elements(parent, tag) {
        if names.contains(&child.attribute("name").unwrap_or("")) {
            return js::js_trim(&text_content(child)).to_owned();
        }
    }
    String::new()
}

/// `Node.textContent`: every descendant text node, CDATA included.
fn text_content(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|text| text.text())
        .collect()
}

fn enabled(parent: Node<'_, '_>, names: &[&str]) -> bool {
    direct_value(parent, "dword", names) == "1"
}

/// A value that can go into ssh_config: no quotes, at most 4096 UTF-16 units.
fn clean_option_value(value: &str) -> String {
    let stripped = strip_control_characters(value);
    let cleaned = js::js_trim(&stripped);
    if !cleaned.is_empty() && !cleaned.contains('"') && js::utf16_len(cleaned) <= 4096 {
        cleaned.to_owned()
    } else {
        String::new()
    }
}

fn clean_serial_path(value: &str) -> String {
    let stripped = strip_control_characters(value);
    let cleaned = js::js_trim(&stripped);
    if js::utf16_len(cleaned) <= 4096 {
        cleaned.to_owned()
    } else {
        String::new()
    }
}

fn serial_data_bits(value: &str) -> u8 {
    match js_parse_int(value) {
        Some(bits) if bits == 5.0 || bits == 6.0 || bits == 7.0 || bits == 8.0 => bits as u8,
        _ => 8,
    }
}

fn serial_stop_bits(value: &str) -> StopBits {
    match value {
        "1" => StopBits::OnePointFive,
        "2" => StopBits::Two,
        _ => StopBits::One,
    }
}

fn serial_parity(value: &str) -> SerialParity {
    match js_parse_int(value) {
        Some(1.0) => SerialParity::Odd,
        Some(2.0) => SerialParity::Even,
        Some(3.0) => SerialParity::Mark,
        Some(4.0) => SerialParity::Space,
        _ => SerialParity::None,
    }
}

fn unique_stable_id(base: String, used: &HashSet<String>) -> String {
    if !used.contains(&base) {
        return base;
    }
    let mut suffix = 2;
    while used.contains(&format!("{base}-{suffix}")) {
        suffix += 1;
    }
    format!("{base}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declarations_are_found_like_the_typescript_pattern() {
        assert!(has_declaration("<!DOCTYPE x>"));
        assert!(has_declaration("<! \u{3000}doctype x>"));
        assert!(has_declaration("<!ENTITY"));
        assert!(has_declaration("<!ENTITY-x"));
        assert!(!has_declaration("<!ENTITYx"));
        assert!(!has_declaration("<!-- DOCTYPE -->"));
        assert!(!has_declaration("<VanDyke/>"));
    }
}
