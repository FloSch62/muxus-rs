//! Host color swatches (`HostColorPicker.tsx`), the color override input,
//! and the port-forwarding diagram (`ForwardDiagram.tsx`).

use std::rc::Rc;

use muxus_core::api::ForwardType;
use muxus_ui::color::hex;
use muxus_ui::gpui_kit::{
    AnyElement, App, BoxShadow, Hsla, InteractiveElement, IntoElement, ParentElement, PathBuilder,
    SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, point, px,
};
use muxus_ui::icon::{MEDIUM, icon};
use muxus_ui::theme::{Theme, fonts};
use muxus_ui::widgets::Tooltip;

use crate::features::host_editor::logic::forwards::forward_diagram;
use crate::features::host_editor::palette::COLOR_CHOICES;
use crate::hosts::organization::HOST_COLORS;
use muxus_ui::widgets::typography::VariantExt;
use muxus_ui::widgets::typography::weight;
use muxus_ui::widgets::typography::{body2, para, text};

/// `ForwardDiagram`'s type: the "SSH" badge (`fontSize: 10`), node titles
/// (`fontSize: 12`) and their mono sub lines (`fontSize: 10.5`).
const DIAGRAM_BADGE_SIZE: f32 = 10.;
const DIAGRAM_TITLE_SIZE: f32 = 12.;
const DIAGRAM_SUB_SIZE: f32 = 10.5;

pub type ColorHandler = Rc<dyn Fn(Option<String>, &mut Window, &mut App)>;
/// Called with the color picked in a color input's choices.
pub type PickColor = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// The swatches: "no color", then the ten host colors.
pub fn host_color_picker(
    theme: &Theme,
    id: &str,
    label: &str,
    value: Option<&str>,
    mixed: bool,
    size: f32,
    on_change: ColorHandler,
) -> AnyElement {
    let p = &theme.palette;
    let glyph = (size * 0.57).round();
    let no_color = !mixed && value.is_none();
    let mut row = div().flex().flex_wrap().gap(Theme::spacing(0.75));
    let handler = on_change.clone();
    row = row.child(
        div()
            .id(SharedString::from(format!("{id}-none")))
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_1()
            .border_color(if no_color {
                p.text.secondary
            } else {
                p.divider
            })
            .cursor_pointer()
            .tooltip(Tooltip::text("No color"))
            .on_click(move |_, window, cx| handler(None, window, cx))
            .when_some(no_color.then_some(()), |d, ()| {
                d.child(icon("Check", glyph, p.text.secondary))
            }),
    );
    for (name, color) in HOST_COLORS {
        let selected = value.is_some_and(|v| v.eq_ignore_ascii_case(color));
        let fill = hex(color);
        let handler = on_change.clone();
        let mut swatch = div()
            .id(SharedString::from(format!("{id}-{name}")))
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(fill)
            .cursor_pointer()
            .tooltip(Tooltip::text(name))
            .on_click(move |_, window, cx| handler(Some(color.to_owned()), window, cx));
        if selected {
            swatch = swatch
                .shadow(vec![
                    BoxShadow {
                        color: fill,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(4.),
                        inset: false,
                    },
                    BoxShadow {
                        color: p.background_paper,
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    },
                ])
                .child(icon("Check", glyph, theme.components.host_swatch_check));
        }
        row = row.child(swatch);
    }
    div()
        .flex()
        .flex_col()
        .child(div().mb(Theme::spacing(1.)).child(text(
            label.to_owned(),
            body2().weight(weight::SEMIBOLD),
            p.text.primary,
        )))
        .child(row)
        .into_any_element()
}

trait WhenSome: Sized {
    fn when_some<T>(self, value: Option<T>, f: impl FnOnce(Self, T) -> Self) -> Self {
        match value {
            Some(value) => f(self, value),
            None => self,
        }
    }
}

impl<T: Styled> WhenSome for T {}

/// The `<input type="color">` box: a 34×30 frame showing the color.
pub fn color_box(
    theme: &Theme,
    id: impl Into<SharedString>,
    color: &str,
    width: f32,
    height: f32,
) -> muxus_ui::gpui_kit::Stateful<muxus_ui::gpui_kit::Div> {
    div()
        .id(muxus_ui::gpui_kit::ElementId::Name(id.into()))
        .w(px(width))
        .h(px(height))
        .p(Theme::spacing(0.25))
        .rounded(px(theme.radius * 0.75))
        .border_1()
        .border_color(theme.palette.divider)
        .cursor_pointer()
        .child(
            div()
                .size_full()
                .rounded(px(theme.radius * 0.25))
                .border_1()
                .border_color(theme.components.color_input_frame)
                .bg(muxus_ui::color::try_hex(color).unwrap_or(Hsla::transparent_black())),
        )
}

/// The color choices a color box opens.
pub fn color_choices(theme: &Theme, current: &str, on_pick: PickColor) -> impl IntoElement {
    let mut grid = div()
        .flex()
        .flex_wrap()
        .gap(Theme::spacing(0.75))
        .w(px(10. * 22. + 9. * 6.));
    for color in COLOR_CHOICES {
        let handler = on_pick.clone();
        let selected = current.eq_ignore_ascii_case(color);
        grid = grid.child(
            div()
                .id(SharedString::from(format!("choice-{color}")))
                .size(px(22.))
                .rounded(px(theme.radius * 0.5))
                .border_1()
                .border_color(if selected {
                    theme.palette.text.primary
                } else {
                    theme.palette.divider
                })
                .bg(hex(color))
                .cursor_pointer()
                .on_click(move |_, window, cx| handler(color.to_owned(), window, cx)),
        );
    }
    grid
}

/// A dashed connector line, `repeating-linear-gradient(90deg, c 0 6px, transparent 6px 12px)`.
fn dashes(color: Hsla) -> impl IntoElement {
    let mut line = div().flex().h(px(2.)).w_full().overflow_hidden();
    for _ in 0..80 {
        line = line
            .child(div().flex_none().w(px(6.)).h(px(2.)).bg(color))
            .child(div().flex_none().w(px(6.)));
    }
    line
}

fn arrowhead(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let mut path = PathBuilder::fill();
            let o = bounds.origin;
            path.move_to(o);
            path.line_to(point(o.x + px(6.), o.y + px(4.)));
            path.line_to(point(o.x, o.y + px(8.)));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .w(px(6.))
    .h(px(8.))
}

fn connector(theme: &Theme, tunnel: bool) -> impl IntoElement {
    let p = &theme.palette;
    let color = if tunnel {
        p.primary.main
    } else {
        p.text.disabled
    };
    let mut connector = div()
        .relative()
        .flex_1()
        .min_w(px(24.))
        .h(px(30.))
        .mx(Theme::spacing(0.5))
        .mt(-Theme::spacing(3.5))
        .child(
            div()
                .absolute()
                .top(px(14.))
                .left(px(0.))
                .right(px(0.))
                .child(dashes(color)),
        )
        .child(
            div()
                .absolute()
                .top(px(12.))
                .right(px(-1.))
                .child(arrowhead(color)),
        );
    if tunnel {
        connector = connector.child(
            div()
                .absolute()
                .top(px(-2.))
                .left(px(0.))
                .right(px(0.))
                .flex()
                .justify_center()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::spacing(0.25))
                        .px(Theme::spacing(0.5))
                        .rounded(px(theme.radius * 0.5))
                        .bg(p.background_paper)
                        .child(icon("LockOutlined", 11., p.primary.main))
                        .child(
                            text(
                                "SSH",
                                body2()
                                    .size(DIAGRAM_BADGE_SIZE)
                                    .weight(weight::BOLD)
                                    .line_height(DIAGRAM_BADGE_SIZE * 1.5)
                                    .tracking(0.4),
                                p.primary.main,
                            )
                            .no_truncate(),
                        ),
                ),
        );
    }
    connector
}

/// The "what does this tunnel actually do?" picture.
pub fn diagram(
    theme: &Theme,
    kind: ForwardType,
    bind_port: &str,
    target_host: &str,
    target_port: &str,
    server: &str,
) -> AnyElement {
    let p = &theme.palette;
    let model = forward_diagram(kind, bind_port, target_host, target_port, server);
    let mut row = div().flex().items_stretch();
    let count = model.nodes.len();
    for (index, node) in model.nodes.iter().enumerate() {
        let last = index + 1 == count;
        let node_el = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(Theme::spacing(0.5))
            .w(px(118.))
            .flex_none()
            .child(
                div()
                    .size(px(44.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(p.action.hover)
                    .border_1()
                    .border_color(p.divider)
                    .child(icon(node.icon.icon_name(), MEDIUM, p.primary.main)),
            )
            .child(
                div().max_w(px(118.)).overflow_hidden().child(text(
                    node.title.clone(),
                    body2()
                        .size(DIAGRAM_TITLE_SIZE)
                        .weight(weight::SEMIBOLD)
                        .line_height(DIAGRAM_TITLE_SIZE * 1.43)
                        .tracking(DIAGRAM_TITLE_SIZE * 0.01071),
                    p.text.primary,
                )),
            )
            .child(
                div()
                    .w_full()
                    .text_center()
                    .font_family(fonts::MONO)
                    .text_size(px(DIAGRAM_SUB_SIZE))
                    .line_height(px(DIAGRAM_SUB_SIZE * 1.43))
                    .text_color(p.text.secondary)
                    .child(node.sub.clone()),
            );
        let mut cell = div().flex().items_center().min_w_0();
        cell = if last {
            cell.flex_none()
        } else {
            cell.flex_1().flex_basis(px(0.))
        };
        cell = cell.child(node_el);
        if !last {
            cell = cell.child(connector(theme, index == 0));
        }
        row = row.child(cell);
    }
    div()
        .flex()
        .flex_col()
        .child(row)
        .child(
            div()
                .mt(Theme::spacing(1.5))
                .flex()
                .justify_center()
                .child(para(model.summary, body2(), p.text.secondary).center()),
        )
        .into_any_element()
}
