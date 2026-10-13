//! The little XML the backend has to understand: which reply a message
//! answers, and what a hello advertises. Everything else stays text for the
//! UI, which parses and renders it.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::NetconfError;

pub const NETCONF_BASE_NS: &str = "urn:ietf:params:xml:ns:netconf:base:1.0";
pub const NETCONF_BASE_1_0: &str = "urn:ietf:params:netconf:base:1.0";
pub const NETCONF_BASE_1_1: &str = "urn:ietf:params:netconf:base:1.1";

pub use crate::workbench::xml::escape_xml;

static ENTITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)&(#x[0-9a-f]+|#[0-9]+|lt|gt|amp|quot|apos);").expect("valid regex")
});

pub fn decode_xml_entities(text: &str) -> String {
    ENTITY
        .replace_all(text, |captures: &regex::Captures<'_>| {
            let entity = captures[1].to_lowercase();
            match entity.as_str() {
                "lt" => "<".to_owned(),
                "gt" => ">".to_owned(),
                "amp" => "&".to_owned(),
                "quot" => "\"".to_owned(),
                "apos" => "'".to_owned(),
                _ => {
                    let code = match entity.strip_prefix("#x") {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => entity[1..].parse::<u32>().ok(),
                    };
                    code.and_then(char::from_u32)
                        .map_or_else(|| captures[0].to_owned(), String::from)
                }
            }
        })
        .into_owned()
}

/// The document element's name (without its namespace prefix) and attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootElement {
    pub name: String,
    /// Attribute names without their prefix, except `xmlns` declarations.
    pub attributes: BTreeMap<String, String>,
}

static TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^<([A-Za-z_][A-Za-z0-9_.-]*(?::[A-Za-z_][A-Za-z0-9_.-]*)?)((?:\s+[^\s=/>]+\s*=\s*(?:"[^"]*"|'[^']*'))*)\s*/?>"#,
    )
    .expect("valid regex")
});
static ATTRIBUTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([^\s=/>]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("valid regex")
});

/// The document element's name and attributes, skipping the prolog.
pub fn root_element(xml: &str) -> Option<RootElement> {
    let mut index = 0;
    loop {
        index += xml[index..].len() - xml[index..].trim_start().len();
        let rest = &xml[index..];
        if rest.starts_with("<?") {
            index += rest.find("?>")? + 2;
            continue;
        }
        if rest.starts_with("<!--") {
            index += rest.find("-->")? + 3;
            continue;
        }
        if rest.starts_with("<!") {
            index += rest.find('>')? + 1;
            continue;
        }
        break;
    }
    let rest = &xml[index..];
    if !rest.starts_with('<') {
        return None;
    }
    let mut end = rest.len().min(8192);
    while !rest.is_char_boundary(end) {
        end -= 1;
    }
    let captures = TAG.captures(&rest[..end])?;
    let qualified = &captures[1];
    let attributes = ATTRIBUTE
        .captures_iter(&captures[2])
        .map(|attribute| {
            let name = &attribute[1];
            let local = if name.contains(':') && !name.starts_with("xmlns") {
                name.split(':').nth(1).unwrap_or(name)
            } else {
                name
            };
            let value = attribute
                .get(2)
                .or_else(|| attribute.get(3))
                .map_or("", |value| value.as_str());
            (local.to_owned(), decode_xml_entities(value))
        })
        .collect();
    Some(RootElement {
        name: qualified
            .split(':')
            .next_back()
            .unwrap_or(qualified)
            .to_owned(),
        attributes,
    })
}

fn element_texts(xml: &str, name: &str) -> Vec<String> {
    let pattern = Regex::new(&format!(
        r"<(?:[A-Za-z0-9_.-]+:)?{name}(?:\s[^>]*)?>([\s\S]*?)</(?:[A-Za-z0-9_.-]+:)?{name}\s*>",
        name = regex::escape(name)
    ))
    .expect("valid regex");
    pattern
        .captures_iter(xml)
        .map(|captures| decode_xml_entities(captures[1].trim()))
        .collect()
}

/// What a server's hello advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub capabilities: Vec<String>,
    pub session_id: Option<String>,
}

pub fn parse_hello(xml: &str) -> Result<Hello, NetconfError> {
    if root_element(xml).map(|root| root.name).as_deref() != Some("hello") {
        return Err(NetconfError::new(
            "The server did not start with a NETCONF <hello>.",
        ));
    }
    Ok(Hello {
        capabilities: element_texts(xml, "capability")
            .into_iter()
            .filter(|capability| !capability.is_empty())
            .collect(),
        session_id: element_texts(xml, "session-id")
            .into_iter()
            .next()
            .filter(|id| !id.is_empty()),
    })
}

pub fn client_hello() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><hello xmlns=\"{NETCONF_BASE_NS}\"><capabilities><capability>{NETCONF_BASE_1_0}</capability><capability>{NETCONF_BASE_1_1}</capability></capabilities></hello>"
    )
}

pub fn wrap_rpc(message_id: &str, body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><rpc message-id=\"{}\" xmlns=\"{NETCONF_BASE_NS}\">{body}</rpc>",
        escape_xml(message_id)
    )
}

pub fn notification_event_time(xml: &str) -> Option<String> {
    element_texts(xml, "eventTime").into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_document_element_past_the_prolog_with_unprefixed_attribute_names() {
        let root = root_element(
            "<?xml version=\"1.0\"?>\n<!-- c --><nc:rpc-reply nc:message-id=\"7\" xmlns:nc=\"x\">",
        )
        .unwrap();
        assert_eq!(root.name, "rpc-reply");
        assert_eq!(
            root.attributes,
            BTreeMap::from([
                ("message-id".to_owned(), "7".to_owned()),
                ("xmlns:nc".to_owned(), "x".to_owned()),
            ])
        );
    }

    #[test]
    fn reads_capabilities_and_the_session_id_from_a_hello() {
        let hello = parse_hello(
            "<hello xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><capabilities>\
             <capability>urn:ietf:params:netconf:base:1.1</capability>\
             <capability>urn:x?module=a&amp;revision=2020-01-01</capability>\
             </capabilities><session-id>42</session-id></hello>",
        )
        .unwrap();
        assert_eq!(
            hello,
            Hello {
                capabilities: vec![
                    "urn:ietf:params:netconf:base:1.1".into(),
                    "urn:x?module=a&revision=2020-01-01".into(),
                ],
                session_id: Some("42".into()),
            }
        );
        assert!(
            parse_hello("<rpc-reply/>")
                .unwrap_err()
                .message
                .contains("hello")
        );
        assert_eq!(decode_xml_entities("&lt;a&gt; &#65;&#x42;"), "<a> AB");
    }
}
