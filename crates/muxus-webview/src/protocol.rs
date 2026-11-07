//! How the app talks to a page window. Each window is a process of its own (the `muxus` binary
//! started with [`WEB_WINDOW_ARG`]), so the browser engine's main loop never meets GPUI's.
//! Messages are JSON, one per line: the app writes a [`WebWindowLaunch`] and then
//! [`WebWindowCommand`]s to the window's stdin; the window writes [`WebWindowEvent`]s to its
//! stdout. The window closes when its stdin ends, so it never outlives the app.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::topology::{TopologyTheme, TopologyWindowRequest};

/// The hidden command line switch that turns the `muxus` binary into a page window.
pub const WEB_WINDOW_ARG: &str = "--web-window";

/// The first line a window reads: the request and where the page keeps its data, or the lab
/// a topology window shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebWindowLaunch {
    /// A [`crate::WebWindowRequest`], checked again by the window.
    #[serde(default)]
    pub request: Value,
    /// The host's own cookie jar and storage.
    #[serde(default)]
    pub data_directory: PathBuf,
    /// A topology window instead of a page window (see [`crate::topology`]); it keeps no data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<TopologyWindowRequest>,
}

/// What the app asks of an open window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum WebWindowCommand {
    /// Bring the window (restored) to the front.
    Focus,
    /// Close the window, as the last app window closes.
    Close,
    /// The app's theme changed: a topology window follows it (page windows ignore this).
    Theme { theme: Box<TopologyTheme> },
}

/// What a window tells the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum WebWindowEvent {
    /// The window is up and loading the page.
    Opened,
    /// The window cannot be shown; it exits after this.
    Error { message: String },
    /// A topology window's "SSH as admin" / "Open shell": open this node. The window closes
    /// after it.
    #[serde(rename = "openNode")]
    OpenNode { name: String },
}

/// One message as the line that carries it.
pub fn line<T: Serialize>(message: &T) -> String {
    let mut text = serde_json::to_string(message).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn writes_one_json_message_per_line() {
        assert_eq!(line(&WebWindowCommand::Focus), "{\"op\":\"focus\"}\n");
        assert_eq!(
            serde_json::from_str::<WebWindowCommand>("{\"op\":\"close\"}").unwrap(),
            WebWindowCommand::Close
        );
        assert_eq!(line(&WebWindowEvent::Opened), "{\"event\":\"opened\"}\n");
        assert_eq!(
            serde_json::from_value::<WebWindowEvent>(
                json!({ "event": "error", "message": "no display" })
            )
            .unwrap(),
            WebWindowEvent::Error {
                message: "no display".into()
            }
        );
        let launch = WebWindowLaunch {
            request: json!({ "key": "k" }),
            data_directory: PathBuf::from("/data/web-pages/k"),
            topology: None,
        };
        assert_eq!(
            serde_json::to_value(&launch).unwrap(),
            json!({ "request": { "key": "k" }, "dataDirectory": "/data/web-pages/k" })
        );
    }

    #[test]
    fn carries_a_topology_window_and_its_messages() {
        let request = crate::topology::tests::request();
        let launch: WebWindowLaunch =
            serde_json::from_value(json!({ "topology": request.clone() })).unwrap();
        assert_eq!(launch.topology, Some(request));
        assert_eq!(launch.request, Value::Null);
        assert_eq!(
            serde_json::to_value(WebWindowEvent::OpenNode {
                name: "srl1".into()
            })
            .unwrap(),
            json!({ "event": "openNode", "name": "srl1" })
        );
        let theme = crate::topology::tests::theme();
        let command = WebWindowCommand::Theme {
            theme: Box::new(theme.clone()),
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["op"], "theme");
        assert_eq!(value["theme"]["mode"], "dark");
        assert_eq!(
            serde_json::from_str::<WebWindowCommand>(&line(&command)).unwrap(),
            WebWindowCommand::Theme {
                theme: Box::new(theme)
            }
        );
    }
}
