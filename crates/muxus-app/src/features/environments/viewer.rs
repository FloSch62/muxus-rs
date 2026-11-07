//! A running lab's topology in the containerlab viewer (`ClabTopologyDialog.tsx`), in a window
//! of its own: a build with page windows (`muxus-webview`'s `webview` feature) shows the lab
//! the way the TypeScript dialog frames it, with the viewer itself; a build without them draws
//! it natively ([`super::topology`]).
//!
//! The windows are processes of their own (see `muxus_webview::topology`). The app keeps one
//! per lab: opening the lab again brings its window to the front. "SSH as admin" or "Open
//! shell" in a window comes back here as the node to open, for the panel that opened the
//! window; the window then closes, as the dialog does. Windows follow the app's theme and close
//! with the app.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;

use muxus_core::environments::{ContainerlabLab, ContainerlabTopologyResponse};
use muxus_ui::color::css;
use muxus_ui::gpui_kit::{App, Global};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_webview::{
    TopologyNode, TopologyTheme, TopologyWindowRequest, WebWindowResult, WebWindows,
};

use super::model::{EnvironmentHost, is_running};
use super::sessions::node_uses_ssh;
use crate::features::launcher::kit::show_toast;
use crate::hosts::system_openssh::SYSTEM_OPENSSH_UNAVAILABLE;

/// Opens the node a window asks for (by name).
type NodeOpener = Rc<dyn Fn(String, &mut App)>;

/// The app's topology windows.
struct TopologyViewers {
    windows: WebWindows,
    /// Who opens a node a window asks for, by window key.
    owners: HashMap<String, NodeOpener>,
}

impl Global for TopologyViewers {}

/// Whether topologies show in the containerlab viewer (else they are drawn natively).
pub fn available(cx: &App) -> bool {
    cx.has_global::<TopologyViewers>()
}

/// Set up topology windows once at startup, in a build with page windows: windows run by this
/// program (`muxus --web-window`), following the theme and closing with the app.
pub fn install(data_root: PathBuf, cx: &mut App) {
    if !muxus_webview::AVAILABLE {
        return;
    }
    let Ok(windows) = WebWindows::for_current_exe(data_root) else {
        return;
    };
    let nodes = windows.opened_nodes();
    cx.spawn(async move |cx| {
        while let Ok(node) = nodes.recv().await {
            cx.update(|cx| {
                let owner = cx
                    .try_global::<TopologyViewers>()
                    .and_then(|viewers| viewers.owners.get(&node.key).cloned());
                if let Some(owner) = owner {
                    owner(node.name, cx);
                }
            });
        }
    })
    .detach();
    let closed = windows.closed();
    cx.spawn(async move |cx| {
        while let Ok(window) = closed.recv().await {
            cx.update(|cx| {
                if cx.has_global::<TopologyViewers>() {
                    cx.global_mut::<TopologyViewers>()
                        .owners
                        .remove(&window.key);
                }
                if let Some(error) = window.error {
                    show_toast(Status::Error, error, cx);
                }
            });
        }
    })
    .detach();
    cx.observe_global::<Theme>(|cx| {
        if let Some(viewers) = cx.try_global::<TopologyViewers>() {
            viewers.windows.set_theme(&topology_theme(cx.theme()));
        }
    })
    .detach();
    let open = windows.clone();
    cx.on_app_quit(move |_| {
        open.close_all();
        async {}
    })
    .detach();
    cx.set_global(TopologyViewers {
        windows,
        owners: HashMap::new(),
    });
}

/// Show a lab's topology in its window; `open_node` opens a node the window asks for.
/// `None` without topology windows; an error when the window could not be started.
pub fn open(
    request: &TopologyWindowRequest,
    open_node: impl Fn(String, &mut App) + 'static,
    cx: &mut App,
) -> Option<Result<WebWindowResult, String>> {
    let viewers = cx.try_global::<TopologyViewers>()?;
    let result = viewers.windows.open_topology(request);
    if let Ok(WebWindowResult::Opened | WebWindowResult::Focused) = result {
        cx.global_mut::<TopologyViewers>()
            .owners
            .insert(request.key.clone(), Rc::new(open_node));
    }
    Some(result)
}

/// The app's look for a window: the viewer's `appearance` (background, text, paper, divider,
/// primary) and the dialog's own colors.
pub fn topology_theme(theme: &Theme) -> TopologyTheme {
    let p = &theme.palette;
    let dark = theme.mode.is_dark();
    // The standard warning alert (`Alert severity="warning"`), as muxus-ui draws it.
    let (warning_background, warning_text) = if dark {
        (
            muxus_ui::color::darken(p.warning.main, 0.9),
            muxus_ui::color::lighten(p.warning.main, 0.6),
        )
    } else {
        (
            muxus_ui::color::lighten(p.warning.main, 0.9),
            muxus_ui::color::darken(p.warning.main, 0.6),
        )
    };
    TopologyTheme {
        mode: if dark { "dark" } else { "light" }.into(),
        background: css(p.background_default),
        foreground: css(p.text.primary),
        surface: css(p.background_paper),
        border: css(p.divider),
        accent: css(p.primary.main),
        accent_text: css(p.primary.contrast_text),
        secondary: css(p.text.secondary),
        disabled: css(p.text.disabled),
        success: css(p.success.main),
        disabled_background: css(p.action.disabled_background),
        disabled_text: css(p.action.disabled),
        warning_background: css(warning_background),
        warning_text: css(warning_text),
    }
}

/// One window per lab of a host.
fn window_key(host: &EnvironmentHost, lab: &ContainerlabLab) -> String {
    let mut hasher = DefaultHasher::new();
    (&host.key, &lab.name, lab.runtime).hash(&mut hasher);
    format!("topology-{:016x}", hasher.finish())
}

/// What the window shows for a lab: its name, `<host> · <topology file>`, the file and its
/// annotations, and each node as the dialog's bar shows it (running, "SSH as admin" or
/// "Open shell", off when not running or when SSH is not available from this host).
pub fn topology_request(
    host: &EnvironmentHost,
    lab: &ContainerlabLab,
    topology: &ContainerlabTopologyResponse,
    theme: &Theme,
) -> TopologyWindowRequest {
    let system = host.uses_system_open_ssh();
    TopologyWindowRequest {
        key: window_key(host, lab),
        title: lab.name.clone(),
        subtitle: format!("{} · {}", host.label, topology.path),
        yaml: topology.yaml.clone(),
        annotations: topology.annotations.clone(),
        theme: topology_theme(theme),
        nodes: lab
            .nodes
            .iter()
            .map(|node| {
                let running = is_running(&node.state);
                let ssh = node_uses_ssh(node);
                TopologyNode {
                    name: node.name.clone(),
                    kind: node.kind.clone(),
                    ipv4: node.ipv4.clone(),
                    running,
                    ssh,
                    disabled: !running || (ssh && system),
                    reason: (ssh && system).then(|| SYSTEM_OPENSSH_UNAVAILABLE.to_owned()),
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use muxus_core::environments::{ContainerRuntime, ContainerlabNode};
    use muxus_ui::theme::Mode;

    use super::*;

    fn node(name: &str, kind: &str, state: &str, ipv4: Option<&str>) -> ContainerlabNode {
        ContainerlabNode {
            name: name.into(),
            container: format!("clab-srl01-{name}"),
            kind: kind.into(),
            image: String::new(),
            state: state.into(),
            ipv4: ipv4.map(Into::into),
            ipv6: None,
        }
    }

    fn lab() -> ContainerlabLab {
        ContainerlabLab {
            name: "srl01".into(),
            runtime: ContainerRuntime::Docker,
            topology_file: Some("/home/lab/srl01.clab.yml".into()),
            owner: None,
            nodes: vec![
                node("srl1", "nokia_srlinux", "running", Some("172.20.20.2")),
                node("client", "linux", "running", Some("172.20.20.3")),
                node("old", "nokia_srlinux", "exited", Some("172.20.20.4")),
            ],
        }
    }

    fn host(system_open_ssh: bool) -> EnvironmentHost {
        EnvironmentHost {
            key: "host_1".into(),
            label: "lab-host".into(),
            system_open_ssh: Some(system_open_ssh),
            ..EnvironmentHost::default()
        }
    }

    fn topology() -> ContainerlabTopologyResponse {
        ContainerlabTopologyResponse {
            path: "/home/lab/srl01.clab.yml".into(),
            yaml: "name: srl01\n".into(),
            annotations: Some("{}".into()),
        }
    }

    #[test]
    fn describes_the_lab_and_its_nodes_as_the_dialog_shows_them() {
        let theme = Theme::new(Mode::Light);
        let request = topology_request(&host(false), &lab(), &topology(), &theme);
        assert_eq!(request.title, "srl01");
        assert_eq!(request.subtitle, "lab-host · /home/lab/srl01.clab.yml");
        assert_eq!(request.annotations.as_deref(), Some("{}"));
        assert_eq!(request.theme.mode, "light");
        assert!(request.key.starts_with("topology-"));
        assert!(muxus_webview::topology::is_valid_topology_request(&request));
        let shown: Vec<(&str, bool, bool, bool)> = request
            .nodes
            .iter()
            .map(|node| (node.name.as_str(), node.running, node.ssh, node.disabled))
            .collect();
        assert_eq!(
            shown,
            [
                ("srl1", true, true, false),
                // A linux node opens a shell.
                ("client", true, false, false),
                ("old", false, true, true),
            ]
        );
        assert!(request.nodes.iter().all(|node| node.reason.is_none()));
    }

    #[test]
    fn ssh_is_off_through_a_system_openssh_host() {
        let request = topology_request(&host(true), &lab(), &topology(), &Theme::new(Mode::Dark));
        assert_eq!(request.theme.mode, "dark");
        let srl1 = &request.nodes[0];
        assert!(srl1.disabled);
        assert_eq!(srl1.reason.as_deref(), Some(SYSTEM_OPENSSH_UNAVAILABLE));
        // A shell does not go through SSH.
        assert!(!request.nodes[1].disabled);
    }

    #[test]
    fn keeps_one_window_per_lab_of_a_host() {
        let (a, b) = (host(false), lab());
        assert_eq!(window_key(&a, &b), window_key(&a, &b));
        let mut other = lab();
        other.name = "srl02".into();
        assert_ne!(window_key(&a, &b), window_key(&a, &other));
        let mut elsewhere = host(false);
        elsewhere.key = "host_2".into();
        assert_ne!(window_key(&a, &b), window_key(&elsewhere, &b));
    }
}
