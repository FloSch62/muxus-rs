//! The request editor (`RequestEditor.tsx`, `GnmiForm.tsx`,
//! `NetconfForm.tsx`, `PathField.tsx`): the operation, its fields, and the
//! run button.

use super::navigator::ListenerIndex;
use super::workbench::ListenerForApp;
use muxus_core::management::{
    GnmiDataType, GnmiSetEncoding, GnmiSubscriptionListMode, GnmiSubscriptionMode,
};
use muxus_mgmt::workbench::data_tree::DataNodeKind;
use muxus_mgmt::workbench::requests::{
    Datastore, FilterType, GnmiDraft, GnmiOperation, GnmiSetItemOp, ManagementDraft, NetconfDraft,
    NetconfOperation, gnmi_draft_problem, netconf_draft_problem, netconf_operation_label,
    netconf_rpc_body,
};
use muxus_mgmt::workbench::store::RunStatus;
use muxus_mgmt::workbench::xml::{pretty_xml, rpc_envelope, xml_fragment_error};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, BoxShadow, Context, Div, InteractiveElement, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point, px,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, fonts};
use muxus_ui::widgets::tooltip::bounds_probe;
use muxus_ui::widgets::{Button, IconButton};

use super::fields::{EditorKey, FieldKey};
use super::logic::device::encoding_name;
use super::logic::editor::{
    datastores, netconf_caption, run_icon, run_label, run_tooltip, set_caption, set_op_label,
    target_options,
};
use super::logic::path_field::{suggestions_for, syntax_error};
use super::tree_view::kind_icon;
use super::ui::{Menu, SelectField};
use super::view::ManagementView;
use super::widgets::{MONO, field_label, switch_row, switch_row_flush, text_tint};
use muxus_ui::widgets::form_field::end_adornment;
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::search_field::input_base;
use muxus_ui::widgets::toggle_button::ToggleButtonGroup;
use muxus_ui::widgets::toggle_button::ToggleOption;

/// Line height of the code editors (Monaco's `lineHeight: 19`).
pub const CODE_LINE: f32 = 19.;

fn toggle(labels: &[&str]) -> Vec<ToggleOption> {
    labels.iter().map(|text| ToggleOption::new(*text)).collect()
}

impl ManagementView {
    pub(super) fn render_request_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> muxus_ui::gpui_kit::Stateful<Div> {
        let theme = *cx.theme();
        let p = &theme.palette;
        let connected = self.connected();
        let (draft, streaming, running) = self.store.read(|state| {
            let selected = state
                .runs
                .iter()
                .find(|run| Some(&run.id) == state.selected_run_id.as_ref());
            let streaming = matches!(&state.draft, ManagementDraft::Gnmi(gnmi) if gnmi.operation == GnmiOperation::Subscribe)
                && selected.is_some_and(|run| run.status == RunStatus::Streaming);
            let running = state
                .runs
                .iter()
                .find(|run| run.status == RunStatus::Running && !run.hidden && run.live.is_none())
                .map(|run| run.id.clone());
            (state.draft.clone(), streaming, running)
        });
        let problem = match &draft {
            ManagementDraft::Gnmi(gnmi) => gnmi_draft_problem(gnmi),
            ManagementDraft::Netconf(netconf) => netconf_draft_problem(netconf),
        };
        let run_text = run_label(&draft, streaming);
        let operation: AnyElement = match &draft {
            ManagementDraft::Gnmi(gnmi) => ToggleButtonGroup::exclusive(
                "gnmi-operation",
                toggle(&["Get", "Set", "Subscribe"]),
                Some(match gnmi.operation {
                    GnmiOperation::Get => 0,
                    GnmiOperation::Set => 1,
                    GnmiOperation::Subscribe => 2,
                }),
                cx.listener_index(|this, index, cx| {
                    let operation = match index {
                        0 => GnmiOperation::Get,
                        1 => GnmiOperation::Set,
                        _ => GnmiOperation::Subscribe,
                    };
                    this.update_gnmi(|draft| draft.operation = operation);
                    cx.notify();
                }),
            )
            .inline(3.6, 12., 12.5)
            .into_any_element(),
            ManagementDraft::Netconf(netconf) => {
                self.render_operation_select(netconf, cx).into_any_element()
            }
        };
        let copy_anchor = self.ui.anchor("copy-as");
        let run_button: AnyElement = if let Some(id) = running {
            Button::new("cancel-run", "Cancel")
                .outlined()
                .color(muxus_ui::widgets::ButtonColor::Inherit)
                .min_width(104.)
                .tooltip("Cancel · Esc")
                .label_element(
                    div()
                        .flex()
                        .items_center()
                        .child(div().ml(px(-2.)).mr(px(8.)).child(CircularProgress::new(
                            ("editor-progress", 1usize),
                            14.,
                            p.primary.main,
                        )))
                        .child(
                            label("Cancel")
                                .size(theme.typography.px_to_rem(13.))
                                .weight(550.)
                                .line_height(theme.typography.px_to_rem(13.) * 1.75)
                                .color(p.text.primary)
                                .no_truncate(),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, _| this.controller.cancel(&id)))
                .into_any_element()
        } else {
            let color = if streaming {
                muxus_ui::widgets::ButtonColor::Error
            } else {
                muxus_ui::widgets::ButtonColor::Primary
            };
            Button::new("run", run_text)
                .contained()
                .color(color)
                .min_width(104.)
                .disabled(!connected)
                .start_icon(run_icon(&draft, streaming))
                .tooltip(run_tooltip(run_text, problem.as_deref(), connected))
                .on_click(cx.listener(|this, _, window, cx| this.run_draft(window, cx)))
                .into_any_element()
        };
        let toolbar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(6.))
            .min_h(px(46.))
            .border_b_1()
            .border_color(p.divider)
            .child(operation)
            .child(div().flex_1())
            .child(
                IconButton::new("copy-as", "ContentCopy")
                    .tooltip("Copy as …")
                    .track_bounds(&copy_anchor)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(position) = copy_anchor.get().map(|bounds| bounds.bottom_left())
                        {
                            this.ui.menu = Some(Menu::CopyAs(position));
                            cx.notify();
                        }
                    })),
            )
            .child(
                IconButton::new("save-request", "BookmarkAddOutlined")
                    .tooltip("Save to the library")
                    .on_click(cx.listener(|this, _, window, cx| this.open_save_dialog(window, cx))),
            )
            .child(run_button);
        let form: AnyElement = match draft {
            ManagementDraft::Gnmi(gnmi) => {
                self.render_gnmi_form(&gnmi, window, cx).into_any_element()
            }
            ManagementDraft::Netconf(netconf) => self
                .render_netconf_form(&netconf, window, cx)
                .into_any_element(),
        };
        let focus = self.editor_focus(cx);
        div()
            .id("request-editor")
            .track_focus(&focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(toolbar)
            .child(
                div()
                    .id("request-form")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(12.))
                    .py(px(10.))
                    .child(form),
            )
    }

    fn render_operation_select(
        &mut self,
        draft: &NetconfDraft,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = *cx.theme();
        let anchor = self.ui.anchor("netconf-operation");
        let probe = anchor.clone();
        let open = matches!(self.ui.menu, Some(Menu::NetconfOperation(_)));
        FormField::new(
            "netconf-operation",
            FieldBody::Element(
                label(netconf_operation_label(draft.operation))
                    .family(MONO)
                    .size(12.5)
                    .weight(600.)
                    .line_height(18.)
                    .color(theme.palette.text.primary)
                    .into_any_element(),
            ),
        )
        .select(open)
        .compact(18. + 2. * 4.8 + 2., 12.5)
        .width(190.)
        .anchor(&anchor)
        .on_click(
            cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                if let Some(bounds) = probe.get() {
                    this.ui.menu = Some(Menu::NetconfOperation(bounds.bottom_left()));
                    cx.notify();
                }
            }),
        )
    }

    // gNMI

    fn render_gnmi_form(
        &mut self,
        draft: &GnmiDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let mut form = div().flex().flex_col().gap(px(12.));
        if draft.operation == GnmiOperation::Set {
            form = form.child(self.render_set_items(draft, window, cx));
        } else {
            form = form.child(self.render_paths(draft, window, cx));
        }
        match draft.operation {
            GnmiOperation::Get => form = form.child(self.render_get_options(draft, window, cx)),
            GnmiOperation::Subscribe => {
                form = form.child(self.render_subscribe_options(draft, window, cx))
            }
            GnmiOperation::Set => form = form.child(self.render_set_confirm(draft, window, cx)),
        }
        let open = self.ui.advanced.unwrap_or(!draft.prefix.is_empty());
        let mut advanced = div().child(
            div().flex().child(
                Button::new("advanced", "Advanced")
                    .tint(text_tint(theme.palette.text.secondary, &theme))
                    .font_size(12.)
                    .padding_y(4.)
                    .padding_x(4.)
                    .end_icon("ExpandMore", 8.)
                    .end_icon_turned(open)
                    .on_click(cx.listener_for_app(
                        move |this: &mut Self, cx: &mut Context<Self>| {
                            this.ui.advanced = Some(!open);
                            cx.notify();
                        },
                    )),
            ),
        );
        if open {
            let prefix = self.bound_input(
                FieldKey::Prefix,
                "Common to every path, e.g. /network-instance[name=default]",
                &draft.prefix,
                window,
                cx,
            );
            let mut row = div().flex().items_center().gap(px(8.)).pt(px(8.)).child(
                FormField::input("prefix", &prefix)
                    .compact(30., 12.5)
                    .shrink(true)
                    .mono(12.5)
                    .placeholder("Common to every path, e.g. /network-instance[name=default]")
                    .label("Prefix")
                    .flexible()
                    .notch(theme.palette.background_default),
            );
            if draft.operation != GnmiOperation::Get {
                row = row.child(self.render_encoding_select(draft, cx));
            }
            advanced = advanced.child(row);
        }
        form.child(advanced)
    }

    fn render_paths(
        &mut self,
        draft: &GnmiDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let label_text = if draft.operation == GnmiOperation::Subscribe {
            "Subscribe to"
        } else {
            "Paths"
        };
        let mut list = div().flex().flex_col().gap(px(6.)).child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(field_label(label_text, &theme))
                .child(
                    Button::new("add-path", "Path")
                        .font_size(12.)
                        .padding_y(0.)
                        .padding_x(5.)
                        .start_icon("Add")
                        .start_icon_style(16., 10.)
                        .on_click(cx.listener_for_app(
                            |this: &mut Self, cx: &mut Context<Self>| {
                                this.update_gnmi(|draft| draft.paths.push(String::new()));
                                this.ui.focus_new_path = true;
                                cx.notify();
                            },
                        )),
                ),
        );
        let count = draft.paths.len();
        for (index, path) in draft.paths.iter().enumerate() {
            let mut row = div()
                .flex()
                .items_start()
                .gap(px(4.))
                .child(self.render_path_field(
                    FieldKey::Path(index),
                    path,
                    "/interface[name=ethernet-1/1]/statistics",
                    window,
                    cx,
                ));
            if count > 1 {
                row = row.child(
                    div().mt(px(2.)).child(
                        IconButton::new(("remove-path", index), "Close")
                            .icon_size(16.)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_gnmi(|draft| {
                                    if index < draft.paths.len() {
                                        draft.paths.remove(index);
                                    }
                                });
                                this.fields.retain(|key| !matches!(key, FieldKey::Path(_)));
                                cx.notify();
                            })),
                    ),
                );
            }
            list = list.child(row);
        }
        list
    }

    /// A path input that completes from the device's own data.
    pub(super) fn render_path_field(
        &mut self,
        key: FieldKey,
        value: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let state = self.bound_input(key.clone(), placeholder, value, window, cx);
        if self.ui.focus_new_path
            && key == FieldKey::Path(self.gnmi_path_count().saturating_sub(1))
            && value.is_empty()
        {
            self.ui.focus_new_path = false;
            self.focus_input(&key, window, cx);
        }
        let focused = self.input_focused(&key, window, cx);
        let error = syntax_error(value);
        let anchor = self.ui.anchor(format!("path-{key:?}"));
        let (suggestions, loading) =
            if focused && self.ui.suggest.open && self.ui.suggest.field.as_ref() == Some(&key) {
                self.store.read(|state| {
                    let found = suggestions_for(&state.explorer.root, value);
                    let loading = found
                        .parent
                        .as_ref()
                        .is_some_and(|parent| state.explorer.loading.contains(parent));
                    (found.suggestions, loading)
                })
            } else {
                (Vec::new(), false)
            };
        if focused && self.connected() {
            self.load_path_level(value);
        }
        let border = if error.is_some() {
            p.error.main
        } else if focused {
            p.primary.main
        } else {
            p.divider
        };
        let mut field = div()
            .relative()
            .flex()
            .items_center()
            .gap(px(4.))
            .h(px(32.))
            .px(px(8.))
            .border_1()
            .border_color(border)
            .rounded(px(theme.radius))
            .bg(p.background_paper)
            .child(input_base(&state, 12.5, true, cx))
            .child(bounds_probe(&anchor));
        if focused {
            let ring = alpha(
                if error.is_some() {
                    p.error.main
                } else {
                    p.primary.main
                },
                0.15,
            );
            field = field.shadow(vec![BoxShadow {
                color: ring,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
                inset: false,
            }]);
            if loading {
                field = field.child(CircularProgress::new(
                    ("editor-progress", 2usize),
                    12.,
                    p.primary.main,
                ));
            }
        }
        let mut column = div().flex_1().min_w_0().child(field);
        if let (Some(error), true) = (&error, focused) {
            let caption = theme.typography.caption;
            column = column.child(
                div()
                    .mt(px(2.))
                    .text_size(px(caption.size))
                    .line_height(px(caption.line_height))
                    .text_color(p.error.main)
                    .child(error.clone()),
            );
        }
        let show = focused && self.ui.suggest.open && (!suggestions.is_empty() || loading);
        if show && let Some(bounds) = anchor.get() {
            let highlighted = self
                .ui
                .suggest
                .highlighted
                .min(suggestions.len().saturating_sub(1));
            let mut list = div()
                .id("suggestions")
                .w(bounds.size.width)
                .mt(px(4.))
                .max_h(px(300.))
                .overflow_y_scroll()
                .py(px(4.))
                .bg(p.background_paper)
                .border_1()
                .border_color(p.divider)
                .rounded(px(theme.radius))
                .shadow(muxus_ui::widgets::button::elevation(8, &theme))
                .on_mouse_down(muxus_ui::gpui_kit::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                });
            if suggestions.is_empty() {
                let caption = theme.typography.caption;
                list = list.child(
                    div().px(px(12.)).py(px(6.)).child(
                        label("Asking the device …")
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary),
                    ),
                );
            }
            for (index, suggestion) in suggestions.into_iter().enumerate() {
                let hover_key = key.clone();
                let accept_key = key.clone();
                let accepted = suggestion.clone();
                let mut row = div()
                    .id(("suggestion", index))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .h(px(26.))
                    .cursor_pointer()
                    .text_size(px(12.5))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.ui.suggest.highlighted != index
                            && this.ui.suggest.field.as_ref() == Some(&hover_key)
                        {
                            this.ui.suggest.highlighted = index;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.accept_suggestion(&accept_key, &accepted, window, cx);
                    }))
                    .child(match suggestion.kind {
                        DataNodeKind::Leaf | DataNodeKind::LeafList => {
                            kind_icon(suggestion.kind, &theme)
                        }
                        DataNodeKind::Root => kind_icon(DataNodeKind::Container, &theme),
                        kind => kind_icon(kind, &theme),
                    })
                    .child(
                        label(suggestion.insert.clone())
                            .family(MONO)
                            .size(12.5)
                            .line_height(18.75)
                            .color(p.text.primary)
                            .no_truncate(),
                    );
                if index == highlighted {
                    row = row.bg(alpha(p.primary.main, 0.14));
                }
                if let Some(preview) = suggestion.preview {
                    row = row.child(
                        label(preview)
                            .family(MONO)
                            .size(12.5)
                            .line_height(18.75)
                            .color(p.text.secondary),
                    );
                }
                if let Some(entries) = suggestion.entries {
                    row = row.child(
                        label(format!("{entries} entries"))
                            .size(11.)
                            .line_height(18.)
                            .color(p.text.disabled)
                            .no_truncate(),
                    );
                }
                list = list.child(row);
            }
            column = column.child(
                deferred(
                    anchored()
                        .position(bounds.bottom_left())
                        .snap_to_window()
                        .child(list),
                )
                .with_priority(1),
            );
        }
        column
    }

    fn gnmi_path_count(&self) -> usize {
        self.store.read(|state| match &state.draft {
            ManagementDraft::Gnmi(draft) => draft.paths.len(),
            ManagementDraft::Netconf(_) => 0,
        })
    }

    fn render_get_options(
        &mut self,
        draft: &GnmiDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let depth_text = draft
            .depth
            .map(|depth| depth.to_string())
            .unwrap_or_default();
        let depth = self.bound_input(FieldKey::Depth, "all", &depth_text, window, cx);
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(
                ToggleButtonGroup::exclusive("data-type", toggle(&["All", "Config", "State", "Operational"]), Some(match draft.data_type {
                        GnmiDataType::All => 0,
                        GnmiDataType::Config => 1,
                        GnmiDataType::State => 2,
                        GnmiDataType::Operational => 3,
                    }), cx.listener_index(|this, index, cx| {
                    let data_type = [
                        GnmiDataType::All,
                        GnmiDataType::Config,
                        GnmiDataType::State,
                        GnmiDataType::Operational,
                    ][index];
                    this.update_gnmi(|draft| draft.data_type = data_type);
                    cx.notify();
                })).inline(3.2, 8.8, 12.),
            )
            .child(self.render_encoding_select(draft, cx))
            .child(
                div()
                    .id("depth")
                    .child(
                        FormField::input("depth-field", &depth).compact(30., 12.5).shrink(true).placeholder("all")
                            .label("Depth")
                            .width(84.)
                            .notch(theme.palette.background_default),
                    )
                    .tooltip(muxus_ui::widgets::Tooltip::text(
                        "Levels below each path to return (gNMI depth extension). Empty returns everything.",
                    )),
            )
    }

    /// `EncodingSelect`.
    fn render_encoding_select(&mut self, draft: &GnmiDraft, cx: &mut Context<Self>) -> AnyElement {
        let value = draft
            .encoding
            .map(|encoding| encoding_name(encoding).to_uppercase())
            .unwrap_or_else(|| "Session default".into());
        self.select_field(SelectField::Encoding, "Encoding", value, 150., false, cx)
    }

    /// A compact select (`TextField select` with `compactFieldSx`), opening its menu below.
    pub(super) fn select_field(
        &mut self,
        field: SelectField,
        label_text: &str,
        value: String,
        width: f32,
        mono: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let anchor = self.ui.anchor(format!("select-{field:?}"));
        let probe = anchor.clone();
        let open =
            matches!(&self.ui.menu, Some(Menu::Select { field: open, .. }) if *open == field);
        let mut select = FormField::new(
            SharedString::from(format!("select-{field:?}")),
            FieldBody::Element(
                label(value)
                    .family(if mono { MONO } else { fonts::UI })
                    .size(12.5)
                    .line_height(12.5 * 1.4375)
                    .color(theme.palette.text.primary)
                    .into_any_element(),
            ),
        )
        .select(open)
        .compact(30., 12.5)
        .shrink(true)
        .width(width)
        .notch(
            if matches!(field, SelectField::Tool(_) | SelectField::ToolRow(..)) {
                theme.palette.background_paper
            } else {
                theme.palette.background_default
            },
        )
        .anchor(&anchor)
        .on_click(
            cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                if let Some(bounds) = probe.get() {
                    this.ui.menu = Some(Menu::Select {
                        field: field.clone(),
                        position: bounds.bottom_left(),
                        width: muxus_ui::zoom::unzoom(bounds.size.width),
                    });
                    cx.notify();
                }
            }),
        );
        if !label_text.is_empty() {
            select = select.label(label_text.to_owned());
        }
        select.into_any_element()
    }

    fn render_subscribe_options(
        &mut self,
        draft: &GnmiDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let sub = &draft.subscribe;
        let mut first = div().flex().flex_wrap().items_center().gap(px(8.)).child(
            ToggleButtonGroup::exclusive(
                "list-mode",
                toggle(&["Stream", "Once", "Poll"]),
                Some(match sub.list_mode {
                    GnmiSubscriptionListMode::Stream => 0,
                    GnmiSubscriptionListMode::Once => 1,
                    GnmiSubscriptionListMode::Poll => 2,
                }),
                cx.listener_index(|this, index, cx| {
                    let mode = [
                        GnmiSubscriptionListMode::Stream,
                        GnmiSubscriptionListMode::Once,
                        GnmiSubscriptionListMode::Poll,
                    ][index];
                    this.update_gnmi(|draft| draft.subscribe.list_mode = mode);
                    cx.notify();
                }),
            )
            .inline(3.2, 8.8, 12.),
        );
        let stream = sub.list_mode == GnmiSubscriptionListMode::Stream;
        if stream {
            first = first.child(
                ToggleButtonGroup::exclusive(
                    "stream-mode",
                    toggle(&["Sample", "On change", "Target defined"]),
                    Some(match sub.mode {
                        GnmiSubscriptionMode::Sample => 0,
                        GnmiSubscriptionMode::OnChange => 1,
                        GnmiSubscriptionMode::TargetDefined => 2,
                    }),
                    cx.listener_index(|this, index, cx| {
                        let mode = [
                            GnmiSubscriptionMode::Sample,
                            GnmiSubscriptionMode::OnChange,
                            GnmiSubscriptionMode::TargetDefined,
                        ][index];
                        this.update_gnmi(|draft| draft.subscribe.mode = mode);
                        cx.notify();
                    }),
                )
                .inline(3.2, 8.8, 12.),
            );
            if sub.mode == GnmiSubscriptionMode::Sample {
                let text = super::logic::format::js_number_string(sub.sample_seconds);
                let input = self.bound_input(FieldKey::SampleSeconds, "", &text, window, cx);
                first = first.child(
                    FormField::input("sample", &input)
                        .compact(30., 12.5)
                        .shrink(true)
                        .label("Every")
                        .width(90.)
                        .notch(theme.palette.background_default)
                        .end(end_adornment("s", &theme)),
                );
            }
        }
        let mut column = div().flex().flex_col().gap(px(8.)).child(first);
        if stream {
            let mut second =
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(16.))
                    .child(switch_row(
                        "updates-only",
                        sub.updates_only,
                        "Changes only",
                        &theme,
                        cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                            this.update_gnmi(|draft| {
                                draft.subscribe.updates_only = !draft.subscribe.updates_only
                            });
                            cx.notify();
                        }),
                    ));
            if sub.mode == GnmiSubscriptionMode::Sample {
                second = second.child(switch_row(
                    "suppress",
                    sub.suppress_redundant,
                    "Suppress unchanged samples",
                    &theme,
                    cx.listener_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                        this.update_gnmi(|draft| {
                            draft.subscribe.suppress_redundant = !draft.subscribe.suppress_redundant
                        });
                        cx.notify();
                    }),
                ));
            }
            if sub.mode == GnmiSubscriptionMode::OnChange || sub.suppress_redundant {
                let text = sub
                    .heartbeat_seconds
                    .map(super::logic::format::js_number_string)
                    .unwrap_or_default();
                let input = self.bound_input(FieldKey::Heartbeat, "off", &text, window, cx);
                second = second.child(
                    FormField::input("heartbeat", &input)
                        .compact(30., 12.5)
                        .shrink(true)
                        .placeholder("off")
                        .label("Heartbeat")
                        .width(110.)
                        .notch(theme.palette.background_default)
                        .end(end_adornment("s", &theme)),
                );
            }
            column = column.child(second);
        }
        column
    }

    fn render_set_confirm(
        &mut self,
        draft: &GnmiDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let caption = theme.typography.caption;
        let confirm = draft.confirm_seconds.is_some_and(|seconds| seconds > 0);
        let text = draft.confirm_seconds.unwrap_or(120).to_string();
        let input = self.bound_input(FieldKey::ConfirmSeconds, "", &text, window, cx);
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(switch_row_flush(
                        "confirm-commit",
                        confirm,
                        "Roll back automatically unless confirmed within",
                        &theme,
                        cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                            this.update_gnmi(|draft| {
                                draft.confirm_seconds = if confirm { None } else { Some(120) }
                            });
                            cx.notify();
                        }),
                    ))
                    .child(
                        FormField::input("confirm-seconds", &input)
                            .compact(30., 12.5)
                            .shrink(true)
                            .disabled(!confirm)
                            .width(96.)
                            .disabled(!confirm)
                            .end(end_adornment("s", &theme)),
                    ),
            )
            .child(
                div()
                    .text_size(px(caption.size))
                    .line_height(px(caption.line_height))
                    .text_color(theme.palette.text.secondary)
                    .child(set_caption(confirm)),
            )
    }

    fn render_set_items(
        &mut self,
        draft: &GnmiDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let connected = self.connected();
        let caption = theme.typography.caption;
        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(field_label("Changes", &theme));
        let count = draft.set.len();
        for (index, item) in draft.set.iter().enumerate() {
            let id = item.id.clone();
            let op_value = set_op_label(item.op).to_owned();
            let mut head = div()
                .flex()
                .items_start()
                .gap(px(6.))
                .child(self.select_field(
                    SelectField::SetOp(id.clone()),
                    "",
                    op_value,
                    110.,
                    false,
                    cx,
                ))
                .child(self.render_path_field(
                    FieldKey::SetPath(id.clone()),
                    &item.path,
                    "/interface[name=ethernet-1/1]/description",
                    window,
                    cx,
                ));
            if item.op != GnmiSetItemOp::Delete {
                let load = item.clone();
                head = head.child(
                    div().mt(px(2.)).child(
                        IconButton::new(
                            SharedString::from(format!("load-{id}")),
                            "CloudDownloadOutlined",
                        )
                        .icon_size(17.)
                        .tooltip("Load the current value from the device")
                        .disabled(
                            !connected
                                || muxus_core::js::js_trim(&item.path).is_empty()
                                || self.ui.set_loading.as_deref() == Some(id.as_str()),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.load_current(load.clone(), cx)),
                        ),
                    ),
                );
            }
            if count > 1 {
                let remove = id.clone();
                head = head.child(
                    div().mt(px(2.)).child(
                        IconButton::new(SharedString::from(format!("remove-{id}")), "Close")
                            .icon_size(16.)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let remove = remove.clone();
                                this.update_gnmi(|draft| {
                                    draft.set.retain(|item| item.id != remove)
                                });
                                cx.notify();
                            })),
                    ),
                );
            }
            let mut card = div()
                .border_1()
                .border_color(p.divider)
                .rounded(px(theme.radius * 1.5))
                .p(px(8.))
                .bg(p.background_paper)
                .child(head);
            if item.op != GnmiSetItemOp::Delete {
                let ascii = item.encoding == GnmiSetEncoding::Ascii;
                let editor = self.code_editor(
                    EditorKey::SetValue(id.clone()),
                    if ascii { "text" } else { "json" },
                    &item.value,
                    false,
                    Some((3, 14)),
                    false,
                    if ascii {
                        "Text value"
                    } else {
                        "\"a JSON value\" — a string, number, or an object for a container"
                    },
                    window,
                    cx,
                );
                let encoding = match item.encoding {
                    GnmiSetEncoding::JsonIetf => "JSON_IETF",
                    GnmiSetEncoding::Json => "JSON",
                    GnmiSetEncoding::Ascii => "ASCII",
                };
                card = card.child(
                    div()
                        .mt(px(6.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(editor)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(self.select_field(
                                    SelectField::SetEncoding(id.clone()),
                                    "",
                                    encoding.into(),
                                    130.,
                                    false,
                                    cx,
                                ))
                                .child(
                                    label(if ascii {
                                        "Sent as text."
                                    } else {
                                        "Leaves take a JSON scalar, containers a JSON object."
                                    })
                                    .size(caption.size)
                                    .line_height(caption.line_height)
                                    .color(p.text.secondary),
                                ),
                        ),
                );
            }
            let _ = index;
            column = column.child(card);
        }
        let mut adds = div().flex().gap(px(4.));
        for (op, text, _) in super::logic::editor::SET_OPS {
            let op = *op;
            adds = adds.child(
                Button::new(SharedString::from(format!("add-{text}")), *text)
                    .start_icon("Add")
                    .start_icon_style(16., 10.)
                    .on_click(cx.listener_for_app(
                        move |this: &mut Self, cx: &mut Context<Self>| {
                            this.update_gnmi(|draft| {
                                draft
                                    .set
                                    .push(muxus_mgmt::workbench::requests::GnmiSetItem {
                                        id: muxus_mgmt::workbench::requests::new_item_id(),
                                        op,
                                        path: String::new(),
                                        value: String::new(),
                                        encoding: GnmiSetEncoding::JsonIetf,
                                    })
                            });
                            cx.notify();
                        },
                    )),
            );
        }
        column.child(adds)
    }

    // NETCONF

    fn render_netconf_form(
        &mut self,
        draft: &NetconfDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = &theme.palette;
        let caption = theme.typography.caption;
        let controller = self.controller.clone();
        let has = move |fragment: &str| controller.has_capability(fragment);
        let stores = datastores(&has);
        let op = draft.operation;
        let mut form = div().flex().flex_col().gap(px(10.));
        let datastore_toggle = |this: &mut Self,
                                id: &'static str,
                                label_text: &'static str,
                                value: Datastore,
                                options: Vec<Datastore>,
                                source: bool,
                                cx: &mut Context<Self>|
         -> AnyElement {
            let selected = options.iter().position(|option| *option == value);
            let labels: Vec<ToggleOption> = options
                .iter()
                .map(|option| ToggleOption::new(option.as_str()))
                .collect();
            let options_for_select = options.clone();
            let body2 = theme.typography.body2;
            let _ = this;
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div().min_w(px(52.)).child(
                        label(label_text)
                            .size(body2.size)
                            .line_height(body2.line_height)
                            .color(theme.palette.text.secondary)
                            .no_truncate(),
                    ),
                )
                .child(
                    ToggleButtonGroup::exclusive(
                        id,
                        labels,
                        selected,
                        cx.listener_index(move |this, index, cx| {
                            let store = options_for_select[index];
                            this.update_netconf(|draft| {
                                if source {
                                    draft.source = store;
                                } else {
                                    draft.target = store;
                                }
                            });
                            cx.notify();
                        }),
                    )
                    .inline(3.2, 8.8, 12.)
                    .mono(),
                )
                .into_any_element()
        };
        match op {
            NetconfOperation::GetConfig | NetconfOperation::Validate => {
                form = form.child(datastore_toggle(
                    self,
                    "source",
                    "Source",
                    draft.source,
                    stores.readable.clone(),
                    true,
                    cx,
                ));
            }
            NetconfOperation::EditConfig
            | NetconfOperation::Lock
            | NetconfOperation::Unlock
            | NetconfOperation::DeleteConfig => {
                form = form.child(datastore_toggle(
                    self,
                    "target",
                    "Target",
                    draft.target,
                    target_options(op, &stores),
                    false,
                    cx,
                ));
            }
            NetconfOperation::CopyConfig => {
                form = form.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(16.))
                        .child(datastore_toggle(
                            self,
                            "from",
                            "From",
                            draft.source,
                            stores.readable.clone(),
                            true,
                            cx,
                        ))
                        .child(datastore_toggle(
                            self,
                            "to",
                            "To",
                            draft.target,
                            stores.edit_targets.clone(),
                            false,
                            cx,
                        )),
                );
            }
            _ => {}
        }
        if matches!(
            op,
            NetconfOperation::Get
                | NetconfOperation::GetConfig
                | NetconfOperation::CreateSubscription
        ) {
            let mut types = vec![ToggleOption::new("Subtree")];
            let mut values = vec![FilterType::Subtree];
            if has(":xpath") && op != NetconfOperation::CreateSubscription {
                types.push(ToggleOption::new("XPath"));
                values.push(FilterType::Xpath);
            }
            types.push(ToggleOption::new("None"));
            values.push(FilterType::None);
            let selected = values.iter().position(|value| *value == draft.filter_type);
            let mut filter = div().flex().flex_col().gap(px(6.)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(field_label("Filter", &theme))
                    .child(div().flex_1())
                    .child(
                        ToggleButtonGroup::exclusive(
                            "filter-type",
                            types,
                            selected,
                            cx.listener_index(move |this, index, cx| {
                                let filter_type = values[index];
                                this.update_netconf(|draft| draft.filter_type = filter_type);
                                cx.notify();
                            }),
                        )
                        .inline(1.6, 8., 11.5),
                    ),
            );
            match draft.filter_type {
                FilterType::Subtree => {
                    let error = xml_fragment_error(&draft.filter);
                    filter = filter
                        .child(self.code_editor(
                            EditorKey::Filter,
                            "html",
                            &draft.filter,
                            false,
                            Some((3, 16)),
                            true,
                            "<interface xmlns=\"urn:example:interfaces\">\n  <name>eth0</name>\n</interface>",
                            window,
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(caption.size))
                                .line_height(px(caption.line_height))
                                .text_color(if error.is_some() { p.error.main } else { p.text.secondary })
                                .child(error.unwrap_or_else(|| {
                                    "Empty selects everything. Right-click a node in the explorer to filter on it.".into()
                                })),
                        );
                }
                FilterType::Xpath => {
                    let input = self.bound_input(
                        FieldKey::XpathFilter,
                        "/interfaces/interface[name='eth0']",
                        &draft.filter,
                        window,
                        cx,
                    );
                    filter = filter.child(
                        FormField::input("xpath", &input)
                            .mono(12.5)
                            .placeholder("/interfaces/interface[name='eth0']")
                            .flexible(),
                    );
                }
                FilterType::None => {}
            }
            form = form.child(filter);
        }
        if matches!(op, NetconfOperation::Get | NetconfOperation::GetConfig) && has("with-defaults")
        {
            let value = match draft.with_defaults.as_str() {
                "" => "Device default",
                "report-all" => "report-all: include defaults",
                "report-all-tagged" => "report-all-tagged",
                "trim" => "trim: leave defaults out",
                "explicit" => "explicit: only what was set",
                other => other,
            };
            form = form.child(div().flex().child(self.select_field(
                SelectField::WithDefaults,
                "Defaults",
                value.into(),
                220.,
                false,
                cx,
            )));
        }
        if op == NetconfOperation::EditConfig {
            let error = xml_fragment_error(&draft.config);
            let mut edit = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field_label("Configuration", &theme))
                .child(self.code_editor(
                    EditorKey::Config,
                    "html",
                    &draft.config,
                    false,
                    Some((6, 24)),
                    true,
                    "<interface xmlns=\"urn:example:interfaces\">\n  <name>eth0</name>\n  <description>uplink</description>\n</interface>",
                    window,
                    cx,
                ));
            if let Some(error) = error {
                edit = edit.child(
                    div()
                        .text_size(px(caption.size))
                        .line_height(px(caption.line_height))
                        .text_color(p.error.main)
                        .child(error),
                );
            }
            let default_operation = if draft.default_operation.is_empty() {
                "merge (default)".to_owned()
            } else {
                draft.default_operation.clone()
            };
            let mut selects = div()
                .flex()
                .flex_wrap()
                .gap(px(8.))
                .child(self.select_field(
                    SelectField::DefaultOperation,
                    "Default operation",
                    default_operation,
                    170.,
                    false,
                    cx,
                ));
            if has(":validate") {
                let test = if draft.test_option.is_empty() {
                    "default".to_owned()
                } else {
                    draft.test_option.clone()
                };
                selects = selects.child(self.select_field(
                    SelectField::TestOption,
                    "Test option",
                    test,
                    160.,
                    false,
                    cx,
                ));
            }
            let on_error = if draft.error_option.is_empty() {
                "stop-on-error (default)".to_owned()
            } else {
                draft.error_option.clone()
            };
            selects = selects.child(self.select_field(
                SelectField::ErrorOption,
                "On error",
                on_error,
                180.,
                false,
                cx,
            ));
            edit = edit.child(selects);
            if draft.target == Datastore::Candidate {
                edit = edit.child(
                    label(
                        "Changes land in the candidate. Review and commit them from the bar above.",
                    )
                    .size(caption.size)
                    .line_height(caption.line_height)
                    .color(p.text.secondary),
                );
            }
            form = form.child(edit);
        }
        if matches!(
            op,
            NetconfOperation::Commit | NetconfOperation::CancelCommit
        ) {
            let mut commit = div().flex().flex_col().gap(px(8.));
            if op == NetconfOperation::Commit && has(":confirmed-commit") {
                let confirmed = draft.confirmed;
                let text = super::logic::format::js_number_string(draft.confirm_timeout);
                let input = self.bound_input(FieldKey::ConfirmTimeout, "", &text, window, cx);
                commit = commit.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(switch_row_flush(
                            "confirmed",
                            confirmed,
                            "Confirmed commit — roll back unless confirmed within",
                            &theme,
                            cx.listener_for_app(move |this: &mut Self, cx: &mut Context<Self>| {
                                this.update_netconf(|draft| draft.confirmed = !confirmed);
                                cx.notify();
                            }),
                        ))
                        .child(
                            FormField::input("confirm-timeout", &input)
                                .width(100.)
                                .disabled(!confirmed)
                                .end(end_adornment("s", &theme)),
                        ),
                );
            }
            if has(":confirmed-commit:1.1") {
                let label_text = if op == NetconfOperation::Commit && draft.confirmed {
                    "Persist id (optional)"
                } else {
                    "Persist id of a persistent confirmed commit"
                };
                let input = self.bound_input(FieldKey::Persist, "", &draft.persist, window, cx);
                commit = commit.child(
                    div().max_w(px(360.)).child(
                        FormField::input("persist", &input)
                            .label(label_text)
                            .flexible(),
                    ),
                );
            }
            form = form.child(commit);
        }
        if op == NetconfOperation::GetSchema {
            let module = self.bound_input(
                FieldKey::SchemaIdentifier,
                "ietf-interfaces",
                &draft.schema_identifier,
                window,
                cx,
            );
            let revision = self.bound_input(
                FieldKey::SchemaVersion,
                "latest",
                &draft.schema_version,
                window,
                cx,
            );
            form = form.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        div().flex_1().max_w(px(420.)).child(
                            FormField::input("schema-module", &module)
                                .placeholder("ietf-interfaces")
                                .label("Module")
                                .flexible(),
                        ),
                    )
                    .child(
                        FormField::input("schema-revision", &revision)
                            .placeholder("latest")
                            .label("Revision")
                            .width(140.),
                    ),
            );
        }
        if op == NetconfOperation::KillSession {
            let input = self.bound_input(FieldKey::SessionId, "", &draft.session_id, window, cx);
            form = form.child(
                div().flex().child(
                    FormField::input("session-id", &input)
                        .label("Session id")
                        .width(160.),
                ),
            );
        }
        if op == NetconfOperation::Custom {
            let error = xml_fragment_error(&draft.rpc);
            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(field_label("RPC", &theme))
                    .child(self.code_editor(
                        EditorKey::Rpc,
                        "html",
                        &draft.rpc,
                        false,
                        Some((6, 24)),
                        true,
                        "<get-config>\n  <source><running/></source>\n</get-config>",
                        window,
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(caption.size))
                            .line_height(px(caption.line_height))
                            .text_color(if error.is_some() { p.error.main } else { p.text.secondary })
                            .child(error.unwrap_or_else(|| {
                                "The operation goes inside <rpc>; Muxus adds the envelope and message-id. A whole <rpc> pasted in works too.".into()
                            })),
                    ),
            );
        }
        if let Some(caption_text) = netconf_caption(op) {
            form = form.child(
                label(caption_text)
                    .size(caption.size)
                    .line_height(caption.line_height)
                    .color(p.text.secondary),
            );
        }
        let preview = self.ui.preview;
        let mut section = div().child(
            div().flex().child(
                Button::new("rpc-preview", "RPC as sent")
                    .tint(text_tint(p.text.secondary, &theme))
                    .font_size(12.)
                    .padding_y(4.)
                    .padding_x(4.)
                    .end_icon("ExpandMore", 8.)
                    .end_icon_turned(preview)
                    .on_click(cx.listener_for_app(
                        move |this: &mut Self, cx: &mut Context<Self>| {
                            this.ui.preview = !preview;
                            cx.notify();
                        },
                    )),
            ),
        );
        if preview {
            let xml = pretty_xml(&rpc_envelope(&netconf_rpc_body(draft), "101"));
            section = section.child(div().pt(px(4.)).child(self.code_editor(
                EditorKey::RpcPreview,
                "html",
                &xml,
                true,
                Some((3, 16)),
                false,
                "",
                window,
                cx,
            )));
        }
        form.child(section)
    }

    /// The code editor frame (`CodeEditor`): a 1px outline, Monaco's white or
    /// #1e1e1e page, growing with its content between `auto_height` lines.
    #[allow(clippy::too_many_arguments)]
    /// A multi-line text input without a frame (`TextField multiline`'s
    /// text area), `size` px mono text, growing from `rows.0` to `rows.1` lines.
    pub(super) fn text_area(
        &mut self,
        key: EditorKey,
        value: &str,
        rows: (usize, usize),
        size: f32,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let state = self.editor(key, "text", value, false, false, window, cx);
        // MUI sets `line-height: 1.4375em` on the input root, so the text area's
        // rows are body1 lines whatever its own font size.
        let line = theme.typography.body1.size * 1.4375;
        let lines = value.split('\n').count().clamp(rows.0, rows.1);
        let mut area = div()
            .relative()
            .w_full()
            .h(px(lines as f32 * line))
            .font_family(MONO)
            .text_size(px(size))
            .line_height(px(line))
            .text_color(theme.palette.text.primary)
            .child(
                muxus_ui::gpui_kit::component::input::Editor::new(&state)
                    .appearance(false)
                    .bordered(false)
                    .font_family(MONO)
                    .text_size(px(size))
                    .line_height(px(line)),
            );
        if !placeholder.is_empty() && value.is_empty() {
            area = area.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .text_color(theme.palette.text.disabled)
                    .child(SharedString::from(placeholder.to_owned())),
            );
        }
        area
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn code_editor(
        &mut self,
        key: EditorKey,
        language: &'static str,
        value: &str,
        read_only: bool,
        auto_height: Option<(usize, usize)>,
        line_numbers: bool,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let state = self.editor(key, language, value, read_only, line_numbers, window, cx);
        let focused =
            muxus_ui::gpui_kit::Focusable::focus_handle(state.read(cx), cx).is_focused(window);
        let mut frame = div()
            .relative()
            .min_h_0()
            .border_1()
            .border_color(if focused {
                theme.palette.primary.main
            } else {
                theme.palette.divider
            })
            .rounded(px(theme.radius))
            .overflow_hidden()
            .bg(theme.components.code_surface)
            .font_family(MONO)
            .text_size(px(12.5))
            .line_height(px(CODE_LINE))
            .text_color(theme.palette.text.primary)
            .pt(px(6.))
            .pb(px(6.))
            .pl(px(if line_numbers { 0. } else { 12. }))
            .child(muxus_ui::gpui_kit::base::input::Editor::new(&state));
        match auto_height {
            Some((min, max)) => {
                let lines = value.split('\n').count().clamp(min, max);
                frame = frame.h(px(lines as f32 * CODE_LINE + 12.));
            }
            None => frame = frame.size_full(),
        }
        if !placeholder.is_empty() && value.is_empty() {
            frame = frame.child(
                div()
                    .absolute()
                    .top(px(6.))
                    .left(px(if line_numbers { 48. } else { 12. }))
                    .right(px(12.))
                    .text_color(theme.palette.text.disabled)
                    .child(SharedString::from(placeholder.to_owned())),
            );
        }
        frame
    }
}
