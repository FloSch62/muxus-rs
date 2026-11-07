//! A running lab's topology in a window of its own (`ClabTopologyDialog.tsx`).
//!
//! The TypeScript client frames the containerlab viewer (`@containerlab/clab-viewer`'s
//! `viewer.html`, served by the app under `/vendor/clab-viewer/`) in a dialog: the frame says
//! it is ready, gets the lab's topology file and annotations with the theme and drawing
//! options in return, and tells which node is selected; the dialog shows that node with a
//! button to open it. Muxus' windows have no web view, so the dialog becomes a window process
//! like the page windows ([`crate::protocol`]): it serves the same viewer build
//! (`assets/clab-viewer`) from a scheme of its own, frames it the same way, and draws the
//! dialog's header and node bar around it. Opening a node is an [`OpenNode`] event for the app,
//! after which the window closes, as the dialog does.
//!
//! [`OpenNode`]: crate::protocol::WebWindowEvent::OpenNode

use muxus_core::js::utf16_len;
use serde::{Deserialize, Serialize};

use crate::request::is_window_key;

/// Larger topology files than this are not sent to a window.
pub const MAX_TOPOLOGY_BYTES: usize = 8 * 1024 * 1024;

/// What a topology window shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyWindowRequest {
    /// One window per lab: opening the lab again brings its window to the front.
    pub key: String,
    /// The lab's name: the window's title and the header's first line.
    pub title: String,
    /// `<host> · <topology file>`, the header's second line.
    pub subtitle: String,
    /// The lab's `.clab.yml`.
    pub yaml: String,
    /// Its `.annotations.json`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<String>,
    pub theme: TopologyTheme,
    /// The lab's nodes, for the bar under the drawing.
    pub nodes: Vec<TopologyNode>,
}

/// The app's look, as CSS colors. The first five are the viewer's `appearance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyTheme {
    /// `light` or `dark`.
    pub mode: String,
    /// `background.default`.
    pub background: String,
    /// `text.primary`.
    pub foreground: String,
    /// `background.paper`.
    pub surface: String,
    /// `divider`.
    pub border: String,
    /// `primary.main`.
    pub accent: String,
    /// `primary.contrastText`: a contained button's label.
    pub accent_text: String,
    /// `text.secondary`.
    pub secondary: String,
    /// `text.disabled`: a stopped node's ring.
    pub disabled: String,
    /// `success.main`: a running node's dot.
    pub success: String,
    /// `action.disabledBackground` and `action.disabled`: a disabled button.
    pub disabled_background: String,
    pub disabled_text: String,
    /// `warning` alert colors (`viewerError`).
    pub warning_background: String,
    pub warning_text: String,
}

/// One node of the lab, as the bar shows it once it is selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopologyNode {
    pub name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<String>,
    pub running: bool,
    /// "SSH as admin" rather than "Open shell".
    pub ssh: bool,
    /// The button is off (the node is not running, or SSH is not available from here).
    pub disabled: bool,
    /// Why, as the button's tooltip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Whether a window may show this request: a window key, a title of at most 500 UTF-16 units
/// and a topology file of at most [`MAX_TOPOLOGY_BYTES`].
pub fn is_valid_topology_request(request: &TopologyWindowRequest) -> bool {
    is_window_key(&request.key)
        && utf16_len(&request.title) <= 500
        && request.yaml.len() <= MAX_TOPOLOGY_BYTES
        && request
            .annotations
            .as_ref()
            .is_none_or(|annotations| annotations.len() <= MAX_TOPOLOGY_BYTES)
        && matches!(request.theme.mode.as_str(), "light" | "dark")
}

/// The node a message from the window's page asks to open (`{"type":"open","name":…}`), if it
/// is one of the lab's nodes and its button is on.
pub fn node_to_open(request: &TopologyWindowRequest, message: &str) -> Option<String> {
    let message: serde_json::Value = serde_json::from_str(message).ok()?;
    if message.get("type")?.as_str()? != "open" {
        return None;
    }
    let name = message.get("name")?.as_str()?;
    request
        .nodes
        .iter()
        .find(|node| node.name == name && !node.disabled)
        .map(|node| node.name.clone())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn theme() -> TopologyTheme {
        let color = |name: &str| format!("var(--{name})");
        TopologyTheme {
            mode: "dark".into(),
            background: color("background"),
            foreground: color("foreground"),
            surface: color("surface"),
            border: color("border"),
            accent: color("accent"),
            accent_text: color("accent-text"),
            secondary: color("secondary"),
            disabled: color("disabled"),
            success: color("success"),
            disabled_background: color("disabled-background"),
            disabled_text: color("disabled-text"),
            warning_background: color("warning-background"),
            warning_text: color("warning-text"),
        }
    }

    pub(crate) fn request() -> TopologyWindowRequest {
        TopologyWindowRequest {
            key: "topology-1".into(),
            title: "srl01".into(),
            subtitle: "lab-host · /home/lab/srl01.clab.yml".into(),
            yaml: "name: srl01\ntopology:\n  nodes:\n    srl1:\n      kind: nokia_srlinux\n".into(),
            annotations: None,
            theme: theme(),
            nodes: vec![
                TopologyNode {
                    name: "srl1".into(),
                    kind: "nokia_srlinux".into(),
                    ipv4: Some("172.20.20.2".into()),
                    running: true,
                    ssh: true,
                    disabled: false,
                    reason: None,
                },
                TopologyNode {
                    name: "client".into(),
                    kind: "linux".into(),
                    ipv4: None,
                    running: false,
                    ssh: false,
                    disabled: true,
                    reason: None,
                },
            ],
        }
    }

    #[test]
    fn checks_what_a_window_is_asked_to_show() {
        assert!(is_valid_topology_request(&request()));
        let mut bad = request();
        bad.key = "../etc".into();
        assert!(!is_valid_topology_request(&bad));
        let mut bad = request();
        bad.theme.mode = "sepia".into();
        assert!(!is_valid_topology_request(&bad));
        let mut bad = request();
        bad.yaml = "x".repeat(MAX_TOPOLOGY_BYTES + 1);
        assert!(!is_valid_topology_request(&bad));
    }

    #[test]
    fn opens_only_nodes_of_the_lab_whose_button_is_on() {
        let request = request();
        assert_eq!(
            node_to_open(&request, r#"{"type":"open","name":"srl1"}"#).as_deref(),
            Some("srl1")
        );
        // Stopped, unknown, or not an open message.
        assert_eq!(
            node_to_open(&request, r#"{"type":"open","name":"client"}"#),
            None
        );
        assert_eq!(
            node_to_open(&request, r#"{"type":"open","name":"srl9"}"#),
            None
        );
        assert_eq!(
            node_to_open(&request, r#"{"type":"select","name":"srl1"}"#),
            None
        );
        assert_eq!(node_to_open(&request, "srl1"), None);
    }

    #[test]
    fn crosses_the_process_boundary_as_camel_case_json() {
        let value = serde_json::to_value(request()).unwrap();
        assert_eq!(value["theme"]["accentText"], "var(--accent-text)");
        assert_eq!(value["nodes"][0]["ipv4"], "172.20.20.2");
        assert!(value["nodes"][1].get("ipv4").is_none());
        assert!(value.get("annotations").is_none());
        let back: TopologyWindowRequest = serde_json::from_value(value).unwrap();
        assert_eq!(back, request());
    }
}
