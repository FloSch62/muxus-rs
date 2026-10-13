//! The one dialog anatomy every host editor renders into
//! (`host-editor/EditorShell.tsx`): title with a storage caption,
//! connection-type tabs while creating, a left section rail beside a
//! fixed-height content area, and a shared action row. Keeping it identical
//! for every connection type is what stops the dialog from jumping around
//! when the connection type changes.
//!
//! Also the plain dialog frames the other host dialogs share, composed from
//! muxus-ui's dialog paper, title, content and actions.

use std::rc::Rc;

use muxus_core::api::SavedHostKind;
use muxus_ui::gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::theme::Theme;
use muxus_ui::widgets::dialog::{dialog_actions, dialog_content, dialog_paper, dialog_title};
use muxus_ui::widgets::typography::{VariantExt, body2, caption, h6, para, text};
use muxus_ui::widgets::{Button, ButtonColor, ButtonVariant, Tab};

pub type Action = Rc<dyn Fn(&mut Window, &mut App)>;
/// Called with the connection type picked in the "Add host" tabs.
pub type KindHandler = Rc<dyn Fn(SavedHostKind, &mut Window, &mut App)>;
/// Called with the id of the section picked in the rail.
pub type SectionHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;
/// Called on Save (`false`) or Save & connect (`true`).
pub type SaveHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// `maxWidth` of the MUI dialog breakpoints the host dialogs use.
pub const DIALOG_XS: f32 = 444.;
pub const DIALOG_SM: f32 = 600.;
pub const DIALOG_MD: f32 = 900.;
/// The editor paper's fixed height (`EditorShell`'s `height: 650`).
const EDITOR_HEIGHT: f32 = 650.;
/// How far a dialog paper stays inside the window on each side (MUI's 32px margin).
const DIALOG_MARGIN: f32 = 32.;
/// `EditorShell`'s section rail: `minWidth: 178`.
const RAIL_MIN_WIDTH: f32 = 178.;

/// One entry of the section rail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionTab {
    pub id: SharedString,
    pub label: SharedString,
    pub icon: &'static str,
    /// Shown as "Label (n)" when above zero.
    pub count: Option<usize>,
}

impl SectionTab {
    pub fn new(id: &str, label: &str, icon: &'static str, count: Option<usize>) -> Self {
        Self {
            id: SharedString::from(id.to_owned()),
            label: SharedString::from(label.to_owned()),
            icon,
            count,
        }
    }

    pub fn text(&self) -> SharedString {
        match self.count {
            Some(count) if count > 0 => format!("{} ({count})", self.label).into(),
            _ => self.label.clone(),
        }
    }
}

/// The connection types the "Add host" tabs offer, in order.
pub const CONNECTION_KINDS: [(SavedHostKind, &str, &str); 7] = [
    (SavedHostKind::Ssh, "SSH", "DnsOutlined"),
    (SavedHostKind::Telnet, "Telnet", "LanguageOutlined"),
    (SavedHostKind::Serial, "Serial", "UsbOutlined"),
    (SavedHostKind::Rdp, "RDP", "DesktopWindowsOutlined"),
    (SavedHostKind::Vnc, "VNC", "ScreenShareOutlined"),
    (SavedHostKind::Gnmi, "gNMI", "SensorsOutlined"),
    (SavedHostKind::Netconf, "NETCONF", "AccountTreeOutlined"),
];

/// Everything the shell shows besides the section content.
pub struct ShellProps {
    pub title: SharedString,
    pub storage: SharedString,
    /// Set while creating a new host to offer the connection-type switch.
    pub kind: Option<SavedHostKind>,
    pub on_kind: Option<KindHandler>,
    pub sections: Vec<SectionTab>,
    pub section: SharedString,
    pub on_section: SectionHandler,
    /// Blocks saving and is shown as a validation warning.
    pub problem: Option<String>,
    /// Blocks saving with a neutral note.
    pub loading: Option<String>,
    pub busy: bool,
    pub on_delete: Option<Action>,
    pub delete_pending: bool,
    pub on_close: Action,
    pub on_save: SaveHandler,
    pub save_label: SharedString,
    /// Offer "Save & connect"; without it the save button is the primary action.
    pub connect_action: bool,
    pub scroll: ScrollHandle,
}

/// Width and height of the editor paper: `maxWidth="md" fullWidth`, 650px
/// tall, kept inside the window.
pub fn paper_size(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    (
        DIALOG_MD.min(muxus_ui::zoom::unzoom(viewport.width) - 2. * DIALOG_MARGIN),
        EDITOR_HEIGHT.min(muxus_ui::zoom::unzoom(viewport.height) - 2. * DIALOG_MARGIN),
    )
}

/// The editors' `DialogActions` padding (`px: 3, pb: 2`).
fn editor_actions() -> muxus_ui::gpui_kit::Div {
    dialog_actions()
        .px(Theme::spacing(3.))
        .pt(Theme::spacing(1.))
        .pb(Theme::spacing(2.))
}

pub fn editor_shell(
    theme: &Theme,
    window: &Window,
    props: ShellProps,
    content: AnyElement,
) -> impl IntoElement {
    let p = &theme.palette;
    let (width, height) = paper_size(window);
    let title = dialog_title(theme)
        .flex_col()
        .items_stretch()
        .pb(Theme::spacing(1.))
        .child(text(props.title, h6(), p.text.primary))
        // Inside DialogTitle the caption inherits the h6's tracking.
        .child(text(
            props.storage,
            caption().tracking(h6().letter_spacing),
            p.text.secondary,
        ));

    let mut body = dialog_content(theme, true, false)
        .overflow_hidden()
        .pb(Theme::spacing(0.5))
        // MUI drops the content's top padding after a title; the theme takes
        // it back as padding and hands it straight back as margin, so floating
        // labels at the very top are not clipped.
        .mt(-Theme::spacing(1.5))
        .pt(Theme::spacing(1.5));
    if let (Some(kind), Some(on_kind)) = (props.kind, props.on_kind.clone()) {
        body = body.child(type_tabs(theme, kind, on_kind));
    }
    let rail = section_rail(
        theme,
        &props.sections,
        &props.section,
        props.on_section.clone(),
    );
    body = body.child(
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .gap(Theme::spacing(2.5))
            .pt(if props.kind.is_some() {
                Theme::spacing(1.5)
            } else {
                px(0.)
            })
            .child(rail)
            .child(
                div()
                    .id("editor-content")
                    .track_scroll(&props.scroll)
                    .flex_1()
                    .min_w_0()
                    .overflow_y_scroll()
                    .pt(Theme::spacing(1.5))
                    .pr(Theme::spacing(0.5))
                    .pb(Theme::spacing(1.))
                    .child(content),
            ),
    );

    let blocked = props.problem.is_some() || props.loading.is_some() || props.busy;
    let mut actions = editor_actions().justify_start();
    if let Some(on_delete) = props.on_delete.clone() {
        actions = actions.child(
            Button::new("editor-delete", "Delete")
                .color(ButtonColor::Error)
                .disabled(props.delete_pending)
                .on_click(move |_, window, cx| on_delete(window, cx)),
        );
    }
    // Validation is the user's problem to fix; a pending load is ours, so
    // they read as different things even though both hold Save back.
    let note = match (&props.problem, &props.loading) {
        (Some(problem), _) => Some((problem.clone(), p.warning.main)),
        (None, Some(loading)) => Some((loading.clone(), p.text.secondary)),
        (None, None) => None,
    };
    if let Some((note, ink)) = note {
        actions = actions.child(
            div()
                .ml(Theme::spacing(1.))
                .flex_shrink(1.)
                .min_w_0()
                .child(para(note, caption(), ink)),
        );
    }
    let (close, save, connect) = (
        props.on_close.clone(),
        props.on_save.clone(),
        props.on_save.clone(),
    );
    actions = actions
        .child(div().flex_1())
        .child(
            Button::new("editor-cancel", "Cancel").on_click(move |_, window, cx| close(window, cx)),
        )
        .child(
            Button::new("editor-save", props.save_label.clone())
                .variant(if props.connect_action {
                    ButtonVariant::Text
                } else {
                    ButtonVariant::Contained
                })
                .disabled(blocked)
                .on_click(move |_, window, cx| save(false, window, cx)),
        );
    if props.connect_action {
        actions = actions.child(
            Button::new("editor-save-connect", "Save & connect")
                .contained()
                .disabled(blocked)
                .on_click(move |_, window, cx| connect(true, window, cx)),
        );
    }

    dialog_paper(theme)
        .w(px(width))
        .h(px(height))
        .child(title)
        .child(body)
        .child(actions)
}

/// The vertical `Tabs` of sections: a divider on the right, 42px rows with
/// the icon 16px before the label, the indicator over the divider.
fn section_rail(
    theme: &Theme,
    sections: &[SectionTab],
    selected: &SharedString,
    on_section: SectionHandler,
) -> impl IntoElement {
    let mut rail = div()
        .flex()
        .flex_col()
        .flex_none()
        .min_w(px(RAIL_MIN_WIDTH))
        .border_r_1()
        .border_color(theme.palette.divider);
    for section in sections {
        let id = section.id.clone();
        let handler = on_section.clone();
        rail = rail.child(
            Tab::new(
                ElementId::Name(format!("section-{}", section.id).into()),
                section.text(),
                &section.id == selected,
            )
            .vertical()
            // The icon wrapper's 8px margin plus the tab's 8px gap.
            .icon(section.icon, 16.)
            .min_width(0.)
            .min_height(42.)
            .padding(4., 16., 9.)
            .indicator_offset(-1.)
            .on_click(move |_, window, cx| handler(id.clone(), window, cx)),
        );
    }
    rail
}

/// `ConnectionTypeTabs`: full-width tabs over a divider.
fn type_tabs(theme: &Theme, kind: SavedHostKind, on_kind: KindHandler) -> impl IntoElement {
    let mut row = div()
        .flex()
        .flex_none()
        .w_full()
        .border_b_1()
        .border_color(theme.palette.divider);
    for (value, label, icon_name) in CONNECTION_KINDS {
        let handler = on_kind.clone();
        row = row.child(
            Tab::new(
                ElementId::Name(format!("kind-{}", value.as_str()).into()),
                label,
                value == kind,
            )
            .fill()
            // `gap: 0.5` plus the icon wrapper's 8px margin.
            .icon(icon_name, 12.)
            .min_height(40.)
            .padding(8., 8., 9.)
            .indicator_offset(-1.)
            .on_click(move |_, window, cx| handler(value, window, cx)),
        );
    }
    row
}

/// A small modal question over the editor (the host-delete confirmation).
pub fn confirm_paper(
    theme: &Theme,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_cancel: Action,
    on_confirm: Action,
) -> impl IntoElement {
    let p = &theme.palette;
    dialog_paper(theme)
        .w(px(DIALOG_XS))
        .child(dialog_title(theme).child(text(title, h6(), p.text.primary)))
        .child(
            div()
                .px(Theme::spacing(3.))
                .pb(Theme::spacing(2.5))
                .child(para(description, body2(), p.text.secondary)),
        )
        .child(
            dialog_actions()
                .child(
                    Button::new("confirm-cancel", "Cancel")
                        .on_click(move |_, window, cx| on_cancel(window, cx)),
                )
                .child(
                    Button::new("confirm-ok", confirm_label)
                        .contained()
                        .color(ButtonColor::Error)
                        .on_click(move |_, window, cx| on_confirm(window, cx)),
                ),
        )
}

/// A dialog's width: its `maxWidth` breakpoint, kept inside the window.
pub fn dialog_width(window: &Window, max: f32) -> f32 {
    max.min(muxus_ui::zoom::unzoom(window.viewport_size().width) - 2. * DIALOG_MARGIN)
}

/// A plain MUI dialog: `DialogTitle` (`title_pb` below the h6), a scrolling
/// `DialogContent` and the `DialogActions` row (`padded`: the editors'
/// `px: 3, pb: 2`).
#[allow(clippy::too_many_arguments)]
pub fn dialog_frame(
    theme: &Theme,
    window: &Window,
    id: &'static str,
    max_width: f32,
    title: impl Into<SharedString>,
    title_pb: f32,
    content: AnyElement,
    actions: Vec<AnyElement>,
    padded: bool,
) -> AnyElement {
    dialog_frame_with(
        theme, window, id, max_width, title, title_pb, content, actions, padded, false,
    )
}

/// [`dialog_frame`] for a dialog whose parts sit in a `<form>`: MUI's flex
/// layout then stops at the form, so the whole paper scrolls (title and
/// actions included) instead of the content alone.
#[allow(clippy::too_many_arguments)]
pub fn form_dialog_frame(
    theme: &Theme,
    window: &Window,
    id: &'static str,
    max_width: f32,
    title: impl Into<SharedString>,
    title_pb: f32,
    content: AnyElement,
    actions: Vec<AnyElement>,
) -> AnyElement {
    dialog_frame_with(
        theme, window, id, max_width, title, title_pb, content, actions, true, true,
    )
}

#[allow(clippy::too_many_arguments)]
fn dialog_frame_with(
    theme: &Theme,
    window: &Window,
    id: &'static str,
    max_width: f32,
    title: impl Into<SharedString>,
    title_pb: f32,
    content: AnyElement,
    actions: Vec<AnyElement>,
    padded: bool,
    scroll_all: bool,
) -> AnyElement {
    let p = &theme.palette;
    let width = dialog_width(window, max_width);
    let max_height = muxus_ui::zoom::unzoom(window.viewport_size().height) - 2. * DIALOG_MARGIN;
    let actions_row = if padded {
        editor_actions()
    } else {
        dialog_actions()
    }
    .children(actions);
    let title = dialog_title(theme)
        .pb(px(title_pb))
        .child(text(title, h6(), p.text.primary));
    if scroll_all {
        return dialog_paper(theme)
            .w(px(width))
            .max_h(px(max_height))
            .child(
                div()
                    .id(id)
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .child(title)
                    .child(
                        div()
                            .flex_none()
                            .px(Theme::spacing(3.))
                            .pb(Theme::spacing(2.5))
                            .child(content),
                    )
                    .child(actions_row),
            )
            .into_any_element();
    }
    dialog_paper(theme)
        .w(px(width))
        .max_h(px(max_height))
        .child(title)
        .child(
            dialog_content(theme, true, false)
                .id(id)
                .overflow_y_scroll()
                .mt(-Theme::spacing(1.5))
                .pt(Theme::spacing(1.5))
                .child(content),
        )
        .child(actions_row)
        .into_any_element()
}
