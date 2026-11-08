//! Monaco's marker navigation widget (`gotoErrorWidget.ts`, a peek view): opened by F8 and
//! Shift+F8 under the problem's line, with an arrow up to it, a frame in the problem's color,
//! a header naming the file and the problem's number with buttons for the next and previous
//! problem and to close, and the message with its source and code below.
//!
//! Monaco inserts the widget as a view zone, pushing the lines below it down; the editor here
//! has no view zones, so the widget lies over those lines.

use muxus_ui::gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement,
    ParentElement, PathBuilder, Pixels, SharedString, StatefulInteractiveElement, Styled, Window,
    canvas, div, point, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::syntax::MonacoColors;
use muxus_ui::theme::fonts;
use muxus_ui::widgets::Tooltip;

use super::features::ProblemWidget;
use super::markers::Severity;

/// `MessageWidget.getHeightInLines`: at most 17 lines of message.
const MAX_MESSAGE_LINES: usize = 17;

/// The widget's height in editor lines (`computeRequiredHeight`: the message and three lines
/// for the header and frame).
pub fn height_in_lines(message: &str) -> usize {
    let lines = message.split('\n').count().max(1);
    3 + lines.min(MAX_MESSAGE_LINES)
}

/// The vertical layout of the zone, in pixels from its top, for `line_height`: the arrow,
/// then the framed container (head, body).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZoneLayout {
    pub height: f32,
    pub arrow: f32,
    pub head: f32,
    pub body: f32,
}

/// `ZoneWidget._showImpl` and `PeekViewWidget._doLayout` with `frameWidth: 1`.
pub fn zone_layout(height_in_lines: usize, line_height: f32) -> ZoneLayout {
    let height = height_in_lines as f32 * line_height;
    let arrow = (line_height / 3.).round();
    let container = height - 2. * arrow - 2.;
    let head = (line_height * 1.2).ceil();
    let body = (container - (head + 1.)).round();
    ZoneLayout {
        height,
        arrow,
        head,
        body,
    }
}

/// Where the widget goes, relative to the editor's top left (laid-out lengths): the top of
/// the zone (the bottom of the problem's line), the arrow's x, and the room left on the right
/// (the minimap and the scrollbar); and the editor's line height in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WidgetPlace {
    pub top: Pixels,
    pub arrow_x: Pixels,
    pub right: Pixels,
    pub line_height: f32,
}

/// A button's click handler.
pub type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// What the widget's buttons do.
pub struct WidgetActions {
    pub next: ClickHandler,
    pub previous: ClickHandler,
    pub close: ClickHandler,
}

fn severity_color(severity: Severity, colors: &MonacoColors) -> Hsla {
    match severity {
        Severity::Error => colors.error,
        Severity::Warning => colors.warning,
    }
}

/// `{0} of {1} problems` / `{0} of {1} problem`.
pub fn problem_count_label(number: usize, total: usize) -> String {
    if total > 1 {
        format!("{number} of {total} problems")
    } else {
        format!("{number} of {total} problem")
    }
}

fn action(
    id: &'static str,
    glyph: &'static str,
    tooltip: &'static str,
    colors: MonacoColors,
    on_click: ClickHandler,
) -> impl IntoElement {
    let hover = colors.toolbar_hover;
    div()
        .id(id)
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .tooltip(Tooltip::text(tooltip))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx)
        })
        .child(icon(glyph, 16., colors.icon_foreground))
}

/// The widget, absolutely placed in the editor's box.
pub fn render_problem_widget(
    widget: &ProblemWidget,
    file_name: SharedString,
    place: WidgetPlace,
    dark: bool,
    actions: WidgetActions,
) -> AnyElement {
    let colors = MonacoColors::new(dark);
    let frame = severity_color(widget.marker.severity, &colors);
    let mut head_background = frame;
    head_background.a *= 0.1;
    let layout = zone_layout(height_in_lines(&widget.marker.message), place.line_height);
    let glyph = match widget.marker.severity {
        Severity::Error => "CodiconError",
        Severity::Warning => "CodiconWarning",
    };
    let title = div()
        .flex()
        .items_center()
        .min_w_0()
        .ml(px(20.))
        .overflow_hidden()
        .child(div().mr(px(4.)).child(icon(glyph, 16., frame)))
        .child(
            label(file_name)
                .size(13.)
                .line_height(layout.head)
                .color(colors.peek_title),
        )
        .child(
            div().ml(px(13. * 0.9 * 0.5)).child(
                label(problem_count_label(widget.number, widget.total))
                    .size(13. * 0.9)
                    .line_height(layout.head)
                    .color(colors.peek_description),
            ),
        );
    let head = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .h(px(layout.head))
        .bg(head_background)
        .child(title)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .pr(px(2.))
                .child(action(
                    "problem-widget-next",
                    "CodiconArrowDown",
                    "Go to Next Problem (Error, Warning, Info)",
                    colors,
                    actions.next,
                ))
                .child(action(
                    "problem-widget-previous",
                    "CodiconArrowUp",
                    "Go to Previous Problem (Error, Warning, Info)",
                    colors,
                    actions.previous,
                ))
                .child(action(
                    "problem-widget-close",
                    "CodiconClose",
                    "Close",
                    colors,
                    actions.close,
                )),
        );
    // The message, line by line, the source and code faded after its last line.
    let lines: Vec<&str> = widget.marker.message.split('\n').collect();
    let count = lines.len();
    let details = widget.marker.details();
    let mut message = div()
        .flex()
        .flex_col()
        .pt(px(8.))
        .pr(px(12.))
        .pl(px(20.))
        .font_family(fonts::MONO)
        .text_size(px(13.))
        .line_height(px(place.line_height))
        .text_color(colors.foreground)
        .whitespace_nowrap();
    if widget.stale {
        message = message.opacity(0.6).italic();
    }
    for (index, line) in lines.into_iter().enumerate() {
        let mut row = div()
            .flex()
            .h(px(place.line_height))
            .child(SharedString::from(line.to_string()));
        if index + 1 == count
            && let Some(details) = &details
        {
            row = row.child(
                div()
                    .pl(px(6.))
                    .opacity(0.6)
                    .child(SharedString::from(details.clone())),
            );
        }
        message = message.child(row);
    }
    let body = div()
        .h(px(layout.body + 1.))
        .border_t_1()
        .border_color(frame)
        .bg(colors.background)
        .overflow_hidden()
        .child(message);
    let container = div()
        .absolute()
        .top(px(layout.arrow))
        .left_0()
        .right_0()
        .flex()
        .flex_col()
        .border_t_1()
        .border_b_1()
        .border_color(frame)
        .bg(colors.background)
        .font_family(fonts::UI)
        .child(head)
        .child(body);
    // The arrow: a triangle pointing up at the problem's column, in the frame's color.
    let arrow_height = px(layout.arrow);
    let arrow_x = place.arrow_x;
    let arrow = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let apex = point(bounds.origin.x + arrow_x, bounds.origin.y);
            let mut path = PathBuilder::fill();
            path.move_to(apex);
            path.line_to(point(apex.x + arrow_height, apex.y + arrow_height));
            path.line_to(point(apex.x - arrow_height, apex.y + arrow_height));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, frame);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    div()
        .id(ElementId::Name("problem-widget".into()))
        .absolute()
        .top(place.top)
        .left_0()
        .right(place.right)
        .h(px(layout.height))
        // The view zone Monaco opens is empty editor; here it covers the lines under it.
        .bg(colors.background)
        .occlude()
        .child(arrow)
        .child(container)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lays_out_the_zone_like_monaco() {
        // One line of message: four editor lines, a 6px arrow, a 22px head and the body.
        assert_eq!(height_in_lines("Expected comma"), 4);
        let layout = zone_layout(4, 18.);
        assert_eq!(layout.height, 72.);
        assert_eq!(layout.arrow, 6.);
        assert_eq!(layout.head, 22.);
        assert_eq!(layout.body, 35.);
        // Long messages stop growing the widget at 17 lines.
        assert_eq!(height_in_lines(&"line\n".repeat(30)), 20);
    }

    #[test]
    fn counts_problems_like_monaco() {
        assert_eq!(problem_count_label(1, 1), "1 of 1 problem");
        assert_eq!(problem_count_label(2, 3), "2 of 3 problems");
    }
}
