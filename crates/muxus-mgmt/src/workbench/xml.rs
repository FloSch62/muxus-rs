//! NETCONF XML on the UI side (port of `client/src/management/xml.ts`):
//! replies are parsed into a small DOM for the tree and error views,
//! pretty-printed for the raw view, and requests are assembled from editor
//! state.
//!
//! The DOM keeps what the browser's `DOMParser`/`XMLSerializer` pair would:
//! qualified names and attributes as written, namespaces resolved, text,
//! CDATA and comments in order.

use std::sync::LazyLock;

use muxus_core::js::{is_js_whitespace, js_trim};
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use regex::Regex;

use super::data_tree::{DataNode, DataNodeKind};

pub const NETCONF_BASE_NS: &str = "urn:ietf:params:xml:ns:netconf:base:1.0";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

pub fn escape_xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// A node of a parsed document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlNode {
    Element(XmlElement),
    Text(String),
    CData(String),
    Comment(String),
    ProcessingInstruction(String),
}

/// An element: its name and attributes as written, its namespace resolved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct XmlElement {
    /// Qualified name (`nc:rpc-reply`), like `Element.tagName`.
    pub name: String,
    /// Attributes in document order, namespace declarations included.
    pub attributes: Vec<(String, String)>,
    /// `Element.namespaceURI`.
    pub namespace: Option<String>,
    pub children: Vec<XmlNode>,
}

impl XmlElement {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// `Element.localName`.
    pub fn local_name(&self) -> &str {
        match self.name.find(':') {
            Some(colon) => &self.name[colon + 1..],
            None => &self.name,
        }
    }

    pub fn prefix(&self) -> Option<&str> {
        self.name.find(':').map(|colon| &self.name[..colon])
    }

    /// `Element.children`: the element children only.
    pub fn elements(&self) -> impl Iterator<Item = &XmlElement> {
        self.children.iter().filter_map(|node| match node {
            XmlNode::Element(element) => Some(element),
            _ => None,
        })
    }

    pub fn has_elements(&self) -> bool {
        self.elements().next().is_some()
    }

    /// `Node.textContent`: every text and CDATA descendant, in order.
    pub fn text_content(&self) -> String {
        let mut out = String::new();
        self.collect_text(&mut out);
        out
    }

    fn collect_text(&self, out: &mut String) {
        for child in &self.children {
            match child {
                XmlNode::Text(text) | XmlNode::CData(text) => out.push_str(text),
                XmlNode::Element(element) => element.collect_text(out),
                _ => {}
            }
        }
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn has_attribute(&self, name: &str) -> bool {
        self.attribute(name).is_some()
    }

    pub fn set_attribute(&mut self, name: &str, value: &str) {
        match self.attributes.iter_mut().find(|(key, _)| key == name) {
            Some(entry) => entry.1 = value.to_owned(),
            None => self.attributes.push((name.to_owned(), value.to_owned())),
        }
    }

    /// `getElementsByTagName(name).length`: descendants with that qualified name.
    pub fn count_descendants_named(&self, name: &str) -> usize {
        self.elements()
            .map(|element| {
                usize::from(name == "*" || element.name == name)
                    + element.count_descendants_named(name)
            })
            .sum()
    }
}

/// A parsed document: its element and the text it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlDocument {
    pub root: XmlElement,
}

/// Namespace bindings in scope while parsing or serializing.
#[derive(Default, Clone)]
struct Scope {
    /// Prefix (`None` = default namespace) → URI; an empty URI undeclares.
    bindings: Vec<(Option<String>, String)>,
}

impl Scope {
    fn lookup(&self, prefix: Option<&str>) -> Option<&str> {
        if prefix == Some("xml") {
            return Some(XML_NS);
        }
        self.bindings
            .iter()
            .rev()
            .find(|(bound, _)| bound.as_deref() == prefix)
            .map(|(_, uri)| uri.as_str())
            .filter(|uri| !uri.is_empty())
    }
}

fn declaration(name: &str) -> Option<Option<String>> {
    if name == "xmlns" {
        Some(None)
    } else {
        name.strip_prefix("xmlns:")
            .map(|prefix| Some(prefix.to_owned()))
    }
}

fn open_element(start: &BytesStart<'_>, scope: &mut Scope) -> Result<(XmlElement, usize), String> {
    let name = String::from_utf8_lossy(start.name().as_ref()).into_owned();
    let mark = scope.bindings.len();
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|err| err.to_string())?;
        let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|err| err.to_string())?
            .into_owned();
        if let Some(prefix) = declaration(&key) {
            scope.bindings.push((prefix, value.clone()));
        }
        attributes.push((key, value));
    }
    let prefix = name.find(':').map(|colon| &name[..colon]);
    let namespace = scope.lookup(prefix).map(str::to_owned);
    if let Some(prefix) = prefix
        && namespace.is_none()
    {
        return Err(format!(
            "Namespace prefix {prefix} on {name} is not defined"
        ));
    }
    for (key, _) in &attributes {
        if let Some(colon) = key.find(':') {
            let prefix = &key[..colon];
            if prefix != "xmlns" && scope.lookup(Some(prefix)).is_none() {
                return Err(format!(
                    "Namespace prefix {prefix} for {} on {name} is not defined",
                    &key[colon + 1..]
                ));
            }
        }
    }
    Ok((
        XmlElement {
            name,
            attributes,
            namespace,
            children: Vec::new(),
        },
        mark,
    ))
}

fn push_text(element: &mut XmlElement, text: &str) {
    if let Some(XmlNode::Text(previous)) = element.children.last_mut() {
        previous.push_str(text);
    } else {
        element.children.push(XmlNode::Text(text.to_owned()));
    }
}

fn predefined_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "apos" => "'",
        "quot" => "\"",
        _ => return None,
    })
}

/// Parse a whole document; the error is a one-line description.
pub fn parse_document(text: &str) -> Result<XmlDocument, String> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut scope = Scope::default();
    let mut stack: Vec<(XmlElement, usize)> = Vec::new();
    let mut root: Option<XmlElement> = None;
    let position = |reader: &Reader<&[u8]>| reader.buffer_position();
    loop {
        let event = reader
            .read_event()
            .map_err(|err| format!("{err} (at byte {})", position(&reader)))?;
        match event {
            Event::Start(start) | Event::Empty(start) if stack.is_empty() && root.is_some() => {
                let _ = start;
                return Err("Extra content at the end of the document".into());
            }
            Event::Start(start) => stack.push(open_element(&start, &mut scope)?),
            Event::Empty(start) => {
                let (element, mark) = open_element(&start, &mut scope)?;
                scope.bindings.truncate(mark);
                match stack.last_mut() {
                    Some((parent, _)) => parent.children.push(XmlNode::Element(element)),
                    None => root = Some(element),
                }
            }
            Event::End(_) => {
                let (element, mark) = stack.pop().ok_or_else(|| "Unexpected end tag".to_owned())?;
                scope.bindings.truncate(mark);
                match stack.last_mut() {
                    Some((parent, _)) => parent.children.push(XmlNode::Element(element)),
                    None => root = Some(element),
                }
            }
            Event::Text(text) => {
                let text = text.xml10_content().map_err(|err| err.to_string())?;
                match stack.last_mut() {
                    Some((parent, _)) => push_text(parent, &text),
                    None if text.chars().all(char::is_whitespace) => {}
                    None => {
                        return Err("Content is not allowed outside the document element".into());
                    }
                }
            }
            Event::GeneralRef(reference) => {
                let name = reference.decode().map_err(|err| err.to_string())?;
                let resolved = match reference
                    .resolve_char_ref()
                    .map_err(|err| err.to_string())?
                {
                    Some(c) => c.to_string(),
                    None => predefined_entity(&name)
                        .ok_or_else(|| format!("Entity '{name}' not defined"))?
                        .to_owned(),
                };
                match stack.last_mut() {
                    Some((parent, _)) => push_text(parent, &resolved),
                    None => {
                        return Err("Content is not allowed outside the document element".into());
                    }
                }
            }
            Event::CData(data) => {
                let data = String::from_utf8_lossy(&data.into_inner()).into_owned();
                match stack.last_mut() {
                    Some((parent, _)) => parent.children.push(XmlNode::CData(data)),
                    None => return Err("CDATA is not allowed outside the document element".into()),
                }
            }
            Event::Comment(comment) => {
                if let Some((parent, _)) = stack.last_mut() {
                    let comment = String::from_utf8_lossy(&comment.into_inner()).into_owned();
                    parent.children.push(XmlNode::Comment(comment));
                }
            }
            Event::PI(instruction) => {
                if let Some((parent, _)) = stack.last_mut() {
                    let instruction =
                        String::from_utf8_lossy(&instruction.into_inner()).into_owned();
                    parent
                        .children
                        .push(XmlNode::ProcessingInstruction(instruction));
                }
            }
            Event::Decl(_) | Event::DocType(_) => {}
            Event::Eof => break,
        }
    }
    if let Some((element, _)) = stack.last() {
        return Err(format!(
            "Opening and ending tag mismatch: {} is not closed",
            element.name
        ));
    }
    root.map(|root| XmlDocument { root })
        .ok_or_else(|| "Document is empty".into())
}

/// The browser's answer to `parseFromString`: a document or the parser's complaint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlParseResult {
    pub document: Option<XmlDocument>,
    pub error: Option<String>,
}

pub fn parse_xml(text: &str) -> XmlParseResult {
    match parse_document(text) {
        Ok(document) => XmlParseResult {
            document: Some(document),
            error: None,
        },
        Err(error) => XmlParseResult {
            document: None,
            error: Some(error.trim().to_owned()),
        },
    }
}

/// Check a fragment (several top-level elements allowed) by wrapping it.
pub fn xml_fragment_error(text: &str) -> Option<String> {
    if js_trim(text).is_empty() {
        return None;
    }
    parse_xml(&format!("<muxus-fragment>{text}</muxus-fragment>")).error
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attribute(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

/// `XMLSerializer.serializeToString(element)`: as written, plus the
/// namespace declarations the element relied on from outside itself.
pub fn serialize_element(element: &XmlElement) -> String {
    let mut out = String::new();
    serialize_into(element, &Scope::default(), &mut out);
    out
}

fn serialize_into(element: &XmlElement, inherited: &Scope, out: &mut String) {
    let mut scope = inherited.clone();
    let mut declared = Vec::new();
    for (key, value) in &element.attributes {
        if let Some(prefix) = declaration(key) {
            scope.bindings.push((prefix.clone(), value.clone()));
            declared.push(prefix);
        }
    }
    out.push('<');
    out.push_str(&element.name);
    let prefix = element.prefix().map(str::to_owned);
    match &element.namespace {
        Some(namespace)
            if !declared.contains(&prefix)
                && scope.lookup(prefix.as_deref()) != Some(namespace) =>
        {
            match &prefix {
                Some(prefix) => out.push_str(&format!(
                    " xmlns:{prefix}=\"{}\"",
                    escape_attribute(namespace)
                )),
                None => out.push_str(&format!(" xmlns=\"{}\"", escape_attribute(namespace))),
            }
            scope.bindings.push((prefix.clone(), namespace.clone()));
        }
        None if prefix.is_none() && !declared.contains(&None) && scope.lookup(None).is_some() => {
            out.push_str(" xmlns=\"\"");
            scope.bindings.push((None, String::new()));
        }
        _ => {}
    }
    for (key, value) in &element.attributes {
        out.push_str(&format!(" {key}=\"{}\"", escape_attribute(value)));
    }
    if element.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &element.children {
        match child {
            XmlNode::Element(child) => serialize_into(child, &scope, out),
            XmlNode::Text(text) => out.push_str(&escape_text(text)),
            XmlNode::CData(text) => out.push_str(&format!("<![CDATA[{text}]]>")),
            XmlNode::Comment(text) => out.push_str(&format!("<!--{text}-->")),
            XmlNode::ProcessingInstruction(text) => out.push_str(&format!("<?{text}?>")),
        }
    }
    out.push_str(&format!("</{}>", element.name));
}

static DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*<\?xml[^>]*\?>").expect("valid regex"));

/// Indent XML for reading; text-only elements stay on one line. Unparseable input is returned as is.
pub fn pretty_xml(text: &str) -> String {
    pretty_xml_with(text, "  ")
}

pub fn pretty_xml_with(text: &str, indent: &str) -> String {
    let Ok(document) = parse_document(text) else {
        return text.to_owned();
    };
    let mut lines = Vec::new();
    if let Some(declaration) = DECLARATION.find(text) {
        lines.push(declaration.as_str().trim().to_owned());
    }
    pretty_element(&document.root, 0, indent, &mut lines);
    lines.join("\n")
}

fn pretty_element(element: &XmlElement, depth: usize, indent: &str, lines: &mut Vec<String>) {
    let pad = indent.repeat(depth);
    let children: Vec<&XmlNode> = element
        .children
        .iter()
        .filter(|child| match child {
            XmlNode::Element(_) | XmlNode::Comment(_) | XmlNode::CData(_) => true,
            XmlNode::Text(text) => !js_trim(text).is_empty(),
            XmlNode::ProcessingInstruction(_) => false,
        })
        .collect();
    let attributes: String = element
        .attributes
        .iter()
        .map(|(key, value)| format!(" {key}=\"{}\"", escape_xml(value)))
        .collect();
    let open = format!("<{}{attributes}", element.name);
    if children.is_empty() {
        lines.push(format!("{pad}{open}/>"));
        return;
    }
    if children
        .iter()
        .all(|child| matches!(child, XmlNode::Text(_)))
    {
        lines.push(format!(
            "{pad}{open}>{}</{}>",
            escape_xml(js_trim(&element.text_content())),
            element.name
        ));
        return;
    }
    lines.push(format!("{pad}{open}>"));
    let inner = indent.repeat(depth + 1);
    for child in children {
        match child {
            XmlNode::Element(child) => pretty_element(child, depth + 1, indent, lines),
            XmlNode::Text(text) => lines.push(format!("{inner}{}", escape_xml(js_trim(text)))),
            XmlNode::CData(text) => lines.push(format!("{inner}<![CDATA[{text}]]>")),
            XmlNode::Comment(text) => lines.push(format!("{inner}<!--{text}-->")),
            XmlNode::ProcessingInstruction(_) => {}
        }
    }
    lines.push(format!("{pad}</{}>", element.name));
}

/// One `<rpc-error>`, its fields collapsed to single lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RpcErrorInfo {
    pub error_type: Option<String>,
    pub tag: Option<String>,
    pub severity: Option<String>,
    pub path: Option<String>,
    pub message: Option<String>,
    /// `<error-info>` children as name/value pairs.
    pub info: Vec<(String, String)>,
}

/// A parsed `<rpc-reply>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RpcReply {
    pub ok: bool,
    /// `<data>`, for retrievals.
    pub data: Option<XmlElement>,
    pub errors: Vec<RpcErrorInfo>,
    /// Reply content that is neither `<ok/>`, `<data>` nor `<rpc-error>`.
    pub other: Vec<XmlElement>,
}

/// Whitespace runs as one space, trimmed (`text.replace(/\s+/g, ' ').trim()`).
pub(crate) fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for c in text.chars() {
        if is_js_whitespace(c) {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

fn child_text(element: &XmlElement, name: &str) -> Option<String> {
    let child = element
        .elements()
        .find(|child| child.local_name() == name)?;
    let text = collapse_whitespace(&child.text_content());
    (!text.is_empty()).then_some(text)
}

pub fn parse_rpc_reply(xml: &str) -> Option<RpcReply> {
    let document = parse_document(xml).ok()?;
    let root = document.root;
    if root.local_name() != "rpc-reply" {
        return None;
    }
    let mut reply = RpcReply::default();
    for child in root.elements() {
        match child.local_name() {
            "ok" => reply.ok = true,
            "data" => reply.data = Some(child.clone()),
            "rpc-error" => {
                let info = child
                    .elements()
                    .find(|candidate| candidate.local_name() == "error-info");
                reply.errors.push(RpcErrorInfo {
                    error_type: child_text(child, "error-type"),
                    tag: child_text(child, "error-tag"),
                    severity: child_text(child, "error-severity"),
                    path: child_text(child, "error-path"),
                    message: child_text(child, "error-message"),
                    info: info
                        .map(|info| {
                            info.elements()
                                .map(|item| {
                                    (
                                        item.local_name().to_owned(),
                                        collapse_whitespace(&item.text_content()),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                });
            }
            _ => reply.other.push(child.clone()),
        }
    }
    Some(reply)
}

/// `<rpc>…</rpc>` pasted whole: keep what is inside, carrying namespace declarations down.
pub fn unwrap_rpc(text: &str) -> String {
    let Ok(document) = parse_document(js_trim(text)) else {
        return text.to_owned();
    };
    let root = document.root;
    if root.local_name() != "rpc" {
        return text.to_owned();
    }
    let declarations: Vec<&(String, String)> = root
        .attributes
        .iter()
        .filter(|(key, value)| key.starts_with("xmlns:") && value != NETCONF_BASE_NS)
        .collect();
    let base = format!(" xmlns=\"{NETCONF_BASE_NS}\"");
    root.elements()
        .map(|child| {
            let mut child = child.clone();
            for (key, value) in &declarations {
                if !child.has_attribute(key) {
                    child.set_attribute(key, value);
                }
            }
            serialize_element(&child).replacen(&base, "", 1)
        })
        .collect()
}

/// The full message as it goes on the wire, for copying.
pub fn rpc_envelope(body: &str, message_id: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rpc message-id=\"{message_id}\" xmlns=\"{NETCONF_BASE_NS}\">\n{body}\n</rpc>"
    )
}

/// A node without a namespace of its own inherits its nearest ancestor's.
fn effective_namespace<'a>(chain: &[&'a DataNode], index: usize) -> Option<&'a str> {
    (0..=index)
        .rev()
        .find_map(|at| chain[at].namespace.as_deref())
}

fn parent_element_index(chain: &[&DataNode], index: usize) -> Option<usize> {
    (0..index)
        .rev()
        .find(|at| chain[*at].kind != DataNodeKind::List)
}

fn key_elements(node: &DataNode) -> String {
    node.keys
        .iter()
        .flat_map(|keys| keys.iter())
        .map(|(key, value)| format!("<{key}>{}</{key}>", escape_xml(value)))
        .collect()
}

fn local_name(node: &DataNode) -> &str {
    match node.name.find(':') {
        Some(colon) => &node.name[colon + 1..],
        None => &node.name,
    }
}

/// A subtree filter that selects the last node of `chain` (a node of a
/// NETCONF tree and its ancestors): every ancestor with its namespace, list
/// entries narrowed by their key leaves. `inner` replaces the target's own
/// content (empty selects all of it).
pub fn subtree_filter_for(chain: &[&DataNode], inner: &str) -> String {
    let mut xml = inner.to_owned();
    for index in (0..chain.len()).rev() {
        let node = chain[index];
        // A list is written through its entries; as the target it selects them all.
        if node.kind == DataNodeKind::List && index != chain.len() - 1 {
            continue;
        }
        let name = local_name(node);
        let namespace = effective_namespace(chain, index);
        // List nodes are not elements: the parent element is the nearest non-list ancestor.
        let parent_namespace = parent_element_index(chain, index)
            .and_then(|parent| effective_namespace(chain, parent));
        let xmlns = match namespace {
            Some(namespace) if Some(namespace) != parent_namespace => {
                format!(" xmlns=\"{}\"", escape_xml(namespace))
            }
            _ => String::new(),
        };
        let content = if node.kind == DataNodeKind::Entry {
            key_elements(node) + &xml
        } else {
            xml
        };
        xml = if content.is_empty() {
            format!("<{name}{xmlns}/>")
        } else {
            format!("<{name}{xmlns}>{content}</{name}>")
        };
    }
    xml
}

/// An XPath that selects the node, for servers with :xpath.
pub fn xpath_for(chain: &[&DataNode]) -> String {
    let steps: Vec<String> = chain
        .iter()
        .enumerate()
        .filter(|(index, node)| node.kind != DataNodeKind::List || *index == chain.len() - 1)
        .map(|(_, node)| {
            let predicates: String = node
                .keys
                .iter()
                .flat_map(|keys| keys.iter())
                .map(|(key, value)| {
                    if value.contains('\'') {
                        format!("[{key}=\"{value}\"]")
                    } else {
                        format!("[{key}='{value}']")
                    }
                })
                .collect();
            format!("{}{predicates}", local_name(node))
        })
        .collect();
    format!("/{}", steps.join("/"))
}

/// The config subtree of a node wrapped in its ancestors, for edit-config.
pub fn config_skeleton_for(chain: &[&DataNode], leaf_value: Option<&str>) -> String {
    let Some(target) = chain.last() else {
        return String::new();
    };
    let inner = leaf_value.map(escape_xml).unwrap_or_default();
    if target.kind == DataNodeKind::Leaf {
        let name = &target.name;
        return subtree_filter_for(
            &chain[..chain.len() - 1],
            &format!("<{name}>{inner}</{name}>"),
        );
    }
    subtree_filter_for(chain, "")
}

fn scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Number(number) => super::data_tree::js_number_text(number),
        serde_json::Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    }
}

/// A tree node and everything below it as XML, the way the device would send it.
pub fn data_node_xml(node: &DataNode, inherited_namespace: Option<&str>, indent: &str) -> String {
    let namespace = node.namespace.as_deref().or(inherited_namespace);
    let xmlns = match node.namespace.as_deref() {
        Some(own) if Some(own) != inherited_namespace => format!(" xmlns=\"{}\"", escape_xml(own)),
        _ => String::new(),
    };
    let name = local_name(node);
    match node.kind {
        DataNodeKind::List => node
            .children
            .iter()
            .map(|entry| data_node_xml(entry, inherited_namespace, indent))
            .collect::<Vec<_>>()
            .join("\n"),
        DataNodeKind::Leaf => format!(
            "{indent}<{name}{xmlns}>{}</{name}>",
            escape_xml(&node.value.as_ref().map(scalar_text).unwrap_or_default())
        ),
        DataNodeKind::LeafList => {
            let values: Vec<serde_json::Value> = match &node.value {
                Some(serde_json::Value::Array(values)) => values.clone(),
                Some(value) => vec![value.clone()],
                None => vec![serde_json::Value::Null],
            };
            values
                .iter()
                .map(|value| {
                    format!(
                        "{indent}<{name}{xmlns}>{}</{name}>",
                        escape_xml(&scalar_text(value))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            let deeper = format!("{indent}  ");
            let inner = node
                .children
                .iter()
                .map(|child| data_node_xml(child, namespace, &deeper))
                .collect::<Vec<_>>()
                .join("\n");
            if inner.is_empty() {
                format!("{indent}<{name}{xmlns}/>")
            } else {
                format!("{indent}<{name}{xmlns}>\n{inner}\n{indent}</{name}>")
            }
        }
    }
}

/// Whether [`edit_config_for`] writes the node's content or deletes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    Edit,
    Delete,
}

/// `prettyXml(<muxus-wrap>…</muxus-wrap>)` without the wrapper lines and their indent.
pub(crate) fn pretty_fragment(xml: &str) -> String {
    let pretty = pretty_xml(&format!("<muxus-wrap>{xml}</muxus-wrap>"));
    let lines: Vec<&str> = pretty.split('\n').collect();
    if lines.len() < 2 {
        return String::new();
    }
    lines[1..lines.len() - 1]
        .iter()
        .map(|line| line.get(2..).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// edit-config content for a node: its ancestors (list entries with their
/// keys) around either the node's current content or a delete operation.
pub fn edit_config_for(chain: &[&DataNode], mode: EditMode) -> String {
    let Some(target) = chain.last() else {
        return String::new();
    };
    let ancestors = &chain[..chain.len() - 1];
    let inherited = parent_element_index(chain, chain.len() - 1)
        .and_then(|parent| effective_namespace(chain, parent));
    let inner = match mode {
        EditMode::Delete => {
            let name = local_name(target);
            let namespace = match target.namespace.as_deref() {
                Some(own) if Some(own) != inherited => format!(" xmlns=\"{}\"", escape_xml(own)),
                _ => String::new(),
            };
            let keys = if target.kind == DataNodeKind::Entry {
                key_elements(target)
            } else {
                String::new()
            };
            format!(
                "<{name}{namespace} xmlns:nc=\"{NETCONF_BASE_NS}\" nc:operation=\"delete\">{keys}</{name}>"
            )
        }
        EditMode::Edit => data_node_xml(target, inherited, "").trim().to_owned(),
    };
    pretty_fragment(&subtree_filter_for(ancestors, &inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rpc_errors_with_their_details() {
        let reply = parse_rpc_reply(
            "<rpc-reply message-id=\"3\" xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><rpc-error>\
             <error-type>application</error-type><error-tag>unknown-element</error-tag>\
             <error-severity>error</error-severity><error-message>Unknown element</error-message>\
             <error-info><bad-element>bogus</bad-element></error-info></rpc-error></rpc-reply>",
        )
        .unwrap();
        assert!(!reply.ok);
        assert_eq!(reply.errors[0].tag.as_deref(), Some("unknown-element"));
        assert_eq!(reply.errors[0].message.as_deref(), Some("Unknown element"));
        assert_eq!(
            reply.errors[0].info,
            vec![("bad-element".to_owned(), "bogus".to_owned())]
        );
        assert!(parse_rpc_reply("<rpc-reply><ok/></rpc-reply>").unwrap().ok);
        assert!(parse_rpc_reply("<hello/>").is_none());
    }

    #[test]
    fn pretty_prints_checks_fragments_and_unwraps_a_pasted_rpc() {
        assert_eq!(
            pretty_xml("<a><b>1</b><c/></a>"),
            "<a>\n  <b>1</b>\n  <c/>\n</a>"
        );
        assert_eq!(xml_fragment_error("<a/><b/>"), None);
        assert!(xml_fragment_error("<a>").is_some());
        assert_eq!(
            unwrap_rpc(
                "<rpc message-id=\"1\" xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><get/></rpc>"
            ),
            "<get/>"
        );
    }

    #[test]
    fn carries_namespace_declarations_into_unwrapped_operations() {
        let unwrapped = unwrap_rpc(
            "<nc:rpc xmlns:nc=\"urn:ietf:params:xml:ns:netconf:base:1.0\" xmlns:x=\"urn:x\" message-id=\"5\">\
             <nc:get-config><nc:source><nc:running/></nc:source><x:filter-hint>a &amp; b</x:filter-hint></nc:get-config></nc:rpc>",
        );
        assert_eq!(
            unwrapped,
            "<nc:get-config xmlns:nc=\"urn:ietf:params:xml:ns:netconf:base:1.0\" xmlns:x=\"urn:x\"><nc:source><nc:running/></nc:source><x:filter-hint>a &amp; b</x:filter-hint></nc:get-config>"
        );
        // A default namespace other than the base one stays.
        assert_eq!(
            unwrap_rpc(
                "<rpc xmlns=\"urn:ietf:params:xml:ns:netconf:base:1.0\"><get-schema xmlns=\"urn:mon\"><identifier>m</identifier></get-schema></rpc>"
            ),
            "<get-schema xmlns=\"urn:mon\"><identifier>m</identifier></get-schema>"
        );
        assert_eq!(unwrap_rpc("<get/>"), "<get/>");
    }

    #[test]
    fn rejects_what_a_browser_rejects() {
        assert!(parse_document("<a><b></a>").is_err());
        assert!(parse_document("<a/><b/>").is_err());
        assert!(parse_document("<x:a/>").is_err());
        assert!(parse_document("<a>&nope;</a>").is_err());
        let document =
            parse_document("<?xml version=\"1.0\"?>\n<a>&lt;&#65;<![CDATA[<b>]]><!--c--></a>")
                .unwrap();
        assert_eq!(document.root.text_content(), "<A<b>");
        assert_eq!(
            pretty_xml("<?xml version=\"1.0\"?><a><b x=\"1&amp;'\">t</b><!--c--></a>"),
            "<?xml version=\"1.0\"?>\n<a>\n  <b x=\"1&amp;&apos;\">t</b>\n  <!--c-->\n</a>"
        );
        assert_eq!(pretty_xml("<a"), "<a");
    }

    #[test]
    fn resolves_namespaces_like_the_dom() {
        let document = parse_document(
            "<data xmlns=\"urn:base\"><if:interface xmlns:if=\"urn:if\"><name>e1</name></if:interface></data>",
        )
        .unwrap();
        let interface = document.root.elements().next().unwrap();
        assert_eq!(interface.local_name(), "interface");
        assert_eq!(interface.namespace.as_deref(), Some("urn:if"));
        let name = interface.elements().next().unwrap();
        assert_eq!(name.namespace.as_deref(), Some("urn:base"));
        assert_eq!(
            serialize_element(name),
            "<name xmlns=\"urn:base\">e1</name>"
        );
        assert_eq!(document.root.count_descendants_named("name"), 1);
    }
}
