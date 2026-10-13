//! gNMI path strings (port of `shared/src/gnmi-path.ts`), as gnmic and the
//! gNMI path-strings convention write them:
//! `/interface[name=ethernet-1/1]/statistics/in-octets`, optionally preceded
//! by an origin (`openconfig:/interfaces`). Element names keep any YANG
//! module prefix (`srl_nokia-interfaces:interface`); a colon only marks an
//! origin when a slash follows it directly.
//!
//! Key values may contain `/`; `]` and `\` inside a value are escaped with a
//! backslash. Error positions count UTF-16 code units, like the TypeScript.

use std::sync::LazyLock;

use regex::Regex;
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_with::skip_serializing_none;

/// Key leaf name → value, in the order they were written (a JSON object on the wire).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GnmiKeys(pub Vec<(String, String)>);

impl GnmiKeys {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The value of one key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// Set a key, keeping its position when it is already present (like
    /// assigning an existing property of a JavaScript object).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        match self.0.iter_mut().find(|(name, _)| *name == key) {
            Some(entry) => entry.1 = value,
            None => self.0.push((key, value)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

impl<K: Into<String>, V: Into<String>> FromIterator<(K, V)> for GnmiKeys {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut keys = GnmiKeys::default();
        for (key, value) in iter {
            keys.insert(key, value);
        }
        keys
    }
}

impl Serialize for GnmiKeys {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for GnmiKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeysVisitor;
        impl<'de> Visitor<'de> for KeysVisitor {
            type Value = GnmiKeys;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("an object of key values")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<GnmiKeys, A::Error> {
                let mut keys = GnmiKeys::default();
                while let Some((key, value)) = access.next_entry::<String, String>()? {
                    keys.insert(key, value);
                }
                Ok(keys)
            }
        }
        deserializer.deserialize_map(KeysVisitor)
    }
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GnmiPathElem {
    pub name: String,
    /// Absent when the element has no keys.
    pub keys: Option<GnmiKeys>,
}

impl GnmiPathElem {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            keys: None,
        }
    }

    pub fn with_keys(name: impl Into<String>, keys: GnmiKeys) -> Self {
        Self {
            name: name.into(),
            keys: Some(keys),
        }
    }

    /// The value of one key, if the element has it.
    pub fn key(&self, key: &str) -> Option<&str> {
        self.keys.as_ref().and_then(|keys| keys.get(key))
    }
}

#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GnmiPath {
    pub origin: Option<String>,
    pub elems: Vec<GnmiPathElem>,
}

/// A path that does not parse, and where (a UTF-16 offset into the text).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GnmiPathError {
    pub message: String,
    /// Offset into the string where the problem was found.
    pub position: usize,
}

fn error(message: impl Into<String>, position: usize) -> GnmiPathError {
    GnmiPathError {
        message: message.into(),
        position,
    }
}

static ORIGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9_.-]+):/").expect("valid regex"));

/// Characters with their UTF-16 offsets, the way the TypeScript indexes a string.
struct Cursor {
    chars: Vec<char>,
    /// UTF-16 offset of each char, plus the total length at the end.
    offsets: Vec<usize>,
}

impl Cursor {
    fn new(text: &str) -> Self {
        let chars: Vec<char> = text.chars().collect();
        let mut offsets = Vec::with_capacity(chars.len() + 1);
        let mut offset = 0;
        for c in &chars {
            offsets.push(offset);
            offset += c.len_utf16();
        }
        offsets.push(offset);
        Self { chars, offsets }
    }

    fn at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    fn len(&self) -> usize {
        self.chars.len()
    }

    fn position(&self, index: usize) -> usize {
        self.offsets[index.min(self.chars.len())]
    }
}

pub fn parse_gnmi_path(text: &str) -> Result<GnmiPath, GnmiPathError> {
    let mut source = crate::js::js_trim(text);
    let mut offset =
        crate::js::utf16_len(text) - crate::js::utf16_len(crate::js::js_trim_start(text));
    let mut origin = None;
    if let Some(captures) = ORIGIN.captures(source) {
        let name = captures[1].to_owned();
        // Only ASCII matched, so byte and UTF-16 lengths agree.
        source = &source[name.len() + 1..];
        offset += name.len() + 1;
        origin = Some(name);
    }
    let cursor = Cursor::new(source);
    let at = |index: usize| offset + cursor.position(index);
    let mut elems = Vec::new();
    let mut i = 0;
    if cursor.at(0) == Some('/') {
        i += 1;
    }
    while i < cursor.len() {
        let mut name = String::new();
        while let Some(c) = cursor.at(i)
            && c != '/'
            && c != '['
        {
            if c == '\\' && i + 1 < cursor.len() {
                name.push(cursor.chars[i + 1]);
                i += 2;
                continue;
            }
            if c == ']' {
                return Err(error("Unexpected “]”.", at(i)));
            }
            name.push(c);
            i += 1;
        }
        if name.is_empty() {
            if cursor.at(i) == Some('[') {
                return Err(error("A key needs an element before it.", at(i)));
            }
            // Tolerate a trailing slash; anything else is an empty element.
            if i + 1 == cursor.len() && cursor.at(i) == Some('/') {
                break;
            }
            if i >= cursor.len() {
                break;
            }
            return Err(error("Empty path element.", at(i)));
        }
        let mut keys: Option<GnmiKeys> = None;
        while cursor.at(i) == Some('[') {
            let start = i;
            i += 1;
            let mut key = String::new();
            while let Some(c) = cursor.at(i)
                && c != '='
                && c != ']'
            {
                key.push(c);
                i += 1;
            }
            if cursor.at(i) != Some('=') {
                return Err(error("A key needs “name=value”.", at(start)));
            }
            i += 1;
            let mut value = String::new();
            let mut closed = false;
            while let Some(c) = cursor.at(i) {
                if c == '\\' && i + 1 < cursor.len() {
                    value.push(cursor.chars[i + 1]);
                    i += 2;
                    continue;
                }
                if c == ']' {
                    closed = true;
                    i += 1;
                    break;
                }
                value.push(c);
                i += 1;
            }
            if !closed {
                return Err(error("Missing “]”.", at(start)));
            }
            let key = crate::js::js_trim(&key);
            if key.is_empty() {
                return Err(error("A key needs a name.", at(start)));
            }
            keys.get_or_insert_with(GnmiKeys::default)
                .insert(key, value);
        }
        elems.push(GnmiPathElem { name, keys });
        if let Some(c) = cursor.at(i) {
            if c != '/' {
                return Err(error(format!("Unexpected “{c}”."), at(i)));
            }
            i += 1;
        }
    }
    Ok(GnmiPath { origin, elems })
}

/// Parse, or `None` when the text is not a valid path.
pub fn try_parse_gnmi_path(text: &str) -> Option<GnmiPath> {
    parse_gnmi_path(text).ok()
}

fn escape_key_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn escape_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '\\' | '/' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn format_gnmi_elem(elem: &GnmiPathElem) -> String {
    let mut text = escape_name(&elem.name);
    for (key, value) in elem.keys.iter().flat_map(GnmiKeys::iter) {
        text.push_str(&format!("[{key}={}]", escape_key_value(value)));
    }
    text
}

pub fn format_gnmi_path(path: &GnmiPath) -> String {
    let body = format!(
        "/{}",
        path.elems
            .iter()
            .map(format_gnmi_elem)
            .collect::<Vec<_>>()
            .join("/")
    );
    match path.origin.as_deref().filter(|origin| !origin.is_empty()) {
        Some(origin) => format!("{origin}:{body}"),
        None => body,
    }
}

/// Normalize how a path is written, keeping the (trimmed) text when it does not parse.
pub fn normalize_gnmi_path(text: &str) -> String {
    match try_parse_gnmi_path(text) {
        Some(parsed) => format_gnmi_path(&parsed),
        None => crate::js::js_trim(text).to_owned(),
    }
}

/// `prefix` + `path`, as a device resolves an update path against a notification prefix.
pub fn join_gnmi_paths(prefix: Option<&GnmiPath>, path: &GnmiPath) -> GnmiPath {
    let Some(prefix) = prefix else {
        return path.clone();
    };
    let nonempty = |origin: &Option<String>| origin.clone().filter(|origin| !origin.is_empty());
    GnmiPath {
        origin: nonempty(&prefix.origin).or_else(|| nonempty(&path.origin)),
        elems: prefix.elems.iter().chain(&path.elems).cloned().collect(),
    }
}

/// `srl_nokia-interfaces:interface` → `interface`.
pub fn strip_module_prefix(name: &str) -> &str {
    match name.find(':') {
        Some(colon) => &name[colon + 1..],
        None => name,
    }
}

/// The path without module prefixes, so the same node compares equal whether
/// a device qualified its first element or not.
pub fn gnmi_path_identity(path: &GnmiPath) -> String {
    format_gnmi_path(&GnmiPath {
        origin: None,
        elems: path
            .elems
            .iter()
            .map(|elem| GnmiPathElem {
                name: strip_module_prefix(&elem.name).to_owned(),
                keys: elem.keys.clone(),
            })
            .collect(),
    })
}

/// The schema node a data path addresses: the path with its keys dropped.
pub fn gnmi_schema_path(path: &GnmiPath) -> String {
    format!(
        "/{}",
        path.elems
            .iter()
            .map(|elem| strip_module_prefix(&elem.name))
            .collect::<Vec<_>>()
            .join("/")
    )
}

/// Whether `path` lies at or below `ancestor` (keys in `ancestor` must match;
/// absent keys and `*` match any).
pub fn gnmi_path_starts_with(path: &GnmiPath, ancestor: &GnmiPath) -> bool {
    if ancestor.elems.len() > path.elems.len() {
        return false;
    }
    ancestor
        .elems
        .iter()
        .zip(&path.elems)
        .all(|(elem, candidate)| {
            if strip_module_prefix(&candidate.name) != strip_module_prefix(&elem.name) {
                return false;
            }
            elem.keys
                .iter()
                .flat_map(GnmiKeys::iter)
                .all(|(key, value)| value == "*" || candidate.key(key) == Some(value))
        })
}

/// A device's "unknown element 'x'. Options are [a, b, c]" split apart.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PathOptions {
    /// The rejected element.
    pub bad: Option<String>,
    /// The alternatives the device offered.
    pub options: Vec<String>,
}

static UNKNOWN_ELEMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u:unknown element )'([^']+)'").expect("valid regex"));
static OPTIONS_ARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u:options are )\[([^\]]*)\]").expect("valid regex"));

/// A device's "unknown element 'x'. Options are [a, b, c]" (SR Linux words
/// it this way) split into the rejected element and the alternatives.
pub fn split_path_options(message: &str) -> PathOptions {
    let bad = UNKNOWN_ELEMENT
        .captures(message)
        .map(|captures| captures[1].to_owned());
    let options = OPTIONS_ARE
        .captures(message)
        .map(|captures| {
            captures[1]
                .split(',')
                .map(|option| crate::js::js_trim(option).to_owned())
                .filter(|option| !option.is_empty())
                .collect()
        })
        .unwrap_or_default();
    PathOptions { bad, options }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(entries: &[(&str, &str)]) -> Option<GnmiKeys> {
        Some(entries.iter().map(|(key, value)| (*key, *value)).collect())
    }

    fn elem(name: &str, entries: &[(&str, &str)]) -> GnmiPathElem {
        GnmiPathElem {
            name: name.into(),
            keys: if entries.is_empty() {
                None
            } else {
                keys(entries)
            },
        }
    }

    fn parse(text: &str) -> GnmiPath {
        parse_gnmi_path(text).unwrap()
    }

    #[test]
    fn parses_elements_and_keys_keeping_slashes_inside_key_values() {
        assert_eq!(
            parse("/interface[name=ethernet-1/1]/subinterface[index=0]/statistics"),
            GnmiPath {
                origin: None,
                elems: vec![
                    elem("interface", &[("name", "ethernet-1/1")]),
                    elem("subinterface", &[("index", "0")]),
                    elem("statistics", &[]),
                ],
            }
        );
    }

    #[test]
    fn reads_several_keys_module_prefixes_and_an_origin() {
        let path = parse("openconfig:/srl_nokia-acl:acl/acl-filter[name=cpm][type=ipv4]");
        assert_eq!(
            path,
            GnmiPath {
                origin: Some("openconfig".into()),
                elems: vec![
                    elem("srl_nokia-acl:acl", &[]),
                    elem("acl-filter", &[("name", "cpm"), ("type", "ipv4")])
                ],
            }
        );
        assert_eq!(
            serde_json::to_string(&path).unwrap(),
            r#"{"origin":"openconfig","elems":[{"name":"srl_nokia-acl:acl"},{"name":"acl-filter","keys":{"name":"cpm","type":"ipv4"}}]}"#
        );
    }

    #[test]
    fn treats_a_colon_without_a_slash_after_it_as_part_of_the_element_name() {
        assert_eq!(
            parse("srl_nokia-interfaces:interface").elems,
            vec![elem("srl_nokia-interfaces:interface", &[])]
        );
    }

    #[test]
    fn round_trips_escaped_brackets_and_backslashes_in_key_values() {
        let path = GnmiPath {
            origin: None,
            elems: vec![elem("policy", &[("name", "a]b\\c")])],
        };
        let text = format_gnmi_path(&path);
        assert_eq!(text, "/policy[name=a\\]b\\\\c]");
        assert_eq!(parse(&text), path);
    }

    #[test]
    fn accepts_the_root_a_missing_leading_slash_and_a_trailing_slash() {
        assert_eq!(parse("/"), GnmiPath::default());
        assert_eq!(parse(""), GnmiPath::default());
        assert_eq!(
            parse("system/name/").elems,
            vec![elem("system", &[]), elem("name", &[])]
        );
        assert_eq!(normalize_gnmi_path("system/name/"), "/system/name");
    }

    #[test]
    fn reports_malformed_paths_with_a_position() {
        assert!(parse_gnmi_path("/interface[name=x").is_err());
        assert!(
            parse_gnmi_path("/a//b")
                .unwrap_err()
                .message
                .contains("Empty path element")
        );
        assert!(
            parse_gnmi_path("/interface[name]")
                .unwrap_err()
                .message
                .contains("name=value")
        );
        assert_eq!(parse_gnmi_path("/a/b]").unwrap_err().position, 4);
        // Positions count UTF-16 units and the trimmed leading whitespace.
        assert_eq!(parse_gnmi_path("  /😀/b]").unwrap_err().position, 7);
        assert_eq!(parse_gnmi_path("o:/a]").unwrap_err().position, 4);
    }

    #[test]
    fn joins_prefixes_and_compares_paths_regardless_of_module_prefixes() {
        let joined = join_gnmi_paths(
            Some(&parse("/network-instance[name=default]")),
            &parse("/protocols/bgp"),
        );
        assert_eq!(
            format_gnmi_path(&joined),
            "/network-instance[name=default]/protocols/bgp"
        );
        assert_eq!(
            gnmi_path_identity(&parse("/srl_nokia-interfaces:interface[name=e1]")),
            "/interface[name=e1]"
        );
        assert_eq!(
            gnmi_schema_path(&parse("/interface[name=e1]/subinterface[index=0]")),
            "/interface/subinterface"
        );
        assert!(gnmi_path_starts_with(
            &parse("/srl_nokia-interfaces:interface[name=e1]/statistics"),
            &parse("/interface[name=*]")
        ));
        assert!(!gnmi_path_starts_with(
            &parse("/interface[name=e2]"),
            &parse("/interface[name=e1]")
        ));
    }

    #[test]
    fn splits_a_devices_list_of_valid_elements_out_of_its_error_message() {
        assert_eq!(
            split_path_options(
                "Path not valid - unknown element 'nope'. Options are [system, tunnel, interface]"
            ),
            PathOptions {
                bad: Some("nope".into()),
                options: vec!["system".into(), "tunnel".into(), "interface".into()]
            }
        );
        assert_eq!(
            split_path_options("permission denied"),
            PathOptions::default()
        );
    }
}
