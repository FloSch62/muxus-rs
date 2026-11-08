//! One section per tool a host has (`components/environments/EnvironmentSections.tsx`):
//! Docker and Podman containers grouped by Compose project, Containerlab labs,
//! Proxmox guests and libvirt domains, each line with its actions.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use muxus_core::environments::{
    ComposeAction, ContainerAction, ContainerInfo, ContainerRuntimeEnvironment, ContainerShell,
    ContainerlabEnvironment, ContainerlabLab, EnvironmentAction, EnvironmentTarget, LibvirtAction,
    LibvirtDomain, LibvirtEnvironment, LibvirtUri, ProxmoxAction, ProxmoxEnvironment, ProxmoxGuest,
    ProxmoxGuestType, environment_command_line,
};
use muxus_ui::gpui_kit::{
    AnyElement, Context, ElementId, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{Theme, fonts};
use muxus_ui::widgets::Tooltip;
use regex::Regex;

use super::hosts::JumpHostOptions;
use super::model::{
    EnvironmentHost, EnvironmentKind, HostEnvironmentsEntry, is_running, plain_containers,
};
use super::panel::{EnvironmentsPanel, GuestRef, MenuEntry, PanelAction};
use super::sessions::{NODE_USER, containerlab_node_entry, node_uses_ssh, port_scheme};
use super::tree::{Health, LEVEL, LineSpec, note_line, small_text, status_dot, tree_line};

/// How Muxus says a feature needs the built-in SSH client (`unavailableWithSystemOpenSsh`).
pub(super) fn unavailable_with_system_open_ssh(what: &str) -> String {
    format!("{what} is not available with system OpenSSH")
}

/// A saved host only exists as a jump hop inside Muxus, and only the ssh program can log in to
/// a host set to use it, so such a host has no way to be jumped through (`jumpUnavailable`).
fn jump_unavailable(host: &EnvironmentHost) -> bool {
    host.uses_system_open_ssh()
        && host
            .profile
            .as_ref()
            .is_some_and(|profile| profile.profile_id.is_some())
}

pub(super) fn matches(filter: &str, values: &[Option<&str>]) -> bool {
    filter.is_empty()
        || values
            .iter()
            .flatten()
            .any(|value| value.to_lowercase().contains(filter))
}

/// `bytes`: a size with one decimal under 10 of its unit.
fn bytes(value: Option<f64>) -> String {
    let Some(value) = value.filter(|value| *value != 0.) else {
        return "0 B".into();
    };
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = value;
    let mut unit = 0;
    while size >= 1024. && unit < UNITS.len() - 1 {
        size /= 1024.;
        unit += 1;
    }
    if size >= 10. || unit == 0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn hidden_note(depth: usize, hidden: usize, noun: &str, theme: &Theme) -> Option<AnyElement> {
    (hidden > 0).then(|| {
        note_line(
            depth,
            format!(
                "{hidden} stopped {noun}{} hidden",
                if hidden == 1 { "" } else { "s" }
            ),
            theme.palette.text.secondary,
        )
    })
}

static EXITED_WITH_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^exited \(([^)]*)\)").expect("exit pattern"));

fn container_health(container: &ContainerInfo) -> Health {
    if is_running(&container.state) {
        return Health::Up;
    }
    if matches!(
        container.state.as_str(),
        "restarting" | "paused" | "created"
    ) {
        return Health::Warn;
    }
    let failed = EXITED_WITH_ERROR
        .captures(&container.status)
        .is_some_and(|found| &found[1] != "0");
    if container.state == "dead" || failed {
        return Health::Down;
    }
    Health::Off
}

/// A service a host serves, opened in the browser through a forward when needed (`PortChip`).
fn port_chip(
    id: String,
    text: String,
    title: String,
    disabled: bool,
    on_click: impl Fn(&muxus_ui::gpui_kit::ClickEvent, &mut Window, &mut muxus_ui::gpui_kit::App)
    + 'static,
    theme: &Theme,
) -> AnyElement {
    let p = theme.palette;
    let mut chip = div()
        .id(ElementId::Name(id.into()))
        .flex()
        .items_center()
        .gap(px(4.))
        .h(px(20.))
        .px(px(6.))
        .rounded(px(10.))
        .border_1()
        .border_color(p.divider)
        .child(
            label(text)
                .size(11.)
                .family(fonts::MONO)
                .color(p.text.secondary),
        )
        .child(icon("OpenInNewOutlined", 11., p.text.secondary))
        .tooltip(Tooltip::text(title));
    if disabled {
        chip = chip.opacity(0.5);
    } else {
        chip = chip
            .cursor_pointer()
            .hover(|style| style.border_color(p.primary.main))
            .on_click(on_click);
    }
    chip.into_any_element()
}

impl EnvironmentsPanel {
    // -----------------------------------------------------------------------
    // Docker and Podman
    // -----------------------------------------------------------------------

    pub(super) fn containers_section(
        &mut self,
        entry: &HostEnvironmentsEntry,
        environment: &ContainerRuntimeEnvironment,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let host = &entry.host;
        let kind = EnvironmentKind::runtime(environment.runtime);
        let all = plain_containers(&environment.containers);
        let running = all.iter().filter(|c| is_running(&c.state)).count();
        let filter = self.filter.clone();
        let matching: Vec<&ContainerInfo> = all
            .iter()
            .copied()
            .filter(|c| {
                matches(
                    &filter,
                    &[Some(&c.name), Some(&c.image), c.compose_project.as_deref()],
                )
            })
            .collect();
        let visible: Vec<&ContainerInfo> = matching
            .iter()
            .copied()
            .filter(|c| self.show_stopped || is_running(&c.state))
            .collect();
        if !filter.is_empty() && visible.is_empty() {
            return Vec::new();
        }
        let key = format!("{}/{}", host.key, environment.runtime.as_str());
        let open = self.is_open(&key, true);
        let spec = LineSpec::new(key.clone(), 1, kind.label())
            .icon(self.logo(kind, window, cx))
            .maybe_detail(environment.version.as_ref().map(|v| format!("v{v}")))
            .status_text(format!("{running} / {} running", all.len()), theme)
            .open(open);
        let click = self.line_click(Some((key, true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if !open {
            return lines;
        }
        let mut projects: BTreeMap<String, Vec<&ContainerInfo>> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        let mut standalone = Vec::new();
        for container in &visible {
            match &container.compose_project {
                Some(project) => {
                    if !projects.contains_key(project) {
                        order.push(project.clone());
                    }
                    projects.entry(project.clone()).or_default().push(container);
                }
                None => standalone.push(*container),
            }
        }
        for project in order {
            let containers = projects.remove(&project).unwrap_or_default();
            lines.extend(self.compose_group(host, environment, &project, &containers, theme, cx));
        }
        for container in standalone {
            lines.extend(self.container_line(host, environment, container, 2, theme, cx));
        }
        if all.is_empty() {
            lines.push(note_line(2, "No containers.", theme.palette.text.secondary));
        }
        lines.extend(hidden_note(
            2,
            matching.len() - visible.len(),
            "container",
            theme,
        ));
        lines
    }

    fn compose_group(
        &mut self,
        host: &EnvironmentHost,
        environment: &ContainerRuntimeEnvironment,
        project: &str,
        containers: &[&ContainerInfo],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let key = format!(
            "{}/{}/compose/{project}",
            host.key,
            environment.runtime.as_str()
        );
        let open = self.is_open(&key, true);
        let running = containers.iter().filter(|c| is_running(&c.state)).count();
        let compose = |action: ComposeAction| PanelAction::Act {
            host: host.clone(),
            action: EnvironmentAction::Compose {
                runtime: environment.runtime,
                project: project.to_string(),
                action,
            },
            confirm: (action == ComposeAction::Stop).then(|| {
                (
                    format!("Stop {project}?"),
                    "Every container of the Compose project stops.".to_string(),
                )
            }),
        };
        let mut spec = LineSpec::new(key.clone(), 2, project.to_string())
            .icon(icon("LayersOutlined", 16., theme.palette.text.secondary))
            .detail("Compose")
            .status_text(format!("{running} running"), theme)
            .open(open);
        if let Some(menu) = self.line_menu(
            format!("env-compose-menu-{key}"),
            format!("More actions for {project}"),
            vec![
                MenuEntry::new("Start project", compose(ComposeAction::Start)),
                MenuEntry::new("Restart project", compose(ComposeAction::Restart)),
                MenuEntry::new("Stop project…", compose(ComposeAction::Stop)).destructive(),
            ],
            cx,
        ) {
            spec = spec.action(menu);
        }
        let click = self.line_click(Some((key, true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if open {
            for container in containers {
                lines.extend(self.container_line(host, environment, container, 3, theme, cx));
            }
        }
        lines
    }

    fn container_line(
        &mut self,
        host: &EnvironmentHost,
        environment: &ContainerRuntimeEnvironment,
        container: &ContainerInfo,
        depth: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let runtime = environment.runtime;
        let up = is_running(&container.state);
        let name = container.name.clone();
        let shell = |shell: Option<ContainerShell>| {
            PanelAction::Target(
                host.clone(),
                EnvironmentTarget::Container {
                    runtime,
                    container: name.clone(),
                    shell,
                },
            )
        };
        let logs = PanelAction::Target(
            host.clone(),
            EnvironmentTarget::ContainerLogs {
                runtime,
                container: name.clone(),
            },
        );
        let lifecycle = |action: ContainerAction| PanelAction::Act {
            host: host.clone(),
            action: EnvironmentAction::Container {
                runtime,
                container: name.clone(),
                action,
            },
            confirm: match action {
                ContainerAction::Stop => Some((
                    format!("Stop {name}?"),
                    "Its processes are asked to exit.".to_string(),
                )),
                ContainerAction::Remove => Some((
                    format!("Remove {name}?"),
                    format!(
                        "{}, with what it wrote outside its volumes. Its image and named volumes are kept.",
                        if up {
                            "The container is stopped and deleted"
                        } else {
                            "The container is deleted"
                        }
                    ),
                )),
                _ => None,
            },
        };
        let mut menu: Vec<MenuEntry> = Vec::new();
        if up {
            menu.push(MenuEntry::new(
                "Shell (bash)",
                shell(Some(ContainerShell::Bash)),
            ));
            menu.push(MenuEntry::new("Shell (sh)", shell(Some(ContainerShell::Sh))).divider());
            menu.push(MenuEntry::new(
                "Restart",
                lifecycle(ContainerAction::Restart),
            ));
            menu.push(if container.state == "paused" {
                MenuEntry::new("Resume", lifecycle(ContainerAction::Unpause))
            } else {
                MenuEntry::new("Pause", lifecycle(ContainerAction::Pause))
            });
            menu.push(
                MenuEntry::new("Stop…", lifecycle(ContainerAction::Stop))
                    .destructive()
                    .divider(),
            );
        } else {
            menu.push(MenuEntry::new("Start", lifecycle(ContainerAction::Start)).divider());
        }
        let target = EnvironmentTarget::Container {
            runtime,
            container: name.clone(),
            shell: None,
        };
        menu.push(MenuEntry::new(
            "Add to hosts",
            PanelAction::AddEnter {
                host: host.clone(),
                name: name.clone(),
                target: target.clone(),
            },
        ));
        menu.push(
            MenuEntry::new(
                "Copy exec command",
                PanelAction::Copy(environment_command_line(&target).unwrap_or_default()),
            )
            .divider(),
        );
        menu.push(MenuEntry::new("Remove…", lifecycle(ContainerAction::Remove)).destructive());

        let ports_hint = container
            .ports
            .iter()
            .map(|port| match port.host_port {
                Some(host_port) => format!(
                    "{}{host_port}→{}/{}",
                    port.host_ip
                        .as_ref()
                        .map(|ip| format!("{ip}:"))
                        .unwrap_or_default(),
                    port.container_port,
                    port.protocol
                ),
                None => format!("{}/{}", port.container_port, port.protocol),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let id = format!("{}/{}/{}", host.key, runtime.as_str(), container.id);
        let mut spec = LineSpec::new(id.clone(), depth, name.clone())
            .icon(status_dot(container_health(container), theme))
            .detail(container.image.clone())
            .status_text(container.status.clone(), theme)
            .hint("Image", container.image.clone())
            .hint("Status", container.status.clone())
            .hint("Compose", container.compose_project.clone())
            .hint(
                "ID",
                muxus_core::js::utf16_prefix(&container.id, 12).to_string(),
            )
            .hint("Ports", ports_hint);
        spec.detail_mono = true;
        spec.dim = !up;
        if up {
            spec = spec.action(self.line_action(
                format!("env-shell-{id}"),
                "Open shell",
                "TerminalOutlined",
                shell(None),
                false,
                cx,
            ));
        } else {
            spec = spec.action(self.line_action(
                format!("env-start-{id}"),
                "Start",
                "PlayArrowOutlined",
                lifecycle(ContainerAction::Start),
                false,
                cx,
            ));
        }
        spec = spec.action(self.line_action(
            format!("env-logs-{id}"),
            "Follow logs",
            "SubjectOutlined",
            logs.clone(),
            false,
            cx,
        ));
        if let Some(menu) = self.line_menu(
            format!("env-menu-{id}"),
            format!("More actions for {name}"),
            menu,
            cx,
        ) {
            spec = spec.action(menu);
        }
        let open = if up { shell(None) } else { logs };
        let click = self.line_click(None, Some(open), cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        let published: Vec<_> = container
            .ports
            .iter()
            .filter(|port| port.host_port.is_some() && port.protocol == "tcp")
            .collect();
        if up && !published.is_empty() {
            let system = host.uses_system_open_ssh();
            let mut row = div()
                .flex()
                .flex_wrap()
                .gap(px(4.))
                .pl(px(8. + depth as f32 * LEVEL + 16. + 6. + 14.))
                .pr(px(8.))
                .pb(px(6.));
            for port in published {
                let host_port = port.host_port.unwrap_or_default();
                let scheme = port_scheme(host_port);
                let title = if system {
                    unavailable_with_system_open_ssh("Open this port")
                } else {
                    format!(
                        "Open {scheme}://…:{host_port}{}",
                        if host.profile.is_some() {
                            format!(", forwarded from {}", host.label)
                        } else {
                            String::new()
                        }
                    )
                };
                let action = PanelAction::Port {
                    host: host.clone(),
                    address: port.host_ip.clone(),
                    port: host_port,
                    scheme,
                };
                row = row.child(port_chip(
                    format!("env-port-{id}-{host_port}-{}", port.container_port),
                    format!("{host_port} → {}", port.container_port),
                    title,
                    system,
                    self.on(action, cx),
                    theme,
                ));
            }
            lines.push(row.into_any_element());
        }
        lines
    }

    // -----------------------------------------------------------------------
    // Containerlab
    // -----------------------------------------------------------------------

    pub(super) fn containerlab_section(
        &mut self,
        entry: &HostEnvironmentsEntry,
        environment: &ContainerlabEnvironment,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let host = &entry.host;
        let filter = self.filter.clone();
        let labs: Vec<&ContainerlabLab> = environment
            .labs
            .iter()
            .filter(|lab| {
                matches(&filter, &[Some(&lab.name)])
                    || lab.nodes.iter().any(|node| {
                        matches(
                            &filter,
                            &[Some(&node.name), Some(&node.kind), node.ipv4.as_deref()],
                        )
                    })
            })
            .collect();
        if !filter.is_empty() && labs.is_empty() {
            return Vec::new();
        }
        let key = format!("{}/containerlab", host.key);
        let open = self.is_open(&key, true);
        let count = environment.labs.len();
        let spec = LineSpec::new(key.clone(), 1, EnvironmentKind::Containerlab.label())
            .icon(self.logo(EnvironmentKind::Containerlab, window, cx))
            .status_text(
                format!("{count} lab{}", if count == 1 { "" } else { "s" }),
                theme,
            )
            .open(open);
        let click = self.line_click(Some((key, true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if open {
            for lab in labs {
                lines.extend(self.lab_lines(host, lab, theme, cx));
            }
        }
        lines
    }

    fn lab_lines(
        &mut self,
        host: &EnvironmentHost,
        lab: &ContainerlabLab,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let key = format!("{}/containerlab/{}", host.key, lab.name);
        let open = self.is_open(&key, true);
        let running = lab
            .nodes
            .iter()
            .filter(|node| is_running(&node.state))
            .count();
        let filter = self.filter.clone();
        let lab_matches = matches(&filter, &[Some(&lab.name)]);
        let matching: Vec<_> = lab
            .nodes
            .iter()
            .filter(|node| {
                lab_matches
                    || matches(
                        &filter,
                        &[Some(&node.name), Some(&node.kind), node.ipv4.as_deref()],
                    )
            })
            .collect();
        let nodes: Vec<_> = matching
            .iter()
            .copied()
            .filter(|node| self.show_stopped || is_running(&node.state))
            .collect();
        let file = lab
            .topology_file
            .as_ref()
            .map(|file| file.rsplit('/').next().unwrap_or(file).to_string());
        let health = if running == lab.nodes.len() {
            Health::Up
        } else if running > 0 {
            Health::Warn
        } else {
            Health::Off
        };
        let mut spec = LineSpec::new(key.clone(), 2, lab.name.clone())
            .icon(status_dot(health, theme))
            .maybe_detail(file)
            .status_text(format!("{running} / {} nodes", lab.nodes.len()), theme)
            .hint("Topology", lab.topology_file.clone())
            .hint("Owner", lab.owner.clone())
            .hint("Nodes", format!("{running} of {} running", lab.nodes.len()))
            .open(open)
            .action(self.line_action(
                format!("env-topology-{key}"),
                "Show topology",
                "HubOutlined",
                PanelAction::Topology {
                    host: host.clone(),
                    lab: lab.clone(),
                },
                lab.topology_file.is_none(),
                cx,
            ));
        spec.emphasis = true;
        let click = self.line_click(Some((key.clone(), true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if !open {
            return lines;
        }
        let system = host.uses_system_open_ssh();
        for node in &nodes {
            let up = is_running(&node.state);
            let ssh = node_uses_ssh(node);
            let open_node = PanelAction::Open(containerlab_node_entry(host, lab, node));
            let container_target = EnvironmentTarget::Container {
                runtime: lab.runtime,
                container: node.container.clone(),
                shell: None,
            };
            let name = format!("{}-{}", lab.name, node.name);
            let mut menu = vec![
                MenuEntry::new(
                    "Shell in the container",
                    PanelAction::Target(host.clone(), container_target.clone()),
                )
                .disabled(!up),
            ];
            if let Some(ipv4) = &node.ipv4 {
                menu.push(
                    MenuEntry::new(
                        "SSH as…",
                        PanelAction::SshAs {
                            host: host.clone(),
                            address: ipv4.clone(),
                            name: node.name.clone(),
                        },
                    )
                    .disabled(!up),
                );
            }
            menu.push(
                MenuEntry::new(
                    "Follow container logs",
                    PanelAction::Target(
                        host.clone(),
                        EnvironmentTarget::ContainerLogs {
                            runtime: lab.runtime,
                            container: node.container.clone(),
                        },
                    ),
                )
                .divider(),
            );
            if let Some(ipv4) = &node.ipv4 {
                menu.push(MenuEntry::new(
                    "Copy management address",
                    PanelAction::Copy(ipv4.clone()),
                ));
            }
            let add = match (&node.ipv4, ssh) {
                (Some(ipv4), true) => PanelAction::AddAddress {
                    host: host.clone(),
                    options: JumpHostOptions {
                        name: name.clone(),
                        address: ipv4.clone(),
                        user: Some(NODE_USER.into()),
                        group: Some(lab.name.clone()),
                    },
                },
                _ => PanelAction::AddEnter {
                    host: host.clone(),
                    name: name.clone(),
                    target: container_target,
                },
            };
            menu.push(MenuEntry::new("Add to hosts", add).disabled(ssh && jump_unavailable(host)));
            let id = format!("{key}/{}", node.container);
            let main_label = if ssh {
                let label = format!("SSH to {} as admin", node.ipv4.clone().unwrap_or_default());
                if system {
                    unavailable_with_system_open_ssh(&label)
                } else {
                    label
                }
            } else {
                "Open shell".to_string()
            };
            let status: AnyElement = match &node.ipv4 {
                Some(ipv4) => label(ipv4.clone())
                    .size(11.)
                    .family(fonts::MONO)
                    .color(theme.palette.text.secondary)
                    .into_any_element(),
                None => small_text(node.state.clone(), theme).into_any_element(),
            };
            let mut spec = LineSpec::new(id.clone(), 3, node.name.clone())
                .icon(status_dot(if up { Health::Up } else { Health::Off }, theme))
                .detail(node.kind.clone())
                .status(status)
                .hint("Kind", node.kind.clone())
                .hint("Image", node.image.clone())
                .hint("State", node.state.clone())
                .hint("IPv4", node.ipv4.clone())
                .hint("IPv6", node.ipv6.clone())
                .hint("Container", node.container.clone())
                .action(self.line_action(
                    format!("env-node-{id}"),
                    main_label,
                    if ssh {
                        "LoginOutlined"
                    } else {
                        "TerminalOutlined"
                    },
                    open_node.clone(),
                    !up || (ssh && system),
                    cx,
                ));
            spec.dim = !up;
            if let Some(menu) = self.line_menu(
                format!("env-node-menu-{id}"),
                format!("More actions for {}", node.name),
                menu,
                cx,
            ) {
                spec = spec.action(menu);
            }
            let click = self.line_click(None, (up && !(ssh && system)).then_some(open_node), cx);
            lines.push(tree_line(spec, theme, click));
        }
        lines.extend(hidden_note(3, matching.len() - nodes.len(), "node", theme));
        lines
    }

    // -----------------------------------------------------------------------
    // Proxmox VE
    // -----------------------------------------------------------------------

    pub(super) fn proxmox_section(
        &mut self,
        entry: &HostEnvironmentsEntry,
        environment: &ProxmoxEnvironment,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let host = &entry.host;
        let filter = self.filter.clone();
        let guests: Vec<&ProxmoxGuest> = environment
            .guests
            .iter()
            .filter(|guest| matches(&filter, &[Some(&guest.name), Some(&guest.vmid.to_string())]))
            .collect();
        if !filter.is_empty() && guests.is_empty() {
            return Vec::new();
        }
        let visible: Vec<&ProxmoxGuest> = guests
            .iter()
            .copied()
            .filter(|guest| {
                self.show_stopped || (guest.status == "running" && guest.template != Some(true))
            })
            .collect();
        let running = environment
            .guests
            .iter()
            .filter(|guest| guest.status == "running")
            .count();
        let system = host.uses_system_open_ssh();
        let key = format!("{}/proxmox", host.key);
        let open = self.is_open(&key, true);
        let spec = LineSpec::new(key.clone(), 1, EnvironmentKind::Proxmox.label())
            .icon(self.logo(EnvironmentKind::Proxmox, window, cx))
            .maybe_detail(environment.version.clone())
            .status_text(
                format!("{running} / {} running", environment.guests.len()),
                theme,
            )
            .open(open)
            .action(self.line_action(
                format!("env-pve-web-{}", host.key),
                if system {
                    unavailable_with_system_open_ssh("Open the web interface")
                } else {
                    "Open the web interface (https :8006)".into()
                },
                "PublicOutlined",
                PanelAction::Port {
                    host: host.clone(),
                    address: Some("127.0.0.1".into()),
                    port: 8006,
                    scheme: "https",
                },
                system,
                cx,
            ));
        let click = self.line_click(Some((key.clone(), true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if !open {
            return lines;
        }
        for guest in &visible {
            let up = guest.status == "running";
            let lxc = guest.guest_type == ProxmoxGuestType::Lxc;
            let console_target = if lxc {
                EnvironmentTarget::ProxmoxLxc { vmid: guest.vmid }
            } else {
                EnvironmentTarget::ProxmoxSerial { vmid: guest.vmid }
            };
            let console = PanelAction::Target(host.clone(), console_target.clone());
            let power = |action: ProxmoxAction| PanelAction::Act {
                host: host.clone(),
                action: EnvironmentAction::Proxmox {
                    guest_type: guest.guest_type,
                    vmid: guest.vmid,
                    action,
                },
                confirm: match action {
                    ProxmoxAction::Stop => Some((
                        format!("Stop {}?", guest.name),
                        "The guest is switched off without shutting down.".to_string(),
                    )),
                    ProxmoxAction::Shutdown => Some((
                        format!("Shut down {}?", guest.name),
                        "The guest is asked to shut down.".to_string(),
                    )),
                    _ => None,
                },
            };
            let load = if up && guest.maxcpu.is_some_and(|max| max != 0.) {
                format!(
                    "CPU {}% · {} / {}",
                    (guest.cpu.unwrap_or(0.) * 100.).round(),
                    bytes(guest.mem),
                    bytes(guest.maxmem)
                )
            } else if guest.template == Some(true) {
                "template".into()
            } else {
                guest.status.clone()
            };
            let kind = if lxc { "CT" } else { "VM" };
            let id = format!("{key}/{}", guest.vmid);
            let guest_ref = GuestRef::Proxmox(guest.guest_type, guest.vmid);
            let mut spec = LineSpec::new(id.clone(), 2, guest.name.clone())
                .icon(status_dot(
                    if up {
                        Health::Up
                    } else if guest.status == "paused" {
                        Health::Warn
                    } else {
                        Health::Off
                    },
                    theme,
                ))
                .detail(format!(
                    "{kind} {}{}",
                    guest.vmid,
                    guest
                        .node
                        .as_ref()
                        .map(|node| format!(" · {node}"))
                        .unwrap_or_default()
                ))
                .status_text(load.clone(), theme)
                .hint(
                    "Type",
                    if lxc {
                        "Container (LXC)".to_string()
                    } else {
                        "Virtual machine".to_string()
                    },
                )
                .hint("ID", guest.vmid.to_string())
                .hint("Node", guest.node.clone())
                .hint(
                    "Status",
                    if guest.template == Some(true) {
                        "template".to_string()
                    } else {
                        guest.status.clone()
                    },
                )
                .hint(
                    "Load",
                    (up && guest.maxcpu.is_some_and(|max| max != 0.)).then(|| load.clone()),
                );
            spec.dim = !up;
            if up {
                spec = spec
                    .action(self.line_action(
                        format!("env-console-{id}"),
                        if lxc {
                            "Enter the container (pct enter)"
                        } else {
                            "Serial console (qm terminal)"
                        },
                        "TerminalOutlined",
                        console.clone(),
                        false,
                        cx,
                    ))
                    .action(self.line_action(
                        format!("env-ssh-{id}"),
                        if system {
                            unavailable_with_system_open_ssh("SSH to the guest")
                        } else {
                            "SSH to the guest".into()
                        },
                        "LoginOutlined",
                        PanelAction::SshGuest {
                            host: host.clone(),
                            guest: guest_ref.clone(),
                            name: guest.name.clone(),
                        },
                        system,
                        cx,
                    ));
            } else if guest.template != Some(true) {
                spec = spec.action(self.line_action(
                    format!("env-start-{id}"),
                    "Start",
                    "PlayArrowOutlined",
                    power(ProxmoxAction::Start),
                    false,
                    cx,
                ));
            }
            let mut menu = Vec::new();
            if up {
                menu.push(MenuEntry::new("Reboot", power(ProxmoxAction::Reboot)));
                menu.push(MenuEntry::new("Shut down…", power(ProxmoxAction::Shutdown)));
                menu.push(
                    MenuEntry::new("Stop (force)…", power(ProxmoxAction::Stop))
                        .destructive()
                        .divider(),
                );
            } else if guest.template != Some(true) {
                menu.push(MenuEntry::new("Start", power(ProxmoxAction::Start)).divider());
            }
            if guest.template != Some(true) {
                menu.push(
                    MenuEntry::new(
                        "Add to hosts",
                        PanelAction::AddGuest {
                            host: host.clone(),
                            name: guest.name.clone(),
                            guest: guest_ref,
                            console: console_target,
                        },
                    )
                    .disabled(jump_unavailable(host)),
                );
            }
            if let Some(menu) = self.line_menu(
                format!("env-guest-menu-{id}"),
                format!("More actions for {}", guest.name),
                menu,
                cx,
            ) {
                spec = spec.action(menu);
            }
            let click = self.line_click(None, up.then_some(console), cx);
            lines.push(tree_line(spec, theme, click));
        }
        lines.extend(hidden_note(2, guests.len() - visible.len(), "guest", theme));
        lines
    }

    // -----------------------------------------------------------------------
    // libvirt
    // -----------------------------------------------------------------------

    pub(super) fn libvirt_section(
        &mut self,
        entry: &HostEnvironmentsEntry,
        environment: &LibvirtEnvironment,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let host = &entry.host;
        let filter = self.filter.clone();
        let domains: Vec<&LibvirtDomain> = environment
            .domains
            .iter()
            .filter(|domain| matches(&filter, &[Some(&domain.name)]))
            .collect();
        if !filter.is_empty() && domains.is_empty() {
            return Vec::new();
        }
        let visible: Vec<&LibvirtDomain> = domains
            .iter()
            .copied()
            .filter(|domain| self.show_stopped || domain.state == "running")
            .collect();
        let running = environment
            .domains
            .iter()
            .filter(|domain| domain.state == "running")
            .count();
        let system = host.uses_system_open_ssh();
        let key = format!("{}/libvirt", host.key);
        let open = self.is_open(&key, true);
        let spec = LineSpec::new(key.clone(), 1, EnvironmentKind::Libvirt.label())
            .icon(self.logo(EnvironmentKind::Libvirt, window, cx))
            .maybe_detail(environment.version.as_ref().map(|v| format!("libvirt {v}")))
            .status_text(
                format!("{running} / {} running", environment.domains.len()),
                theme,
            )
            .open(open);
        let click = self.line_click(Some((key.clone(), true)), None, cx);
        let mut lines = vec![tree_line(spec, theme, click)];
        if !open {
            return lines;
        }
        for domain in &visible {
            let up = domain.state == "running";
            let console_target = EnvironmentTarget::LibvirtConsole {
                domain: domain.name.clone(),
                uri: domain.uri,
            };
            let console = PanelAction::Target(host.clone(), console_target.clone());
            let power = |action: LibvirtAction| PanelAction::Act {
                host: host.clone(),
                action: EnvironmentAction::Libvirt {
                    uri: domain.uri,
                    domain: domain.name.clone(),
                    action,
                },
                confirm: match action {
                    LibvirtAction::Destroy => Some((
                        format!("Force off {}?", domain.name),
                        "The VM is switched off without shutting down.".to_string(),
                    )),
                    LibvirtAction::Shutdown => Some((
                        format!("Shut down {}?", domain.name),
                        "The VM is asked to shut down.".to_string(),
                    )),
                    _ => None,
                },
            };
            let guest_ref = GuestRef::Libvirt(domain.uri, domain.name.clone());
            let id = format!(
                "{key}/{}:{}",
                domain.uri.map(LibvirtUri::as_str).unwrap_or(""),
                domain.name
            );
            let mut spec = LineSpec::new(id.clone(), 2, domain.name.clone())
                .icon(status_dot(
                    if up {
                        Health::Up
                    } else if domain.state == "paused" {
                        Health::Warn
                    } else {
                        Health::Off
                    },
                    theme,
                ))
                .maybe_detail((domain.uri == Some(LibvirtUri::Session)).then_some("user session"))
                .status_text(domain.state.clone(), theme)
                .hint("State", domain.state.clone())
                .hint("Connection", domain.uri.map(|uri| uri.as_str().to_string()))
                .hint("ID", domain.id.map(|id| id.to_string()));
            spec.dim = !up;
            if up {
                spec = spec
                    .action(self.line_action(
                        format!("env-console-{id}"),
                        "Serial console (virsh console)",
                        "TerminalOutlined",
                        console.clone(),
                        false,
                        cx,
                    ))
                    .action(self.line_action(
                        format!("env-vnc-{id}"),
                        if system {
                            unavailable_with_system_open_ssh("VNC display")
                        } else {
                            "VNC display".into()
                        },
                        "ScreenShareOutlined",
                        PanelAction::Vnc {
                            host: host.clone(),
                            uri: domain.uri,
                            domain: domain.name.clone(),
                        },
                        system,
                        cx,
                    ))
                    .action(self.line_action(
                        format!("env-ssh-{id}"),
                        if system {
                            unavailable_with_system_open_ssh("SSH to the VM")
                        } else {
                            "SSH to the VM".into()
                        },
                        "LoginOutlined",
                        PanelAction::SshGuest {
                            host: host.clone(),
                            guest: guest_ref.clone(),
                            name: domain.name.clone(),
                        },
                        system,
                        cx,
                    ));
            } else {
                spec = spec.action(self.line_action(
                    format!("env-start-{id}"),
                    "Start",
                    "PlayArrowOutlined",
                    power(LibvirtAction::Start),
                    false,
                    cx,
                ));
            }
            let mut menu = Vec::new();
            if up {
                menu.push(MenuEntry::new("Reboot", power(LibvirtAction::Reboot)));
                menu.push(MenuEntry::new("Shut down…", power(LibvirtAction::Shutdown)));
                menu.push(
                    MenuEntry::new("Force off…", power(LibvirtAction::Destroy))
                        .destructive()
                        .divider(),
                );
            } else {
                menu.push(MenuEntry::new("Start", power(LibvirtAction::Start)).divider());
            }
            menu.push(
                MenuEntry::new(
                    "Add to hosts",
                    PanelAction::AddGuest {
                        host: host.clone(),
                        name: domain.name.clone(),
                        guest: guest_ref,
                        console: console_target,
                    },
                )
                .disabled(jump_unavailable(host)),
            );
            if let Some(menu) = self.line_menu(
                format!("env-domain-menu-{id}"),
                format!("More actions for {}", domain.name),
                menu,
                cx,
            ) {
                spec = spec.action(menu);
            }
            let click = self.line_click(None, up.then_some(console), cx);
            lines.push(tree_line(spec, theme, click));
        }
        lines.extend(hidden_note(2, domains.len() - visible.len(), "VM", theme));
        lines
    }
}
