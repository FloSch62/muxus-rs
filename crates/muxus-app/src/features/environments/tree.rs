//! The panel's tree lines (`components/environments/EnvironmentTree.tsx`):
//! chevron, icon, name with a dim detail, a status column and the line's own
//! actions. Hosts, providers, groups and items all use one line, so every
//! level reads the same way.

use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, ClickEvent, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{Theme, fonts, weight};
use muxus_ui::widgets::{Placement, Tooltip};

/// Indent of one tree level, in px.
pub const LEVEL: f32 = 16.;

/// How a line's dot reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Up,
    Warn,
    Down,
    Off,
}

/// An 8px status dot (`StatusDot`).
pub fn status_dot(health: Health, theme: &Theme) -> AnyElement {
    let p = theme.palette;
    let dot = div().size(px(8.)).rounded(px(4.)).flex_none();
    match health {
        Health::Off => dot.border(px(1.5)).border_color(p.text.disabled),
        Health::Up => dot.bg(p.success.main),
        Health::Warn => dot.bg(p.warning.main),
        Health::Down => dot.bg(p.error.main),
    }
    .into_any_element()
}

/// What one line shows (`TreeLine` props).
pub struct LineSpec {
    pub id: String,
    pub depth: usize,
    /// `None`: a leaf, no chevron.
    pub open: Option<bool>,
    pub icon: Option<AnyElement>,
    pub title: SharedString,
    /// The title in the monospace face (pod names).
    pub title_mono: bool,
    pub detail: Option<SharedString>,
    pub detail_mono: bool,
    pub status: Option<AnyElement>,
    pub actions: Vec<AnyElement>,
    pub emphasis: bool,
    pub dim: bool,
    /// Label and value pairs shown when the pointer rests on the line; empty values are left out.
    pub hint: Vec<(&'static str, String)>,
}

impl LineSpec {
    pub fn new(id: impl Into<String>, depth: usize, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            depth,
            open: None,
            icon: None,
            title: title.into(),
            title_mono: false,
            detail: None,
            detail_mono: false,
            status: None,
            actions: Vec::new(),
            emphasis: false,
            dim: false,
            hint: Vec::new(),
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = Some(open);
        self
    }

    pub fn icon(mut self, icon: impl IntoElement) -> Self {
        self.icon = Some(icon.into_any_element());
        self
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn maybe_detail(mut self, detail: Option<impl Into<SharedString>>) -> Self {
        self.detail = detail.map(Into::into);
        self
    }

    pub fn status_text(mut self, status: impl Into<SharedString>, theme: &Theme) -> Self {
        self.status = Some(small_text(status, theme).into_any_element());
        self
    }

    pub fn status(mut self, status: impl IntoElement) -> Self {
        self.status = Some(status.into_any_element());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn hint(mut self, label: &'static str, value: impl Into<Option<String>>) -> Self {
        if let Some(value) = value.into().filter(|value| !value.is_empty()) {
            self.hint.push((label, value));
        }
        self
    }
}

/// 11px secondary text, cut off with an ellipsis.
pub fn small_text(text: impl Into<SharedString>, theme: &Theme) -> impl IntoElement {
    label(text)
        .size(11.)
        .line_height(16.)
        .color(theme.palette.text.secondary)
}

/// A line's click handler.
pub type Click =
    Box<dyn Fn(&ClickEvent, &mut muxus_ui::gpui_kit::Window, &mut muxus_ui::gpui_kit::App)>;

/// One line of the tree. `on_click` runs on every click, with the click count, so a single
/// click can toggle and a double click open the line's main action.
pub fn tree_line(spec: LineSpec, theme: &Theme, on_click: Option<Click>) -> AnyElement {
    let p = theme.palette;
    let group: SharedString = format!("env-line-{}", spec.id).into();
    let chevron = div()
        .w(px(16.))
        .flex()
        .flex_none()
        .when_some(spec.open, |el, open| {
            el.child(icon(
                if open { "ExpandMore" } else { "ChevronRight" },
                16.,
                p.text.secondary,
            ))
        });
    let mut title = label(spec.title.clone())
        .size(13.)
        .line_height(18.)
        .weight(if spec.emphasis {
            weight::SEMIBOLD
        } else {
            weight::MEDIUM
        })
        .color(p.text.primary);
    if spec.title_mono {
        title = title.family(fonts::MONO).size(12.);
    }
    let mut text = div()
        .id(ElementId::Name(format!("env-text-{}", spec.id).into()))
        .flex()
        .flex_1()
        .min_w_0()
        .items_baseline()
        .gap(px(6.))
        .child(div().flex_shrink(1.).min_w_0().child(title));
    if let Some(detail) = spec.detail.clone() {
        let mut detail = label(detail)
            .size(11.)
            .line_height(16.)
            .color(p.text.secondary);
        if spec.detail_mono {
            detail = detail.family(fonts::MONO);
        }
        text = text.child(div().flex_shrink(100.).min_w_0().child(detail));
    }
    if !spec.hint.is_empty() {
        let lines: Vec<SharedString> = spec
            .hint
            .iter()
            .map(|(label, value)| {
                if label.is_empty() {
                    value.clone().into()
                } else {
                    format!("{label}: {value}").into()
                }
            })
            .collect();
        text = text
            .tooltip(Tooltip::card(
                spec.title.clone(),
                lines,
                None,
                Placement::Bottom,
            ))
            .tooltip_show_delay(std::time::Duration::from_millis(500));
    }
    let mut row = div()
        .id(ElementId::Name(format!("env-line-{}", spec.id).into()))
        .group(group.clone())
        .flex()
        .items_center()
        .gap(px(6.))
        .min_h(px(30.))
        .pl(px(8. + spec.depth as f32 * LEVEL))
        .pr(px(6.))
        .when(spec.open.is_some(), |el| el.cursor_pointer())
        .when(spec.dim, |el| el.opacity(0.62))
        .hover(|style| style.bg(p.action.hover))
        .child(chevron)
        .children(spec.icon.map(|icon| div().flex().flex_none().child(icon)))
        .child(text);
    if let Some(status) = spec.status {
        row = row.child(
            div()
                .flex_none()
                .max_w(px(180.))
                .overflow_hidden()
                .child(status),
        );
    }
    if !spec.actions.is_empty() {
        row = row.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .opacity(0.55)
                .group_hover(group, |style| style.opacity(1.))
                // A press on an action is the action's, not the line's (no toggle, no open).
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .children(spec.actions),
        );
    }
    if let Some(on_click) = on_click {
        row = row.on_click(move |event, window, cx| on_click(event, window, cx));
    }
    row.into_any_element()
}

/// A short note under an item, such as why a pod is not running (`NoteLine`).
pub fn note_line(
    depth: usize,
    text: impl Into<SharedString>,
    color: muxus_ui::gpui_kit::Hsla,
) -> AnyElement {
    div()
        .pl(px(8. + depth as f32 * LEVEL + 22.))
        .pr(px(8.))
        .pb(px(4.))
        .child(muxus_ui::label::paragraph(text).size(11.).color(color))
        .into_any_element()
}
