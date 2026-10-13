//! What floats over the workbench: the node menu (`NodeMenu`), "Copy as",
//! the NETCONF operation menu, the select menus, and the dialogs (review,
//! save, confirm, prompt).

use super::workbench::ListenerForApp;
use muxus_core::management::GnmiEncoding;
use muxus_mgmt::workbench::data_tree::{DataNode, DataNodeKind, find_node};
use muxus_mgmt::workbench::requests::{
    GnmiSetItemOp, ManagementDraft, NETCONF_OPERATION_GROUPS, NetconfOperation,
};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, FontWeight, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::dialog::{
    CloseHandler, DialogWidth, dialog_actions, dialog_content, dialog_paper, dialog_title,
};
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel};
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::menu::{menu_divider, paper, popup};
use muxus_ui::widgets::{Alert, Button, ButtonColor, MenuItem};

use super::fields::FieldKey;
use super::logic::diff::{DiffRow, DiffSide};
use super::logic::editor::{copy_as_items, netconf_unsupported};
use super::palette::diff_colors;
use super::ui::{ConfirmAction, Dialog, Menu, PromptAction, ReviewApply, SelectField, TreeSource};
use super::view::ManagementView;
use super::widgets::MONO;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::progress::CircularProgress;

/// A menu item with a second, dimmer line (`ListItemText secondary`).
fn two_line_item(
    id: impl Into<SharedString>,
    icon_name: Option<&str>,
    primary: AnyElement,
    secondary: Option<(String, Hsla)>,
    theme: &Theme,
) -> muxus_ui::gpui_kit::Stateful<Div> {
    let p = &theme.palette;
    let body2 = theme.typography.body2;
    let mut text = div().flex().flex_col().my(px(4.)).child(primary);
    if let Some((secondary, color)) = secondary {
        text = text.child(
            label(secondary)
                .size(body2.size)
                .line_height(body2.line_height)
                .color(color),
        );
    }
    let hover = p.action.hover;
    let mut item = div()
        .id(id.into())
        .flex()
        .items_center()
        .px(px(16.))
        .py(px(6.))
        .min_h(px(34.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover));
    if let Some(name) = icon_name {
        item = item.child(div().min_w(px(36.)).flex_none().child(icon(
            name,
            SMALL,
            p.action.active,
        )));
    }
    item.child(text)
}

impl ManagementView {
    pub(super) fn render_overlays(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut overlays = Vec::new();
        if let Some(menu) = self.ui.menu.clone() {
            overlays.push(self.render_menu(menu, window, cx));
        }
        if let Some(dialog) = self.ui.dialog.clone() {
            overlays.push(self.render_dialog(dialog, window, cx));
        }
        overlays
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        self.ui.menu = None;
        cx.notify();
    }

    fn render_menu(
        &mut self,
        menu: Menu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let dismiss =
            cx.closer_for_app(|this: &mut Self, cx: &mut Context<Self>| this.close_menu(cx));
        match menu {
            Menu::Node {
                source,
                node_id,
                position,
            } => {
                let content = self.render_node_menu(&source, &node_id, &theme, cx);
                popup(position, dismiss, content).into_any_element()
            }
            Menu::CopyAs(position) => {
                let mut list = paper(&theme);
                for (index, (text, detail, icon_name)) in
                    copy_as_items(self.is_gnmi()).iter().enumerate()
                {
                    let primary = label(*text)
                        .size(theme.typography.body1.size)
                        .line_height(theme.typography.body1.line_height)
                        .color(theme.palette.text.primary)
                        .no_truncate()
                        .into_any_element();
                    list = list.child(
                        two_line_item(
                            format!("copy-as-{index}"),
                            Some(icon_name),
                            primary,
                            Some(((*detail).to_owned(), theme.palette.text.secondary)),
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ui.menu = None;
                            this.copy_as(index, cx);
                        })),
                    );
                }
                popup(position, dismiss, list).into_any_element()
            }
            Menu::NetconfOperation(position) => {
                let p = &theme.palette;
                let current = self.store.read(|state| match &state.draft {
                    ManagementDraft::Netconf(draft) => Some(draft.operation),
                    ManagementDraft::Gnmi(_) => None,
                });
                let controller = self.controller.clone();
                let mut list = paper(&theme)
                    .max_h(px(520.))
                    .id("netconf-operations")
                    .overflow_y_scroll();
                for (group, operations) in NETCONF_OPERATION_GROUPS {
                    list = list.child(
                        div()
                            .px(px(16.))
                            .h(px(28.))
                            .flex()
                            .items_center()
                            .bg(p.background_paper)
                            .child(super::widgets::caps(*group, 11., 500., p.text.secondary)),
                    );
                    for choice in operations.iter() {
                        let missing = netconf_unsupported(choice.value, |fragment| {
                            controller.has_capability(fragment)
                        });
                        let primary = label(choice.label)
                            .family(MONO)
                            .size(12.5)
                            .line_height(theme.typography.body1.line_height)
                            .color(p.text.primary)
                            .no_truncate()
                            .into_any_element();
                        let secondary = match &missing {
                            Some(text) => (text.clone(), p.warning.main),
                            None => (choice.description.to_owned(), p.text.secondary),
                        };
                        let value = choice.value;
                        let mut item = two_line_item(
                            format!("op-{}", choice.value.as_str()),
                            None,
                            primary,
                            Some(secondary),
                            &theme,
                        )
                        .py(px(4.))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ui.menu = None;
                            this.update_netconf(|draft| draft.operation = value);
                            cx.notify();
                        }));
                        if current == Some(choice.value) {
                            item = item.bg(alpha(p.primary.main, p.action.selected_opacity));
                        }
                        list = list.child(item);
                    }
                }
                popup(position, dismiss, list).into_any_element()
            }
            Menu::Select {
                field,
                position,
                width,
            } => {
                let content = self.render_select_menu(&field, width, &theme, cx);
                popup(position, dismiss, content).into_any_element()
            }
        }
        .pipe_window(window)
    }

    fn render_select_menu(
        &mut self,
        field: &SelectField,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let options: Vec<(String, Option<String>)> = match field {
            SelectField::Encoding => {
                let offered = self
                    .controller
                    .gnmi_info()
                    .map(|info| info.encodings.clone())
                    .filter(|encodings| !encodings.is_empty())
                    .unwrap_or_else(|| {
                        vec![
                            GnmiEncoding::JsonIetf,
                            GnmiEncoding::Json,
                            GnmiEncoding::Proto,
                            GnmiEncoding::Ascii,
                        ]
                    });
                std::iter::once(("Session default".to_owned(), None))
                    .chain(offered.into_iter().map(|encoding| {
                        (
                            super::logic::device::encoding_name(encoding).to_uppercase(),
                            None,
                        )
                    }))
                    .collect()
            }
            SelectField::SetOp(_) => super::logic::editor::SET_OPS
                .iter()
                .map(|(_, text, detail)| ((*text).to_owned(), Some((*detail).to_owned())))
                .collect(),
            SelectField::SetEncoding(_) => ["JSON_IETF", "JSON", "ASCII"]
                .into_iter()
                .map(|text| (text.to_owned(), None))
                .collect(),
            SelectField::WithDefaults => [
                "Device default",
                "report-all: include defaults",
                "report-all-tagged",
                "trim: leave defaults out",
                "explicit: only what was set",
            ]
            .into_iter()
            .map(|text| (text.to_owned(), None))
            .collect(),
            SelectField::DefaultOperation => ["merge (default)", "merge", "replace", "none"]
                .into_iter()
                .map(|text| (text.to_owned(), None))
                .collect(),
            SelectField::TestOption => ["default", "test-then-set", "set", "test-only"]
                .into_iter()
                .map(|text| (text.to_owned(), None))
                .collect(),
            SelectField::ErrorOption => {
                let mut options = vec![
                    "stop-on-error (default)",
                    "stop-on-error",
                    "continue-on-error",
                ];
                if self.controller.has_capability(":rollback-on-error") {
                    options.push("rollback-on-error");
                }
                options
                    .into_iter()
                    .map(|text| (text.to_owned(), None))
                    .collect()
            }
            SelectField::Tool(_) | SelectField::ToolRow(..) => self.tool_select_options(field),
        };
        let mut list = paper(theme).min_w(px(width));
        for (index, (text, detail)) in options.into_iter().enumerate() {
            let field = field.clone();
            let primary = label(text.clone())
                .size(theme.typography.body1.size)
                .line_height(theme.typography.body1.line_height)
                .color(theme.palette.text.primary)
                .no_truncate()
                .into_any_element();
            let item = match detail {
                Some(detail) => two_line_item(
                    format!("option-{index}"),
                    None,
                    primary,
                    Some((detail, theme.palette.text.secondary)),
                    theme,
                )
                .into_any_element(),
                None => MenuItem::new(("option", index), text).into_any_element(),
            };
            list = list.child(
                div()
                    .id(("select-option", index))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.ui.menu = None;
                            this.choose_select(&field, index, cx);
                            cx.notify();
                        }),
                    )
                    .child(item),
            );
        }
        list
    }

    fn render_node_menu(
        &mut self,
        source: &TreeSource,
        node_id: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let p = &theme.palette;
        let gnmi = self.is_gnmi();
        let connected = self.connected();
        let node: Option<DataNode> = self
            .with_tree(source, |root| find_node(root, node_id).cloned())
            .flatten();
        let Some(node) = node else {
            return paper(theme);
        };
        let leaf = node.is_leaf();
        let title = {
            let name = muxus_core::gnmi_path::strip_module_prefix(&node.name);
            if name.is_empty() {
                "/".to_owned()
            } else {
                name.to_owned()
            }
        };
        let action = |this: &mut Self, which: NodeAction, cx: &mut Context<Self>| {
            this.ui.menu = None;
            this.node_action(which, cx);
            cx.notify();
        };
        let id = node.id.clone();
        let source = source.clone();
        let item = |id_name: &'static str,
                    text: &str,
                    icon_name: &'static str,
                    which: NodeAction,
                    disabled: bool,
                    cx: &mut Context<Self>| {
            let source = source.clone();
            let node_id = id.clone();
            MenuItem::new(id_name, text.to_owned())
                .icon(icon_name)
                .dense()
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    action(
                        this,
                        NodeAction {
                            kind: which.kind,
                            source: source.clone(),
                            node_id: node_id.clone(),
                        },
                        cx,
                    )
                }))
        };
        let blank = NodeAction {
            kind: NodeActionKind::Get,
            source: TreeSource::Explorer,
            node_id: String::new(),
        };
        let with = |kind: NodeActionKind| NodeAction {
            kind,
            ..blank.clone()
        };
        let mut menu = paper(theme).min_w(px(240.)).child(
            div()
                .px(px(16.))
                .h(px(2. * 4. + theme.typography.body2.line_height))
                .flex()
                .items_center()
                .child(
                    label(title)
                        .family(MONO)
                        .size(12.)
                        .line_height(theme.typography.body2.line_height)
                        .color(p.text.primary)
                        .no_truncate(),
                ),
        );
        menu = menu.child(item(
            "node-get",
            if gnmi { "Get" } else { "Get with state" },
            "PlayArrowRounded",
            with(NodeActionKind::Get),
            !connected,
            cx,
        ));
        if !gnmi {
            menu = menu.child(item(
                "node-get-config",
                "Get configuration",
                "SettingsOutlined",
                with(NodeActionKind::GetConfig),
                !connected,
                cx,
            ));
        }
        if gnmi {
            let watch_source = source.clone();
            let watch_id = id.clone();
            let primary = label("Watch")
                .size(theme.typography.body2.size)
                .line_height(theme.typography.body2.line_height)
                .color(p.text.primary)
                .no_truncate()
                .into_any_element();
            let mut watch = two_line_item(
                "node-watch",
                Some("MonitorHeartOutlined"),
                primary,
                Some(("Subscribe and stream updates".into(), p.text.secondary)),
                theme,
            )
            .py(px(4.));
            if connected {
                watch = watch.on_click(cx.listener(move |this, _, _, cx| {
                    this.ui.menu = None;
                    this.node_action(
                        NodeAction {
                            kind: NodeActionKind::Watch,
                            source: watch_source.clone(),
                            node_id: watch_id.clone(),
                        },
                        cx,
                    );
                    cx.notify();
                }));
            } else {
                watch = watch.opacity(0.38);
            }
            menu = menu.child(watch);
        }
        menu = menu.child(menu_divider(theme));
        menu = if gnmi {
            menu.child(item(
                "node-set",
                if leaf {
                    "Set value…"
                } else {
                    "Edit as Set…"
                },
                "EditOutlined",
                with(NodeActionKind::SetValue),
                false,
                cx,
            ))
        } else {
            menu.child(item(
                "node-edit",
                "Edit configuration…",
                "EditOutlined",
                with(NodeActionKind::EditConfig),
                !connected,
                cx,
            ))
            .child(item(
                "node-filter",
                "Use as filter",
                "FilterAltOutlined",
                with(NodeActionKind::UseAsFilter),
                false,
                cx,
            ))
        };
        menu = menu.child(
            item(
                "node-delete",
                "Delete…",
                "DeleteOutlined",
                with(NodeActionKind::Remove),
                false,
                cx,
            )
            .icon_color(p.error.main),
        );
        menu = menu.child(menu_divider(theme)).child(item(
            "node-copy-path",
            if gnmi { "Copy path" } else { "Copy XPath" },
            "ContentCopy",
            with(NodeActionKind::CopyPath),
            false,
            cx,
        ));
        if !gnmi {
            menu = menu.child(item(
                "node-copy-filter",
                "Copy subtree filter",
                "ContentCopy",
                with(NodeActionKind::CopyFilter),
                false,
                cx,
            ));
        }
        menu = menu.child(item(
            "node-copy-value",
            if leaf { "Copy value" } else { "Copy as JSON" },
            "ContentCopy",
            with(NodeActionKind::CopyValue),
            false,
            cx,
        ));
        if gnmi {
            menu = menu.child(item(
                "node-copy-gnmic",
                "Copy gnmic get",
                "ContentCopy",
                with(NodeActionKind::CopyGnmic),
                false,
                cx,
            ));
        }
        let _ = DataNodeKind::Leaf;
        menu
    }

    fn render_dialog(
        &mut self,
        dialog: Dialog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match dialog {
            Dialog::Review {
                title,
                description,
                sections,
                loading,
                error,
                apply,
                tab,
            } => self.render_review(
                title,
                description,
                sections,
                loading,
                error,
                apply,
                tab,
                window,
                cx,
            ),
            Dialog::Save { host_only, saving } => self.render_save(host_only, saving, window, cx),
            Dialog::Confirm {
                confirmation,
                action,
            } => self.render_confirm(confirmation, action, window, cx),
            Dialog::Prompt {
                title,
                description,
                label: field_label,
                confirm_label,
                action,
                error,
            } => self.render_prompt(
                title,
                description,
                field_label,
                confirm_label,
                action,
                error,
                window,
                cx,
            ),
            Dialog::TypedConfirm {
                title,
                description,
                expected,
                confirm_label,
                action: _,
            } => self.render_typed_confirm(title, description, expected, confirm_label, window, cx),
            Dialog::FilePreview {
                path,
                text,
                loading,
                error,
            } => self.render_file_preview(path, text, loading, error, window, cx),
        }
    }

    /// `TypedConfirmDialog`: the device name must be typed before the button works.
    fn render_typed_confirm(
        &mut self,
        title: String,
        description: String,
        expected: String,
        confirm_label: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        let key = FieldKey::Tool("confirm.typed");
        let typed = self.ui.tools.local.typed.clone();
        let state = self.bound_input(key.clone(), "", &typed, window, cx);
        if self.ui.prompt_initial.take().is_some() {
            self.focus_input(&key, window, cx);
        }
        let matches = super::tools::kit::typed_matches(&typed, &expected);
        let content = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .pt(px(4.))
            .child(
                paragraph(description)
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.primary),
            )
            .child(
                FormField::input("typed-confirm", &state)
                    .label(format!("Type {expected} to confirm"))
                    .flexible()
                    .notch(p.background_paper),
            );
        let actions = vec![
            Button::new("typed-cancel", "Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ui.dialog = None;
                    cx.notify();
                }))
                .into_any_element(),
            Button::new("typed-ok", confirm_label)
                .contained()
                .color(ButtonColor::Error)
                .disabled(!matches)
                .on_click(cx.listener(|this, _, window, cx| this.typed_confirmed(window, cx)))
                .into_any_element(),
        ];
        let dialog = dialog(title, content, actions, Some((24., 16.)), &theme, window);
        let close = std::rc::Rc::new(cx.closer_for_app(
            |this: &mut Self, cx: &mut Context<Self>| {
                this.ui.dialog = None;
                cx.notify();
            },
        ));
        window_modal(
            div()
                .id("typed-confirm-dialog")
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.ui.dialog = None;
                        cx.notify();
                    }
                }))
                .child(dialog),
            close,
            window,
        )
        .into_any_element()
    }

    /// The Files tool's preview: the file read-only, with Download.
    fn render_file_preview(
        &mut self,
        path: String,
        text: Option<String>,
        loading: bool,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let body = self.file_preview_body(
            &path,
            text.as_deref(),
            loading,
            error.as_deref(),
            window,
            cx,
        );
        let download_path = path.clone();
        let height = 760f32.min(muxus_ui::zoom::unzoom(window.viewport_size().height) * 0.88);
        let paper = muxus_ui::widgets::dialog::dialog_paper(&theme)
            .id("file-preview")
            .w(px(dialog_width(DialogWidth::Lg, window)))
            .h(px(height))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.ui.dialog = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .px(px(24.))
                    .py(px(16.))
                    .font_family(MONO)
                    .text_size(px(15.))
                    .font_weight(FontWeight(theme.typography.h6.weight))
                    .line_height(px(15. * 1.6))
                    .child(path),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .px(px(24.))
                    .pb(px(20.))
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(24.))
                    .pt(px(8.))
                    .pb(px(16.))
                    .child(Button::new("preview-close", "Close").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.ui.dialog = None;
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("preview-download", "Download")
                            .contained()
                            .start_icon("FileDownloadOutlined")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.download_previewed(download_path.clone(), cx)
                            })),
                    ),
            );
        let close = std::rc::Rc::new(cx.closer_for_app(
            |this: &mut Self, cx: &mut Context<Self>| {
                this.ui.dialog = None;
                cx.notify();
            },
        ));
        window_modal(paper, close, window).into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_review(
        &mut self,
        title: String,
        description: Option<String>,
        sections: Vec<super::ui::ReviewSection>,
        loading: bool,
        error: Option<String>,
        apply: Option<(ReviewApply, String)>,
        tab: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        let h6 = theme.typography.h6;
        let active = sections
            .get(tab.min(sections.len().saturating_sub(1)))
            .cloned();
        let unchanged = !loading
            && !sections.is_empty()
            && sections.iter().all(|section| section.delta == (0, 0));
        let mut content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(10.))
            .px(px(24.))
            .pb(px(20.));
        if let Some(description) = description {
            content = content.child(
                paragraph(description)
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(p.text.secondary),
            );
        }
        if let Some(error) = &error {
            content = content.child(Alert::new(Status::Error, error.clone()));
        }
        if loading {
            content = content.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .child(CircularProgress::new(
                        ("overlays-progress", 1usize),
                        28.,
                        p.primary.main,
                    ))
                    .child(
                        label("Reading the current configuration …")
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.secondary)
                            .no_truncate(),
                    ),
            );
        } else if !sections.is_empty() {
            if sections.len() > 1 {
                let mut tabs = div()
                    .flex()
                    .min_h(px(36.))
                    .border_b_1()
                    .border_color(p.divider);
                for (index, section) in sections.iter().enumerate() {
                    let selected = index == tab.min(sections.len() - 1);
                    let mut button = div()
                        .id(("review-tab", index))
                        .relative()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .px(px(16.))
                        .cursor_pointer()
                        .child(
                            label(section.label.clone())
                                .family(MONO)
                                .size(12.)
                                .line_height(18.)
                                .color(if selected {
                                    p.text.primary
                                } else {
                                    p.text.secondary
                                })
                                .no_truncate(),
                        )
                        .child(delta_chips(section.delta, &theme))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(Dialog::Review { tab, .. }) = &mut this.ui.dialog {
                                *tab = index;
                            }
                            cx.notify();
                        }));
                    if selected {
                        button = button.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(2.))
                                .rounded_t(px(2.))
                                .bg(p.text.primary),
                        );
                    }
                    tabs = tabs.child(button);
                }
                content = content.child(tabs);
            }
            if let Some(active) = &active {
                let mut row = div().flex().items_center().gap(px(8.));
                if sections.len() == 1 {
                    row = row
                        .child(
                            label(active.label.clone())
                                .family(MONO)
                                .size(12.5)
                                .line_height(body2.line_height)
                                .no_truncate(),
                        )
                        .child(delta_chips(active.delta, &theme));
                }
                if let Some(note) = &active.note {
                    row = row.child(
                        label(note.clone())
                            .size(theme.typography.caption.size)
                            .line_height(theme.typography.caption.line_height)
                            .color(p.text.secondary)
                            .no_truncate(),
                    );
                }
                content = content.child(row);
            }
            if unchanged {
                content = content.child(Alert::new(
                    Status::Info,
                    "Nothing changes: the device already has this configuration.",
                ));
            }
            if let Some(active) = &active {
                content = content.child(
                    div()
                        .flex_1()
                        .min_h(px(200.))
                        .flex()
                        .child(diff_view(&active.rows, &theme)),
                );
            }
        }
        let close = cx.closer_for_app(|this: &mut Self, cx: &mut Context<Self>| {
            this.ui.dialog = None;
            cx.notify();
        });
        let mut actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(24.))
            .pb(px(16.))
            .pt(px(8.))
            .child(div().flex_1())
            .child(
                Button::new(
                    "review-close",
                    if apply.is_some() { "Cancel" } else { "Close" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ui.dialog = None;
                    cx.notify();
                })),
            );
        if let Some((what, text)) = apply {
            actions = actions.child(
                Button::new("review-apply", text)
                    .contained()
                    .disabled(loading || error.is_some())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.apply_review(what, window, cx)),
                    ),
            );
        }
        let height = 780f32.min(muxus_ui::zoom::unzoom(window.viewport_size().height) * 0.9);
        let surface = dialog_paper(&theme)
            .id("review-dialog")
            .w(px(dialog_width(DialogWidth::Lg, window)))
            .h(px(height))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let modifier =
                    event.keystroke.modifiers.control || event.keystroke.modifiers.platform;
                if event.keystroke.key == "enter" && modifier {
                    if let Some(Dialog::Review {
                        apply: Some((what, _)),
                        loading: false,
                        error: None,
                        ..
                    }) = this.ui.dialog.clone()
                    {
                        this.apply_review(what, window, cx);
                    }
                } else if event.keystroke.key == "escape" {
                    this.ui.dialog = None;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .px(px(24.))
                    .pt(px(16.))
                    .pb(px(8.))
                    .text_size(px(h6.size))
                    .font_weight(FontWeight(h6.weight))
                    .line_height(px(h6.line_height))
                    .child(title),
            )
            .child(content)
            .child(actions);
        window_modal(surface, std::rc::Rc::new(close), window).into_any_element()
    }

    fn render_save(
        &mut self,
        host_only: bool,
        saving: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        let gnmi = self.is_gnmi();
        let has_profile = self.profile_id().is_some();
        let name = self.input(
            FieldKey::SaveName,
            if gnmi {
                "Interface counters"
            } else {
                "BGP configuration"
            },
            window,
            cx,
        );
        let empty = self
            .input_value(&FieldKey::SaveName, cx)
            .is_none_or(|value| value.trim().is_empty());
        let protocol = if gnmi { "gNMI" } else { "NETCONF" };
        let radio = |id: &'static str,
                     checked: bool,
                     disabled: bool,
                     text: AnyElement,
                     host: bool,
                     cx: &mut Context<Self>| {
            FormControlLabel::new(id, FormControl::Radio(checked), "")
                .small()
                .disabled(disabled)
                .label_element(text)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(Dialog::Save { host_only, .. }) = &mut this.ui.dialog {
                        *host_only = host;
                    }
                    cx.notify();
                }))
        };
        let mut host_label = div().flex().items_baseline().child(
            label(format!("Only for {}", self.title))
                .size(body2.size)
                .line_height(body2.line_height)
                .color(if has_profile {
                    p.text.primary
                } else {
                    p.text.disabled
                })
                .no_truncate(),
        );
        if !has_profile {
            host_label = host_label.child(
                label(" (save the host first)")
                    .size(theme.typography.caption.size)
                    .line_height(theme.typography.caption.line_height)
                    .color(p.text.secondary)
                    .no_truncate(),
            );
        }
        let content = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .pt(px(4.))
            .child(
                FormField::input("save-name", &name)
                    .placeholder(if gnmi {
                        "Interface counters"
                    } else {
                        "BGP configuration"
                    })
                    .label("Name")
                    .flexible()
                    .notch(p.background_paper),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(radio(
                        "save-host",
                        host_only && has_profile,
                        !has_profile,
                        host_label.into_any_element(),
                        true,
                        cx,
                    ))
                    .child(radio(
                        "save-all",
                        !host_only || !has_profile,
                        false,
                        label(format!("For every {protocol} host"))
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(p.text.primary)
                            .no_truncate()
                            .into_any_element(),
                        false,
                        cx,
                    )),
            );
        let actions = vec![
            Button::new("save-cancel", "Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ui.dialog = None;
                    cx.notify();
                }))
                .into_any_element(),
            Button::new("save-confirm", "Save")
                .contained()
                .disabled(empty || saving)
                .on_click(cx.listener(|this, _, _, cx| this.save_request(cx)))
                .into_any_element(),
        ];
        let dialog = dialog(
            "Save request",
            content,
            actions,
            Some((24., 16.)),
            &theme,
            window,
        );
        let close = std::rc::Rc::new(cx.closer_for_app(
            |this: &mut Self, cx: &mut Context<Self>| {
                this.ui.dialog = None;
                cx.notify();
            },
        ));
        window_modal(
            div()
                .id("save-dialog")
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            this.ui.dialog = None;
                            cx.notify();
                        }
                        "enter" => this.save_request(cx),
                        _ => {}
                    }
                }))
                .child(dialog),
            close,
            window,
        )
        .into_any_element()
    }

    fn render_confirm(
        &mut self,
        confirmation: super::logic::editor::Confirmation,
        action: ConfirmAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let body2 = theme.typography.body2;
        let confirm_action = action.clone();
        let key_action = action;
        let mut actions = vec![
            Button::new("confirm-cancel", "Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ui.dialog = None;
                    cx.notify();
                }))
                .into_any_element(),
        ];
        actions.push(
            Button::new("confirm-ok", confirmation.confirm_label.clone())
                .contained()
                .color(if confirmation.destructive {
                    ButtonColor::Error
                } else {
                    ButtonColor::Primary
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.ui.dialog = None;
                    this.confirmed(confirm_action.clone(), window, cx);
                }))
                .into_any_element(),
        );
        let content = if confirmation.description.is_empty() {
            div()
        } else {
            div().child(
                paragraph(confirmation.description.clone())
                    .size(body2.size)
                    .line_height(body2.line_height)
                    .color(theme.palette.text.secondary),
            )
        };
        let dialog = dialog(
            confirmation.title.clone(),
            content,
            actions,
            None,
            &theme,
            window,
        );
        let close = std::rc::Rc::new(cx.closer_for_app(
            |this: &mut Self, cx: &mut Context<Self>| {
                this.ui.dialog = None;
                cx.notify();
            },
        ));
        window_modal(
            div()
                .id("confirm-dialog")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            this.ui.dialog = None;
                            cx.notify();
                        }
                        "enter" => {
                            this.ui.dialog = None;
                            this.confirmed(key_action.clone(), window, cx);
                        }
                        _ => {}
                    }
                }))
                .child(dialog),
            close,
            window,
        )
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_prompt(
        &mut self,
        title: String,
        description: Option<String>,
        field_label: String,
        confirm_label: String,
        action: PromptAction,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let body2 = theme.typography.body2;
        let state = self.input(FieldKey::PromptText, "", window, cx);
        if let Some(initial) = self.ui.prompt_initial.take() {
            state.update(cx, |state, cx| state.set_value(initial, window, cx));
            self.focus_input(&FieldKey::PromptText, window, cx);
        }
        let mut content = div().flex().flex_col();
        if let Some(description) = description {
            content = content.child(
                div().mb(px(12.)).child(
                    paragraph(description)
                        .size(body2.size)
                        .line_height(body2.line_height)
                        .color(p.text.secondary),
                ),
            );
        }
        content = content
            .child(
                div().mt(px(4.)).child(
                    FormField::input("prompt-field", &state)
                        .label(field_label)
                        .flexible()
                        .error(error.is_some())
                        .notch(p.background_paper),
                ),
            )
            .child(
                div()
                    .mt(px(3.))
                    .mx(px(14.))
                    .h(px(theme.typography.caption.line_height))
                    .child(
                        label(error.clone().unwrap_or_default())
                            .size(theme.typography.caption.size)
                            .line_height(theme.typography.caption.line_height)
                            .color(p.error.main),
                    ),
            );
        let submit_action = action.clone();
        let key_action = action;
        let actions = vec![
            Button::new("prompt-cancel", "Cancel")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.ui.dialog = None;
                    cx.notify();
                }))
                .into_any_element(),
            Button::new("prompt-ok", confirm_label)
                .contained()
                .disabled(error.is_some())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.submit_prompt(submit_action.clone(), window, cx)
                }))
                .into_any_element(),
        ];
        let dialog = dialog(title, content, actions, None, &theme, window);
        let close = std::rc::Rc::new(cx.closer_for_app(
            |this: &mut Self, cx: &mut Context<Self>| {
                this.ui.dialog = None;
                cx.notify();
            },
        ));
        window_modal(
            div()
                .id("prompt-dialog")
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            this.ui.dialog = None;
                            cx.notify();
                        }
                        "enter" => this.submit_prompt(key_action.clone(), window, cx),
                        _ => {}
                    }
                }))
                .child(dialog),
            close,
            window,
        )
        .into_any_element()
    }
}

/// `DeltaChips`: lines added and removed.
fn delta_chips(delta: (usize, usize), theme: &Theme) -> impl IntoElement {
    let p = &theme.palette;
    div()
        .flex()
        .gap(px(6.))
        .child(
            Chip::new(format!("+{}", delta.0))
                .outlined()
                .color(p.success.main)
                .height(18.)
                .font_size(10.5)
                .label_padding(4.8),
        )
        .child(
            Chip::new(format!("−{}", delta.1))
                .outlined()
                .color(p.error.main)
                .height(18.)
                .font_size(10.5)
                .label_padding(4.8),
        )
}

/// One side of a diff row: gutter and text, tinted when changed.
fn diff_cell(
    side: Option<&DiffSide>,
    same: Option<(usize, &str)>,
    added: bool,
    theme: &Theme,
) -> Div {
    let p = &theme.palette;
    let dark = theme.mode.is_dark();
    let line = super::editor::CODE_LINE;
    let colors = diff_colors(dark);
    let row_tint = if added {
        colors.inserted_line
    } else {
        colors.removed_line
    };
    let mark_tint = if added {
        colors.inserted_text
    } else {
        colors.removed_text
    };
    let gutter = |number: Option<usize>| {
        div()
            .w(px(44.))
            .flex_none()
            .pr(px(10.))
            .flex()
            .justify_end()
            .text_color(colors.line_number)
            .child(number.map(|number| number.to_string()).unwrap_or_default())
    };
    let cell = div()
        .flex()
        .h(px(line))
        .flex_1()
        .min_w_0()
        .overflow_hidden();
    match (side, same) {
        (_, Some((number, text))) => cell.child(gutter(Some(number))).child(
            div()
                .whitespace_nowrap()
                .child(SharedString::from(text.to_owned())),
        ),
        (Some(side), None) => {
            let styled = muxus_ui::gpui_kit::StyledText::new(SharedString::from(side.text.clone()))
                .with_highlights(side.marks.iter().map(|range| {
                    (
                        range.clone(),
                        muxus_ui::gpui_kit::HighlightStyle {
                            background_color: Some(mark_tint),
                            ..Default::default()
                        },
                    )
                }));
            cell.bg(row_tint)
                .child(gutter(Some(side.number)))
                .child(div().whitespace_nowrap().child(styled))
        }
        (None, None) => cell.bg(colors.filler),
    }
    .text_color(p.text.primary)
}

/// The side-by-side diff (`DiffViewer`).
fn diff_view(rows: &[DiffRow], theme: &Theme) -> impl IntoElement {
    let p = &theme.palette;
    let dark = theme.mode.is_dark();
    let mut left = div().flex().flex_col();
    let mut right = div().flex().flex_col();
    for row in rows {
        match row {
            DiffRow::Same {
                left: l,
                right: r,
                text,
            } => {
                left = left.child(diff_cell(None, Some((*l, text)), false, theme));
                right = right.child(diff_cell(None, Some((*r, text)), true, theme));
            }
            DiffRow::Changed { left: l, right: r } => {
                left = left.child(diff_cell(l.as_ref(), None, false, theme));
                right = right.child(diff_cell(r.as_ref(), None, true, theme));
            }
            DiffRow::Hidden { count } => {
                let bar = |count: usize| {
                    div()
                        .h(px(super::editor::CODE_LINE + 2.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(diff_colors(dark).hidden_lines)
                        .text_color(p.text.secondary)
                        .font_family(muxus_ui::theme::fonts::UI)
                        .text_size(px(12.))
                        .child(format!("{count} hidden lines"))
                };
                left = left.child(bar(*count));
                right = right.child(bar(*count));
            }
        }
    }
    div()
        .id("diff")
        .flex_1()
        .min_w_0()
        .border_1()
        .border_color(p.divider)
        .rounded(px(theme.radius))
        .overflow_hidden()
        .bg(theme.components.code_surface)
        .font_family(MONO)
        .text_size(px(12.5))
        .line_height(px(super::editor::CODE_LINE))
        .py(px(6.))
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .child(left.flex_1().min_w_0().border_r_1().border_color(p.divider))
                .child(right.flex_1().min_w_0()),
        )
}

/// Which node action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeActionKind {
    Get,
    GetConfig,
    Watch,
    SetValue,
    Remove,
    EditConfig,
    UseAsFilter,
    CopyPath,
    CopyFilter,
    CopyValue,
    CopyGnmic,
}

/// A node action on a node of one of the trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeAction {
    pub kind: NodeActionKind,
    pub source: TreeSource,
    pub node_id: String,
}

/// `.pipe_window` keeps the match arms' types uniform.
trait PipeWindow: Sized {
    fn pipe_window(self, _window: &mut Window) -> Self {
        self
    }
}

impl PipeWindow for AnyElement {}

#[allow(dead_code)]
fn unused(_: GnmiSetItemOp, _: NetconfOperation) {}

/// The width a `fullWidth` dialog of `max` takes in this window (32px margins).
fn dialog_width(max: DialogWidth, window: &Window) -> f32 {
    (muxus_ui::zoom::unzoom(window.viewport_size().width) - 64.).min(max.px())
}

/// An `xs` `fullWidth` dialog: title, content, actions (`actions_padding`
/// is DialogActions' `px`/`pb` override, `None` for MUI's 8px).
fn dialog(
    title: impl Into<SharedString>,
    content: impl IntoElement,
    actions: Vec<AnyElement>,
    actions_padding: Option<(f32, f32)>,
    theme: &Theme,
    window: &Window,
) -> Div {
    let mut row = dialog_actions();
    if let Some((x, bottom)) = actions_padding {
        row = row.px(px(x)).pb(px(bottom));
    }
    dialog_paper(theme)
        .w(px(dialog_width(DialogWidth::Xs, window)))
        .child(dialog_title(theme).child(title.into()))
        .child(dialog_content(theme, true, false).child(content))
        .child(row.children(actions))
}

/// A dialog over the whole window, wherever the workbench sits in it.
fn window_modal(content: impl IntoElement, close: CloseHandler, window: &Window) -> AnyElement {
    modal_layer(window, "workbench-dialog", content, Some(close))
}
