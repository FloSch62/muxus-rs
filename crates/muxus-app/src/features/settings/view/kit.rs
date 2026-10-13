//! The layout language every settings section shares (`SettingsLayout.tsx`):
//! a page heading, then groups (a small uppercase label above a bordered
//! card) holding rows with the label and its explanation on the left and the
//! control on the right. Plus the controls the sections use that the shared
//! widget set does not have yet: toggle button groups, the compact slider,
//! the color well, outlined fields with a floating label and outlined alerts.
//!
//! Sizes are MUI's under the Muxus theme, measured in the Electron build's
//! settings window (13px body2, 11.14px caption, 8px spacing unit).

use std::rc::Rc;

use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, BoxShadow, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement,
    IntoElement, MouseButton, ParentElement, SharedString, Stateful, StatefulInteractiveElement,
    Styled, Transformation, Window, div, point, px, relative, size,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{Status, Theme, fonts};
use muxus_ui::widgets::{AnchorBounds, bounds_probe};

pub type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
/// A choice by position (toggle groups).
pub type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// A position along a slider, 0–1.
pub type FractionHandler = Rc<dyn Fn(f32, &mut Window, &mut App)>;

/// A line height on the device-pixel grid of a 2× display. GPUI rounds a
/// text line's box up to whole device pixels, so Chrome's 18.59px body2
/// line would become 19px and every row would grow; the nearest half pixel
/// keeps stacked rows within a fraction of Chrome's positions.
pub fn snap(value: f32) -> f32 {
    (value * 2.).round() / 2.
}

/// MUI `body2` as the settings set it: 13px on a 1.43 line.
pub const BODY2: f32 = 13.;
pub const BODY2_LINE: f32 = 13. * 1.43;
/// MUI `caption` under the 13px theme: pxToRem(12).
pub const CAPTION: f32 = 12. * 13. / 14.;
/// MUI `body1` under the 13px theme (inputs, selects).
pub const BODY1: f32 = 16. * 13. / 14.;

/// `SettingsPage`: the page heading, then its groups 24px apart.
pub fn page(
    theme: &Theme,
    title: &str,
    description: Option<AnyElement>,
    children: Vec<AnyElement>,
) -> Div {
    let p = &theme.palette;
    let mut header = div().flex().flex_col().child(
        label(title.to_string())
            .size(18.)
            .weight(650.)
            .line_height(snap(18. * 1.3))
            .tracking(-0.2)
            .color(p.text.primary),
    );
    if let Some(description) = description {
        header = header.child(div().mt(px(4.)).max_w(px(620.)).child(description));
    }
    div()
        .flex()
        .flex_col()
        .gap(px(24.))
        .child(header)
        .children(children)
}

/// Body text in the secondary color (page and group descriptions).
pub fn secondary_text(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    paragraph(text)
        .size(BODY2)
        .line_height(snap(BODY2_LINE))
        .color(theme.palette.text.secondary)
        .into_any_element()
}

/// A caption paragraph (`Typography variant="caption"`), secondary by default.
pub fn caption(theme: &Theme, text: impl Into<SharedString>, line_height: f32) -> AnyElement {
    paragraph(text)
        .size(CAPTION)
        .line_height(snap(CAPTION * line_height))
        .color(theme.palette.text.secondary)
        .into_any_element()
}

/// `SettingsGroup`.
pub struct Group {
    title: Option<SharedString>,
    description: Option<AnyElement>,
    actions: Vec<AnyElement>,
    flush: bool,
    rows: Vec<AnyElement>,
}

pub fn group(title: impl Into<SharedString>) -> Group {
    let title: SharedString = title.into();
    Group {
        title: (!title.is_empty()).then_some(title),
        description: None,
        actions: Vec::new(),
        flush: false,
        rows: Vec::new(),
    }
}

impl Group {
    pub fn description(mut self, description: AnyElement) -> Self {
        self.description = Some(description);
        self
    }

    /// Sits at the end of the label line, e.g. an Add button.
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    /// No row separators: the children bring their own (lists, forms).
    pub fn flush(mut self) -> Self {
        self.flush = true;
        self
    }

    pub fn row(mut self, row: impl IntoElement) -> Self {
        self.rows.push(row.into_any_element());
        self
    }

    pub fn rows(mut self, rows: impl IntoIterator<Item = AnyElement>) -> Self {
        self.rows.extend(rows);
        self
    }

    pub fn render(self, theme: &Theme) -> AnyElement {
        let p = &theme.palette;
        let mut section = div().flex().flex_col();
        if self.title.is_some() || !self.actions.is_empty() {
            let mut head = div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_h(px(28.))
                .mb(px(6.));
            if let Some(title) = self.title {
                head = head.child(
                    label(title.to_uppercase())
                        .size(CAPTION)
                        .weight(600.)
                        .line_height(snap(CAPTION * 1.66))
                        .tracking(CAPTION * 0.06)
                        .color(p.text.secondary)
                        .no_truncate(),
                );
            }
            if !self.actions.is_empty() {
                head = head.child(
                    div()
                        .ml_auto()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .children(self.actions),
                );
            }
            section = section.child(head);
        }
        if let Some(description) = self.description {
            section = section.child(div().mb(px(10.)).max_w(px(640.)).child(description));
        }
        let flush = self.flush;
        let divider = p.divider;
        let rows = self.rows.into_iter().enumerate().map(move |(index, row)| {
            div()
                .when(!flush && index > 0, |row| {
                    row.border_t_1().border_color(divider)
                })
                .child(row)
                .into_any_element()
        });
        section
            .child(
                div()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(p.divider)
                    .rounded(px(16.))
                    .bg(p.background_paper)
                    .overflow_hidden()
                    .children(rows),
            )
            .into_any_element()
    }
}

/// `SettingRow`.
pub struct Row {
    label: AnyElement,
    description: Option<AnyElement>,
    controls: Vec<AnyElement>,
    stacked: bool,
    extra: Vec<AnyElement>,
    disabled: bool,
}

pub fn row(theme: &Theme, text: impl Into<SharedString>) -> Row {
    Row::with_label(theme, text, false)
}

impl Row {
    fn with_label(theme: &Theme, text: impl Into<SharedString>, disabled: bool) -> Self {
        Row {
            label: row_label(theme, text, disabled),
            description: None,
            controls: Vec::new(),
            stacked: false,
            extra: Vec::new(),
            disabled,
        }
    }

    /// Greys the text out along with a disabled control.
    pub fn disabled(
        mut self,
        theme: &Theme,
        text: impl Into<SharedString>,
        disabled: bool,
    ) -> Self {
        self.disabled = disabled;
        self.label = row_label(theme, text, disabled);
        self
    }

    pub fn description_text(mut self, theme: &Theme, text: impl Into<SharedString>) -> Self {
        let color = if self.disabled {
            theme.palette.text.disabled
        } else {
            theme.palette.text.secondary
        };
        self.description = Some(
            paragraph(text)
                .size(CAPTION)
                .line_height(snap(CAPTION * 1.5))
                .color(color)
                .into_any_element(),
        );
        self
    }

    pub fn description(mut self, element: impl IntoElement) -> Self {
        self.description = Some(element.into_any_element());
        self
    }

    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.controls.push(control.into_any_element());
        self
    }

    /// A wide control (a path field, a form) under the text.
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// Extra content under the row (a hint, an alert, a sub-setting).
    pub fn child(mut self, element: impl IntoElement) -> Self {
        self.extra.push(element.into_any_element());
        self
    }

    pub fn render(self) -> AnyElement {
        let mut text = div().flex().flex_col().min_w_0().child(self.label);
        if let Some(description) = self.description {
            text = text.child(div().mt(px(2.)).child(description));
        }
        let mut line = div().flex();
        if self.stacked {
            line = line.flex_col().gap(px(10.));
        } else {
            line = line
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_x(px(16.))
                .gap_y(px(8.));
            text = text.flex_grow(1.).flex_shrink(1.).flex_basis(px(240.));
        }
        line = line.child(text);
        if !self.controls.is_empty() {
            let mut controls = div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap(px(8.))
                .max_w(relative(1.))
                .children(self.controls);
            if !self.stacked {
                controls = controls.ml_auto();
            }
            line = line.child(controls);
        }
        div()
            .px(px(16.))
            .py(px(12.))
            .child(line)
            .children(self.extra)
            .into_any_element()
    }
}

fn row_label(theme: &Theme, text: impl Into<SharedString>, disabled: bool) -> AnyElement {
    let color = if disabled {
        theme.palette.text.disabled
    } else {
        theme.palette.text.primary
    };
    paragraph(text)
        .size(BODY2)
        .weight(550.)
        .line_height(snap(BODY2_LINE))
        .color(color)
        .into_any_element()
}

/// `StatusText`: a row description that reports a state, tinted, behind a
/// status icon.
pub fn status_text(theme: &Theme, tone: Status, text: impl Into<SharedString>) -> AnyElement {
    let color = theme.status_text_color(tone);
    let name = match tone {
        Status::Success => "CheckCircleOutlined",
        Status::Info => "InfoOutlined",
        Status::Warning => "WarningAmberOutlined",
        Status::Error => "ErrorOutlineOutlined",
    };
    div()
        .flex()
        .items_start()
        .gap(px(4.))
        .child(div().mt(px(1.)).child(icon(name, 15., color)))
        .child(
            div().flex_1().min_w_0().child(
                paragraph(text)
                    .size(CAPTION)
                    .weight(500.)
                    .line_height(snap(CAPTION * 1.5))
                    .color(color),
            ),
        )
        .into_any_element()
}

/// One button of a [`toggle_group`].
#[derive(Clone)]
pub struct ToggleItem {
    pub label: SharedString,
    pub icon: Option<&'static str>,
    /// Mirror the icon (the sidebar glyph draws its panel on the right).
    pub mirrored: bool,
}

impl ToggleItem {
    pub fn text(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            icon: None,
            mirrored: false,
        }
    }

    pub fn icon(label: impl Into<SharedString>, icon: &'static str, mirrored: bool) -> Self {
        Self {
            label: label.into(),
            icon: Some(icon),
            mirrored,
        }
    }
}

/// How a [`toggle_group`] sets its buttons.
#[derive(Clone, Copy)]
pub struct ToggleStyle {
    /// Horizontal padding in px (MUI small: 7px vertical).
    pub padding_x: f32,
    pub min_width: f32,
    /// `fontFamily: 'monospace'` (the cursor style toggles).
    pub mono: bool,
}

impl ToggleStyle {
    pub const TEXT: ToggleStyle = ToggleStyle {
        padding_x: 14.,
        min_width: 0.,
        mono: false,
    };
    /// The layout toggles share one width so the groups line up.
    pub const LAYOUT: ToggleStyle = ToggleStyle {
        padding_x: 12.,
        min_width: 102.,
        mono: false,
    };
    pub const MONO: ToggleStyle = ToggleStyle {
        padding_x: 12.,
        min_width: 0.,
        mono: true,
    };
}

/// MUI `ToggleButtonGroup exclusive size="small"`: buttons with collapsed
/// 1px divider borders, 8px outer corners, the selected one tinted.
pub fn toggle_group(
    id: impl Into<SharedString>,
    theme: &Theme,
    items: Vec<ToggleItem>,
    selected: Option<usize>,
    style: ToggleStyle,
    on_select: IndexHandler,
) -> Div {
    let id: SharedString = id.into();
    let p = &theme.palette;
    let font_size = 13. * 13. / 14.;
    let line = font_size * 1.75;
    let count = items.len();
    let mut group = div().flex().flex_none();
    for (index, item) in items.into_iter().enumerate() {
        let is_selected = selected == Some(index);
        let ink = if is_selected {
            p.text.primary
        } else {
            p.action.active
        };
        let base = if is_selected {
            alpha(p.text.primary, p.action.selected_opacity)
        } else {
            Hsla::transparent_black()
        };
        let hover = alpha(
            p.text.primary,
            p.action.hover_opacity
                + if is_selected {
                    p.action.selected_opacity
                } else {
                    0.
                },
        );
        let radius = px(8.);
        let mut button = div()
            .id(ElementId::Name(format!("{id}-{index}").into()))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(px(line + 16.))
            .px(px(style.padding_x))
            .min_w(px(style.min_width))
            .border_1()
            .border_color(p.divider)
            .bg(base)
            .cursor_pointer()
            .hover(move |s| s.bg(hover));
        if index > 0 {
            // Grouped buttons share their neighbour's border (MUI hides the
            // left one under a -1px margin).
            button = button.border_l_0();
        }
        if index == 0 {
            button = button.rounded_l(radius);
        }
        if index + 1 == count {
            button = button.rounded_r(radius);
        }
        if let Some(name) = item.icon {
            let mut glyph = icon(name, SMALL, ink);
            if item.mirrored {
                glyph = glyph.with_transformation(Transformation::scale(size(-1., 1.)));
            }
            button = button.child(glyph);
        }
        let text = label(item.label.clone())
            .size(font_size)
            .weight(550.)
            .line_height(snap(line))
            .color(ink)
            .no_truncate();
        button = button.child(if style.mono {
            text.family("DejaVu Sans Mono").weight(700.)
        } else {
            text
        });
        let on_select = on_select.clone();
        if !is_selected {
            button = button.on_click(move |_, window, cx| on_select(index, window, cx));
        }
        group = group.child(button);
    }
    group
}

/// The closed `RowSelect`: a compact outlined select at a fixed width that
/// records its bounds, so the owner can open the option menu below it.
#[allow(clippy::too_many_arguments)]
pub fn row_select(
    id: impl Into<ElementId>,
    width: f32,
    value: AnyElement,
    open: bool,
    disabled: bool,
    anchor: &AnchorBounds,
    theme: &Theme,
    on_open: Handler,
) -> Stateful<Div> {
    let id: ElementId = id.into();
    let group_name: SharedString = format!("select-outline-{id:?}").into();
    let p = &theme.palette;
    let line = BODY1 * 1.4375;
    let outline = if open { p.primary.main } else { p.divider };
    let hover_border = theme.components.input_hover_border;
    let mut field = div()
        .id(id)
        .relative()
        .flex()
        .items_center()
        .w(px(width))
        .max_w(relative(1.))
        .h(px(line + 17.))
        .pl(px(14.))
        .pr(px(32.))
        .rounded(px(8.))
        .font_family(fonts::UI)
        .text_size(px(BODY1))
        .line_height(px(line))
        .text_color(if disabled {
            p.text.disabled
        } else {
            p.text.primary
        })
        .child(div().flex_1().min_w_0().overflow_hidden().child(value))
        .child(
            div()
                .absolute()
                .right(px(7.))
                .top(px((line + 17. - 22.2857) / 2.))
                .child(icon(
                    "ArrowDropDown",
                    22.2857,
                    if disabled {
                        p.action.disabled
                    } else {
                        p.action.active
                    },
                )),
        )
        .child(bounds_probe(anchor));
    // The outline is an overlay (MUI's fieldset), so it never narrows the
    // text: a 150px select leaves 104px for its value, as in the browser.
    let outline_box = div().absolute().inset_0().rounded(px(8.));
    let outline_box = if open {
        outline_box.border_2().border_color(outline)
    } else if disabled {
        outline_box.border_1().border_color(p.action.disabled)
    } else {
        outline_box
            .border_1()
            .border_color(outline)
            .group_hover(group_name.clone(), move |s| s.border_color(hover_border))
    };
    field = field.group(group_name).child(outline_box);
    if !disabled {
        field = field
            .cursor_pointer()
            .on_click(move |_, window, cx| on_open(window, cx));
    }
    field
}

/// A plain one-line select value in body1.
pub fn select_value(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    label(text)
        .size(BODY1)
        .line_height(snap(BODY1 * 1.4375))
        .color(theme.palette.text.primary)
        .into_any_element()
}

/// MUI `Slider size="small"`: a 2px rail at 38% of the track color, the
/// filled track and a 12px thumb, 180px wide in a settings row.
pub fn slider(
    id: impl Into<ElementId>,
    theme: &Theme,
    fraction: f32,
    disabled: bool,
    anchor: &AnchorBounds,
    on_press: FractionHandler,
) -> Stateful<Div> {
    let p = &theme.palette;
    let color = if disabled {
        if theme.mode.is_dark() {
            p.grey.g600
        } else {
            p.grey.g400
        }
    } else {
        p.primary.main
    };
    let fraction = fraction.clamp(0., 1.);
    let width = 180.;
    let anchor_for_press = anchor.clone();
    let mut root = div()
        .id(id)
        .relative()
        .w(px(width))
        .h(px(28.))
        .flex_none()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(px(13.))
                .h(px(2.))
                .rounded(px(1.))
                .bg(alpha(color, 0.38)),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(13.))
                .h(px(2.))
                .w(px(width * fraction))
                .rounded(px(1.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .top(px(8.))
                .left(px(width * fraction - 6.))
                .size(px(12.))
                .rounded(px(6.))
                .bg(color),
        )
        .child(bounds_probe(anchor));
    if !disabled {
        root = root
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                if let Some(bounds) = anchor_for_press.get() {
                    let x = muxus_ui::zoom::unzoom(event.position.x - bounds.origin.x);
                    let fraction = (x / muxus_ui::zoom::unzoom(bounds.size.width)).clamp(0., 1.);
                    on_press(fraction, window, cx);
                }
            });
    }
    root
}

/// The value beside a slider: 44px, right aligned, tabular figures.
pub fn slider_value(theme: &Theme, text: impl Into<SharedString>, disabled: bool) -> Div {
    div().w(px(44.)).flex().justify_end().child(
        label(text)
            .size(BODY2)
            .weight(550.)
            .line_height(snap(BODY2_LINE))
            .tabular_nums()
            .color(if disabled {
                theme.palette.text.disabled
            } else {
                theme.palette.text.primary
            })
            .no_truncate(),
    )
}

/// A native color well: the swatch framed like the other controls (34×28,
/// 2px padding, a divider border), Chrome's 1px grey swatch border inside.
pub fn color_well(
    id: impl Into<ElementId>,
    theme: &Theme,
    color: Hsla,
    width: f32,
    height: f32,
    on_click: Handler,
) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .border_1()
        .border_color(theme.palette.divider)
        .rounded(px(6.))
        .cursor_pointer()
        .child(
            div()
                .absolute()
                .left(px(4.))
                .right(px(4.))
                .top(px(6.))
                .bottom(px(6.))
                .border_1()
                .border_color(theme.components.color_input_frame)
                .bg(color),
        )
        .on_click(move |_, window, cx| on_click(window, cx))
}

/// `Alert variant="outlined"`: a status-colored border and text, no fill.
pub fn outlined_alert(theme: &Theme, status: Status, message: AnyElement) -> Div {
    let p = &theme.palette;
    let color = match status {
        Status::Success => p.success,
        Status::Error => p.error,
        Status::Warning => p.warning,
        Status::Info => p.info,
    };
    let name = match status {
        Status::Success => "CheckCircleOutlined",
        Status::Error => "ErrorOutlineOutlined",
        Status::Warning => "ReportProblemOutlined",
        Status::Info => "InfoOutlined",
    };
    // MUI outlined: text darkened (lightened in dark mode) from the light tone.
    let ink = if theme.mode.is_dark() {
        muxus_ui::color::lighten(color.light, 0.6)
    } else {
        muxus_ui::color::darken(color.light, 0.6)
    };
    div()
        .flex()
        .items_start()
        .px(px(16.))
        .py(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(color.light)
        .text_color(ink)
        .child(
            div()
                .flex()
                .py(px(7.))
                .mr(px(12.))
                .child(icon(name, 22., alpha(color.main, 0.9))),
        )
        .child(div().py(px(8.)).flex_1().min_w_0().child(message))
}

/// Body2 text for an alert's message.
pub fn alert_text(theme: &Theme, status: Status, text: impl Into<SharedString>) -> AnyElement {
    let p = &theme.palette;
    let color = match status {
        Status::Success => p.success,
        Status::Error => p.error,
        Status::Warning => p.warning,
        Status::Info => p.info,
    };
    // MUI outlined: text darkened (lightened in dark mode) from the light tone.
    let ink = if theme.mode.is_dark() {
        muxus_ui::color::lighten(color.light, 0.6)
    } else {
        muxus_ui::color::darken(color.light, 0.6)
    };
    paragraph(text)
        .size(BODY2)
        .line_height(snap(BODY2_LINE))
        .color(ink)
        .into_any_element()
}

/// A rounded placeholder block (`Skeleton variant="rounded"`).
pub fn skeleton(theme: &Theme, height: f32) -> Div {
    let tone = if theme.mode.is_dark() {
        alpha(theme.palette.text.primary, 0.13)
    } else {
        alpha(theme.palette.text.primary, 0.11)
    };
    div().w(relative(1.)).h(px(height)).rounded(px(8.)).bg(tone)
}

/// A keyboard chord in the caption tier (`chordSx`): JetBrains Mono 11px.
pub fn chord_text(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    label(text)
        .family(fonts::MONO)
        .size(11.)
        .line_height(snap(CAPTION * 1.66))
        .color(color)
        .no_truncate()
        .into_any_element()
}

/// A click handler that ignores the event.
pub fn on_click(handler: Handler) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, window, cx| handler(window, cx)
}

/// The soft shadow of the layout preview (`0 1px 2px`).
pub fn hairline_shadow(theme: &Theme) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: alpha(
            theme.palette.common_black,
            if theme.mode.is_dark() { 0.4 } else { 0.06 },
        ),
        offset: point(px(0.), px(1.)),
        blur_radius: px(2.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// Weight helper for elements built with `div()` text.
pub fn weight(value: f32) -> FontWeight {
    FontWeight(value)
}

/// A modal dialog with free-form parts (`Dialog` + `DialogTitle` +
/// `DialogContent` + `DialogActions`): MUI's 32px margin, the paper at
/// `max_width` (full width up to it), the title padded 16×24, the content
/// 20×24 (`dividers`: lines above and below, 16×24).
pub struct DialogParts {
    pub max_width: f32,
    pub title: AnyElement,
    pub content: AnyElement,
    pub dividers: bool,
    pub actions: Vec<AnyElement>,
    /// DialogActions padding (x, y); MUI's is 8×8.
    pub actions_padding: (f32, f32),
}

pub fn dialog(theme: &Theme, parts: DialogParts, on_close: Option<Handler>) -> AnyElement {
    let p = &theme.palette;
    let mut content = div().flex().flex_col().flex_1().min_h_0().px(px(24.));
    content = if parts.dividers {
        content
            .py(px(16.))
            .border_t_1()
            .border_b_1()
            .border_color(p.divider)
    } else {
        content.pt(px(0.)).pb(px(20.))
    };
    let paper = muxus_ui::widgets::dialog::dialog_paper(theme)
        .m(px(32.))
        .w(px(parts.max_width))
        .max_w(px(parts.max_width))
        .child(div().px(px(24.)).py(px(16.)).child(parts.title))
        .child(content.child(parts.content))
        .when(!parts.actions.is_empty(), |paper| {
            paper.child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(parts.actions_padding.0))
                    .py(px(parts.actions_padding.1))
                    .children(parts.actions),
            )
        });
    muxus_ui::widgets::dialog::modal(
        paper,
        on_close.map(|close| -> muxus_ui::widgets::dialog::CloseHandler { close }),
    )
    .into_any_element()
}

/// The h6 a dialog title is set in (18.57px, 600).
pub fn dialog_title(theme: &Theme, text: impl Into<SharedString>) -> AnyElement {
    let h6 = theme.typography.h6;
    label(text)
        .size(h6.size)
        .weight(h6.weight)
        .line_height(snap(h6.line_height))
        .tracking(-0.2)
        .color(theme.palette.text.primary)
        .into_any_element()
}
