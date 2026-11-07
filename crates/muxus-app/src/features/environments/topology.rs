//! A running lab's topology (`components/environments/ClabTopologyDialog.tsx`).
//!
//! The TypeScript client draws it with the containerlab viewer, a React app in
//! a frame. Without a web view, Muxus draws the topology itself: the nodes of
//! the lab's `.clab.yml`, placed where its saved annotations put them (else on
//! a circle), and the links between them. Selecting a node offers to open it.

use std::collections::HashMap;
use std::rc::Rc;

use muxus_core::environments::{ContainerRuntime, ContainerlabLab, ContainerlabTopologyResponse};
use muxus_session::Backend;
use muxus_session::environments::EnvironmentSource;
use muxus_ui::gpui_kit::{
    AnyElement, Bounds, Context, ElementId, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, PathBuilder, Pixels, Render, SharedString, StatefulInteractiveElement, Styled,
    Task, Window, canvas, div, point, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts, weight};
use muxus_ui::widgets::dialog::dialog_paper;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::{Button, IconButton};

use super::model::{EnvironmentHost, is_running};
use super::sessions::{containerlab_node_entry, node_uses_ssh};
use super::tree::{Health, status_dot};
use crate::state::tabs::SessionSetEntry;

/// What the dialog asks the panel to do.
pub enum TopologyEvent {
    Close,
    Open(SessionSetEntry),
}

/// The nodes and links of a containerlab topology file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Topology {
    pub nodes: Vec<String>,
    pub links: Vec<(String, String)>,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    let value = value.split(" #").next().unwrap_or(value).trim();
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value)
        .to_string()
}

/// The node of an endpoint: `"leaf1:e1-1"` or a `node:` key.
fn endpoint_node(value: &str) -> String {
    let value = unquote(value);
    value.split(':').next().unwrap_or("").trim().to_string()
}

/// Read the nodes and links of a `.clab.yml`: the block-style YAML containerlab writes and
/// its documentation shows (`topology.nodes` keys, `topology.links[].endpoints` as a flow list,
/// a block list, or `node:` entries).
pub fn parse_topology(yaml: &str) -> Topology {
    let mut topology = Topology::default();
    let lines: Vec<&str> = yaml
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .collect();
    let mut section: Option<(&str, usize)> = None;
    let mut node_indent: Option<usize> = None;
    let mut endpoints: Vec<String> = Vec::new();
    let mut in_endpoints = false;
    let mut link_indent: Option<usize> = None;
    let flush = |endpoints: &mut Vec<String>, topology: &mut Topology| {
        if endpoints.len() >= 2 {
            topology
                .links
                .push((endpoints[0].clone(), endpoints[1].clone()));
        }
        endpoints.clear();
    };
    for line in lines {
        let depth = indent(line);
        let text = line.trim();
        if let Some((_, section_indent)) = section
            && depth <= section_indent
        {
            if section.is_some_and(|(name, _)| name == "links") {
                flush(&mut endpoints, &mut topology);
            }
            section = None;
            node_indent = None;
            link_indent = None;
            in_endpoints = false;
        }
        if section.is_none() {
            if text == "nodes:" {
                section = Some(("nodes", depth));
            } else if text == "links:" {
                section = Some(("links", depth));
            }
            continue;
        }
        match section {
            Some(("nodes", _)) => {
                let indent_of_node = *node_indent.get_or_insert(depth);
                if depth == indent_of_node
                    && let Some(name) = text.strip_suffix(':').or_else(|| {
                        text.split_once(':')
                            .filter(|(_, rest)| rest.trim().starts_with('{'))
                            .map(|(name, _)| name)
                    })
                {
                    let name = unquote(name);
                    if !name.is_empty() && !topology.nodes.contains(&name) {
                        topology.nodes.push(name);
                    }
                }
            }
            Some(("links", _)) => {
                let item_indent = *link_indent.get_or_insert(depth);
                let body = text.strip_prefix("- ").unwrap_or(text);
                if depth == item_indent && text.starts_with("- ") {
                    // A new link.
                    flush(&mut endpoints, &mut topology);
                    in_endpoints = false;
                }
                if let Some(list) = body.strip_prefix("endpoints:") {
                    let list = list.trim();
                    if let Some(inner) = list.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                        endpoints.extend(inner.split(',').map(endpoint_node));
                    } else {
                        in_endpoints = true;
                    }
                } else if let Some(node) = body.strip_prefix("node:") {
                    endpoints.push(endpoint_node(node));
                } else if in_endpoints && text.starts_with("- ") && !body.contains(": ") {
                    endpoints.push(endpoint_node(body));
                }
            }
            _ => {}
        }
    }
    if section.is_some_and(|(name, _)| name == "links") {
        flush(&mut endpoints, &mut topology);
    }
    topology
        .links
        .retain(|(a, b)| !a.is_empty() && !b.is_empty());
    topology
}

/// Saved node positions from `<topology>.annotations.json` (`nodeAnnotations[].position`).
pub fn annotation_positions(annotations: Option<&str>) -> HashMap<String, (f32, f32)> {
    let Some(value) =
        annotations.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
    else {
        return HashMap::new();
    };
    let mut positions = HashMap::new();
    if let Some(nodes) = value
        .get("nodeAnnotations")
        .and_then(|nodes| nodes.as_array())
    {
        for node in nodes {
            let id = node.get("id").and_then(|id| id.as_str());
            let x = node.pointer("/position/x").and_then(|x| x.as_f64());
            let y = node.pointer("/position/y").and_then(|y| y.as_f64());
            if let (Some(id), Some(x), Some(y)) = (id, x, y) {
                positions.insert(id.to_string(), (x as f32, y as f32));
            }
        }
    }
    positions
}

/// Where each node sits in a `width` × `height` area: the saved positions scaled to fit,
/// or a circle.
pub fn layout(
    topology: &Topology,
    positions: &HashMap<String, (f32, f32)>,
    width: f32,
    height: f32,
) -> HashMap<String, (f32, f32)> {
    let margin = 60.;
    let mut placed = HashMap::new();
    let saved: Vec<(&String, (f32, f32))> = topology
        .nodes
        .iter()
        .filter_map(|node| positions.get(node).map(|at| (node, *at)))
        .collect();
    if !saved.is_empty() && saved.len() == topology.nodes.len() {
        let (min_x, max_x) = saved
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), (_, (x, _))| {
                (lo.min(*x), hi.max(*x))
            });
        let (min_y, max_y) = saved
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), (_, (_, y))| {
                (lo.min(*y), hi.max(*y))
            });
        let span_x = (max_x - min_x).max(1.);
        let span_y = (max_y - min_y).max(1.);
        let scale = ((width - 2. * margin) / span_x).min((height - 2. * margin) / span_y);
        let offset_x = (width - span_x * scale) / 2.;
        let offset_y = (height - span_y * scale) / 2.;
        for (node, (x, y)) in saved {
            placed.insert(
                node.clone(),
                (
                    offset_x + (x - min_x) * scale,
                    offset_y + (y - min_y) * scale,
                ),
            );
        }
        return placed;
    }
    let count = topology.nodes.len().max(1) as f32;
    let radius = ((width.min(height) / 2.) - margin).max(40.);
    for (index, node) in topology.nodes.iter().enumerate() {
        let angle = std::f32::consts::TAU * index as f32 / count - std::f32::consts::FRAC_PI_2;
        placed.insert(
            node.clone(),
            (
                width / 2. + radius * angle.cos(),
                height / 2. + radius * angle.sin(),
            ),
        );
    }
    placed
}

enum TopologyState {
    Loading,
    Ready(ContainerlabTopologyResponse, Topology),
    Error(String),
}

pub struct TopologyDialog {
    host: EnvironmentHost,
    lab: ContainerlabLab,
    state: TopologyState,
    selected: Option<String>,
    _task: Task<()>,
}

impl EventEmitter<TopologyEvent> for TopologyDialog {}

impl TopologyDialog {
    pub fn new(
        host: EnvironmentHost,
        lab: ContainerlabLab,
        source: Task<Result<Option<EnvironmentSource>, String>>,
        backend: Backend,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let runtime: ContainerRuntime = lab.runtime;
        let name = lab.name.clone();
        let label = host.label.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = match source.await {
                Ok(Some(source)) => backend
                    .environment_topology(source, runtime, name)
                    .await
                    .map_err(|error| error.message),
                Ok(None) => Err(format!("{label} is not connected")),
                Err(message) => Err(message),
            };
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(response) => {
                        let topology = parse_topology(&response.yaml);
                        if topology.nodes.is_empty() {
                            TopologyState::Error("The topology could not be drawn.".into())
                        } else {
                            TopologyState::Ready(response, topology)
                        }
                    }
                    Err(message) => TopologyState::Error(message),
                };
                cx.notify();
            });
        });
        Self {
            host,
            lab,
            state: TopologyState::Loading,
            selected: None,
            _task: task,
        }
    }

    fn render_graph(
        &self,
        topology: &Topology,
        annotations: Option<&str>,
        width: f32,
        height: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let positions = layout(topology, &annotation_positions(annotations), width, height);
        let segments: Vec<((f32, f32), (f32, f32))> = topology
            .links
            .iter()
            .filter_map(|(a, b)| Some((*positions.get(a)?, *positions.get(b)?)))
            .collect();
        let link_color = p.text.disabled;
        let selected_ink = p.primary.main;
        let lines = canvas(
            |_, _, _| (),
            move |bounds: Bounds<Pixels>, _, window, _| {
                for ((ax, ay), (bx, by)) in &segments {
                    let mut builder = PathBuilder::stroke(px(1.5));
                    builder.move_to(point(bounds.origin.x + px(*ax), bounds.origin.y + px(*ay)));
                    builder.line_to(point(bounds.origin.x + px(*bx), bounds.origin.y + px(*by)));
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, link_color);
                    }
                }
            },
        )
        .absolute()
        .inset_0();
        let mut area = div().relative().w(px(width)).h(px(height)).child(lines);
        for node in &topology.nodes {
            let Some((x, y)) = positions.get(node).copied() else {
                continue;
            };
            let info = self
                .lab
                .nodes
                .iter()
                .find(|candidate| &candidate.name == node);
            let up = info.is_some_and(|info| is_running(&info.state));
            let selected = self.selected.as_deref() == Some(node.as_str());
            let name = node.clone();
            let entity = cx.entity().downgrade();
            area = area.child(
                div()
                    .id(ElementId::Name(format!("topology-node-{node}").into()))
                    .absolute()
                    .left(px(x - 60.))
                    .top(px(y - 14.))
                    .w(px(120.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if selected { selected_ink } else { p.divider })
                    .bg(p.background_paper)
                    .cursor_pointer()
                    .child(status_dot(if up { Health::Up } else { Health::Off }, theme))
                    .child(label(node.clone()).size(12.).color(p.text.primary))
                    .on_click(move |_, _, cx| {
                        let name = name.clone();
                        let _ = entity.update(cx, |this, cx| {
                            this.selected = Some(name);
                            cx.notify();
                        });
                    }),
            );
        }
        area.into_any_element()
    }
}

impl Render for TopologyDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let viewport = window.viewport_size();
        let width = (muxus_ui::zoom::unzoom(viewport.width) - 64.).clamp(320., 1200.);
        let height = (muxus_ui::zoom::unzoom(viewport.height) * 0.82).max(320.);
        let graph_height = (height - 120.).max(200.);
        let entity = cx.entity().downgrade();
        let path = match &self.state {
            TopologyState::Ready(response, _) => response.path.clone(),
            _ => self.lab.topology_file.clone().unwrap_or_default(),
        };
        let close = entity.clone();
        let header = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .py(px(10.))
            .border_b_1()
            .border_color(p.divider)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        label(self.lab.name.clone())
                            .variant(theme.typography.subtitle1)
                            .weight(weight::BOLD)
                            .color(p.text.primary),
                    )
                    .child(
                        label(format!("{} · {path}", self.host.label))
                            .size(12.)
                            .family(fonts::MONO)
                            .color(p.text.secondary),
                    ),
            )
            .child(
                IconButton::new("topology-close", "Close")
                    .tooltip("Close")
                    .on_click(move |_, _, cx| {
                        let _ = close.update(cx, |_, cx| cx.emit(TopologyEvent::Close));
                    }),
            );
        let body: AnyElement = match &self.state {
            TopologyState::Loading => div()
                .h(px(graph_height))
                .flex()
                .items_center()
                .justify_center()
                .child(CircularProgress::new(
                    "topology-loading",
                    28.,
                    p.primary.main,
                ))
                .into_any_element(),
            TopologyState::Error(message) => div()
                .h(px(graph_height))
                .p(px(16.))
                .child(
                    paragraph(message.clone())
                        .variant(theme.typography.body2)
                        .color(theme.status_text_color(Status::Error)),
                )
                .into_any_element(),
            TopologyState::Ready(response, topology) => self.render_graph(
                topology,
                response.annotations.as_deref(),
                width,
                graph_height,
                &theme,
                cx,
            ),
        };
        let mut paper = dialog_paper(&theme)
            .w(px(width))
            .h(px(height))
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().min_h_0().overflow_hidden().child(body));
        let node = self
            .selected
            .as_ref()
            .and_then(|name| self.lab.nodes.iter().find(|node| &node.name == name));
        if let Some(node) = node {
            let up = is_running(&node.state);
            let ssh = node_uses_ssh(node);
            let system = self.host.uses_system_open_ssh();
            let entry = containerlab_node_entry(&self.host, &self.lab, node);
            let open = entity.clone();
            paper = paper.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(p.divider)
                    .child(status_dot(
                        if up { Health::Up } else { Health::Off },
                        &theme,
                    ))
                    .child(
                        label(node.name.clone())
                            .size(13.)
                            .weight(weight::SEMIBOLD)
                            .color(p.text.primary),
                    )
                    .child(label(node.kind.clone()).size(12.).color(p.text.secondary))
                    .children(node.ipv4.clone().map(|ip| {
                        label(ip)
                            .size(12.)
                            .family(fonts::MONO)
                            .color(p.text.secondary)
                    }))
                    .child(div().flex_1())
                    .child(
                        Button::new(
                            SharedString::from("topology-open"),
                            if ssh { "SSH as admin" } else { "Open shell" },
                        )
                        .contained()
                        .start_icon(if ssh {
                            "LoginOutlined"
                        } else {
                            "TerminalOutlined"
                        })
                        .disabled(!up || (ssh && system))
                        .on_click(move |_, _, cx| {
                            let entry = entry.clone();
                            let _ = open.update(cx, |_, cx| cx.emit(TopologyEvent::Open(entry)));
                        }),
                    ),
            );
        }
        let dismiss = entity;
        modal_layer(
            window,
            "topology-dialog",
            paper,
            Some(Rc::new(move |_, cx| {
                let _ = dismiss.update(cx, |_, cx| cx.emit(TopologyEvent::Close));
            })),
        )
    }
}
