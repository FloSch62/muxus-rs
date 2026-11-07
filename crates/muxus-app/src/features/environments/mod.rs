//! Containers, clusters and VMs on your hosts (upstream `bff978f`, `1345c99`,
//! `693ee67`): Docker and Podman containers, Containerlab labs, Kubernetes
//! contexts and pods, Proxmox guests and libvirt domains, read with the host's
//! own tools over the connection that is already open.
//!
//! - [`model`]: what is known about each host, and the summaries the sidebar
//!   and the quick launcher show (`environments.ts`, `state/environments.ts`).
//! - [`sessions`]: the profiles of sessions into them (`environment-sessions.ts`).
//! - [`hosts`]: keeping them as sidebar entries (`environment-hosts.ts`).
//! - [`launcher`]: quick launcher entries (`environment-launcher.ts`).
//! - [`hub`]: one per window; reads hosts as tabs connect and on the refresh
//!   interval (`useEnvironmentDiscovery.ts`).
//! - [`panel`], [`sections`], [`kubernetes`], [`tree`]: the panel.
//! - [`topology`]: a lab's topology, drawn natively.
//! - [`logos`]: the tools' own logos; [`sidebar_mark`]: the mark at the end of
//!   a host's sidebar row (`EnvironmentBadges.tsx`).

pub mod hosts;
pub mod hub;
pub mod kubernetes;
pub mod launcher;
pub mod logos;
pub mod model;
pub mod panel;
pub mod sections;
pub mod sessions;
pub mod topology;
pub mod tree;

use muxus_ui::gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::{Placement, Tooltip};

use model::EnvironmentBadge;

/// How often connected hosts are read again; 0 reads them only when they connect
/// (`ENVIRONMENT_REFRESH_OPTIONS` of Settings → Behavior).
pub fn refresh_options() -> Vec<(u32, String)> {
    muxus_store::prefs::state::ENVIRONMENT_REFRESH_SECONDS
        .iter()
        .map(|&seconds| {
            let text = match seconds {
                0 => "Only when connecting".to_string(),
                s if s < 60 => format!("Every {s} seconds"),
                60 => "Every minute".to_string(),
                s => format!("Every {} minutes", s / 60),
            };
            (seconds, text)
        })
        .collect()
}

/// What a host runs, as one small mark at the end of its sidebar row: the logos of its
/// container runtimes, labs, Kubernetes contexts and VMs, overlapped so the row keeps its
/// width for the name. The counts are in the tooltip. A host that runs none of them shows
/// nothing, so the tree stays as it was (`EnvironmentBadges`).
pub fn sidebar_mark(
    key: &str,
    badges: &[EnvironmentBadge],
    open: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &mut App,
) -> Option<AnyElement> {
    if badges.is_empty() {
        return None;
    }
    let theme = *cx.theme();
    let p = theme.palette;
    let tooltip: Vec<SharedString> = badges
        .iter()
        .skip(1)
        .map(|badge| badge.label.clone().into())
        .collect();
    let mut mark = div()
        .id(ElementId::Name(format!("environment-mark-{key}").into()))
        .flex()
        .flex_none()
        .items_center()
        .h(px(18.))
        .px(px(2.))
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|style| style.bg(p.action.hover))
        .tooltip(Tooltip::card(
            badges[0].label.clone(),
            tooltip,
            None,
            Placement::Bottom,
        ))
        .on_mouse_down(muxus_ui::gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        });
    if open {
        mark = mark.bg(p.action.selected);
    }
    for (index, badge) in badges.iter().enumerate() {
        let logo = logos::environment_logo(badge.kind, 12., window, cx);
        mark = mark.child(if index == 0 {
            div().child(logo)
        } else {
            div().ml(px(-2.)).child(logo)
        });
    }
    Some(mark.into_any_element())
}

#[cfg(test)]
mod tests;
