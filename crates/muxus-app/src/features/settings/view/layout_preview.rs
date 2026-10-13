//! `components/LayoutPreview.tsx`: a miniature of the window that follows the
//! layout preferences, so moving the sidebar, the file browser or the command
//! bar is visible before Settings close. Purely decorative.

use muxus_store::prefs::state::{CommandBarPosition, FileBrowserPosition, SidebarPosition};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{Div, Hsla, ParentElement, Styled, div, px, relative};
use muxus_ui::theme::Theme;

use super::kit::hairline_shadow;

const WIDTH: f32 = 200.;
const HEIGHT: f32 = 126.;
/// Children are placed inside the 1px frame.
const INNER_WIDTH: f32 = WIDTH - 2.;
const INNER_HEIGHT: f32 = HEIGHT - 2.;
const TOP_BAR: f32 = 13.;
const COMMAND_BAR: f32 = 11.;
const SIDEBAR: f32 = 58.;
/// The vertical tabs that appear with the file browser docked in the sidebar.
const RAIL: f32 = 10.;
const FILE_PANEL: f32 = 46.;
const TAB_STRIP: f32 = 10.;

/// How strong a stroke of "text" is, per theme.
fn ink(theme: &Theme, strength: f32) -> Hsla {
    alpha(
        theme.palette.text.primary,
        if theme.mode.is_dark() {
            strength
        } else {
            strength * 0.8
        },
    )
}

/// One rounded stroke standing in for a line of text.
enum Width {
    Px(f32),
    Fraction(f32),
}

fn line(width: Width, color: Hsla, indent: f32) -> Div {
    let stroke = div()
        .h(px(3.))
        .ml(px(indent))
        .rounded(px(16.))
        .flex_none()
        .bg(color);
    match width {
        Width::Px(width) => stroke.w(px(width)),
        Width::Fraction(fraction) => stroke.w(relative(fraction)),
    }
}

fn dot(size: f32, color: Hsla) -> Div {
    div()
        .size(px(size))
        .rounded(px(size / 2.))
        .flex_none()
        .bg(color)
}

pub fn layout_preview(
    theme: &Theme,
    sidebar: SidebarPosition,
    file_browser: FileBrowserPosition,
    command_bar: CommandBarPosition,
) -> Div {
    let p = &theme.palette;
    let on_right = sidebar == SidebarPosition::Right;
    let files_in_sidebar = file_browser == FileBrowserPosition::Sidebar;
    let rail = if files_in_sidebar { RAIL } else { 0. };
    let bar_at_bottom = command_bar == CommandBarPosition::Bottom;
    let main_top = TOP_BAR + if bar_at_bottom { 0. } else { COMMAND_BAR };
    let main_bottom = if bar_at_bottom { COMMAND_BAR } else { 0. };
    let text = ink(theme, 0.16);
    let field_border = ink(theme, 0.14);

    // Top bar: logo mark on the left, a few controls on the right.
    let top = div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(TOP_BAR))
        .flex()
        .items_center()
        .gap(px(3.))
        .px(px(6.))
        .bg(p.sidebar)
        .border_b_1()
        .border_color(p.divider)
        .child(div().size(px(5.)).rounded(px(4.)).bg(p.primary.main))
        .child(line(Width::Px(18.), ink(theme, 0.28), 0.))
        .child(div().flex_1())
        .children((0..4).map(|_| dot(4., ink(theme, 0.22))));

    // Command bar: a few outlined buttons and the manage control.
    let bar_top = if bar_at_bottom {
        INNER_HEIGHT - COMMAND_BAR
    } else {
        TOP_BAR
    };
    let mut bar = div()
        .absolute()
        .left_0()
        .right_0()
        .top(px(bar_top))
        .h(px(COMMAND_BAR))
        .flex()
        .items_center()
        .gap(px(3.))
        .px(px(5.))
        .bg(p.sidebar)
        .border_color(p.divider);
    bar = if bar_at_bottom {
        bar.border_t_1()
    } else {
        bar.border_b_1()
    };
    for width in [16., 22., 13.] {
        bar = bar.child(
            div()
                .w(px(width))
                .h(px(5.))
                .rounded(px(4.))
                .border_1()
                .border_color(alpha(p.primary.main, 0.7)),
        );
    }
    bar = bar.child(div().flex_1()).child(dot(4., ink(theme, 0.22)));

    // Vertical tabs at the window edge: hosts in front, the file browser behind.
    let rail_strip = files_in_sidebar.then(|| {
        let strip = div()
            .absolute()
            .top(px(main_top))
            .bottom(px(main_bottom))
            .left(px(if on_right { INNER_WIDTH - RAIL } else { 0. }))
            .w(px(RAIL))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(3.))
            .pt(px(5.))
            .bg(p.sidebar)
            .border_color(p.divider)
            .child(div().size(px(5.)).rounded(px(4.)).bg(ink(theme, 0.34)))
            .child(div().size(px(5.)).rounded(px(4.)).bg(ink(theme, 0.14)));
        if on_right {
            strip.border_l_1()
        } else {
            strip.border_r_1()
        }
    });

    // Hosts sidebar: search box, then a short tree with one live host.
    let mut hosts = div()
        .absolute()
        .top(px(main_top))
        .bottom(px(main_bottom))
        .left(px(if on_right {
            INNER_WIDTH - rail - SIDEBAR
        } else {
            rail
        }))
        .w(px(SIDEBAR))
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(5.))
        .bg(p.sidebar)
        .border_color(p.divider)
        .child(
            div()
                .h(px(6.))
                .mb(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(field_border)
                .flex_none(),
        )
        .child(line(Width::Fraction(0.7), text, 0.))
        .child(line(Width::Fraction(0.52), text, 5.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(2.))
                .ml(px(5.))
                .child(dot(3., p.success.main))
                .child(line(Width::Px(22.), text, 0.)),
        )
        .child(line(Width::Fraction(0.6), text, 0.))
        .child(line(Width::Fraction(0.44), text, 5.));
    hosts = if on_right {
        hosts.border_l_1()
    } else {
        hosts.border_r_1()
    };

    // Pane canvas: a tab strip over a terminal with a prompt and cursor.
    let strip = div()
        .h(px(TAB_STRIP))
        .flex_none()
        .flex()
        .items_end()
        .px(px(4.))
        .bg(p.sidebar)
        .border_b_1()
        .border_color(p.divider)
        .child(
            div()
                .w(px(34.))
                .h(px(7.))
                .flex()
                .items_center()
                .gap(px(2.))
                .px(px(3.))
                .rounded_t(px(2.))
                .bg(p.background_default)
                .child(dot(3., p.success.main))
                .child(line(Width::Px(16.), text, 0.)),
        );
    let terminal = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(6.))
        .child(line(Width::Fraction(0.46), text, 0.))
        .child(line(Width::Fraction(0.68), text, 0.))
        .child(line(Width::Fraction(0.38), text, 0.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .child(line(Width::Px(14.), alpha(p.success.main, 0.8), 0.))
                .child(line(Width::Px(22.), text, 0.))
                .child(div().w(px(3.)).h(px(5.)).bg(p.primary.main).opacity(0.8)),
        );
    // The file browser beside its terminal: a path, then a few files.
    let files = (!files_in_sidebar).then(|| {
        div()
            .w(px(FILE_PANEL))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(5.))
            .bg(p.background_paper)
            .border_l_1()
            .border_color(p.divider)
            .child(
                div()
                    .h(px(6.))
                    .mb(px(2.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(field_border)
                    .flex_none(),
            )
            .children(
                [70., 55., 80., 48.]
                    .into_iter()
                    .enumerate()
                    .map(|(index, width)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .child(div().size(px(3.)).rounded(px(2.)).flex_none().bg(
                                if index < 2 {
                                    alpha(p.primary.main, 0.7)
                                } else {
                                    ink(theme, 0.22)
                                },
                            ))
                            .child(line(Width::Fraction(width / 100.), text, 0.))
                    }),
            )
    });
    let panes = div()
        .absolute()
        .top(px(main_top))
        .bottom(px(main_bottom))
        .left(px(if on_right { 0. } else { SIDEBAR + rail }))
        .right(px(if on_right { SIDEBAR + rail } else { 0. }))
        .flex()
        .flex_col()
        .child(strip)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(terminal)
                .children(files),
        );

    div()
        .relative()
        .w(px(WIDTH))
        .h(px(HEIGHT))
        .flex_none()
        .overflow_hidden()
        .rounded(px(12.))
        .border_1()
        .border_color(p.divider)
        .bg(p.background_default)
        .shadow(hairline_shadow(theme))
        .child(
            div()
                .absolute()
                .inset_0()
                .child(top)
                .child(bar)
                .children(rail_strip)
                .child(hosts)
                .child(panes),
        )
}
