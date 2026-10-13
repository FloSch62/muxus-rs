//! What the tools share (`tools/common.tsx`): their state in the workbench
//! store, gRPC calls and streams on the backend runtime, the network
//! instance list, and the fields and alerts they are built from.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use muxus_core::gnmi_path::{GnmiKeys, GnmiPath, GnmiPathElem, format_gnmi_path};
use muxus_core::management::{GnmiDataType, GrpcJson, ManagementError, ManagementResult};
use muxus_mgmt::workbench::connection::{GrpcStream, GrpcStreamUpdate, ManagementConnection};
use muxus_mgmt::workbench::requests::{GnmiDraft, ManagementDraft, default_gnmi_draft};
use muxus_ui::gpui_kit::{
    AnyElement, App, ClickEvent, Context, Div, Hsla, InteractiveElement, IntoElement,
    ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::IconButton;
use muxus_ui::widgets::tooltip::bounds_probe;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::super::fields::FieldKey;
use super::super::view::ManagementView;
use super::super::widgets::MONO;
use super::super::workbench::ListenerForApp;
use muxus_ui::widgets::display::Chip;
use muxus_ui::widgets::display::{Alert, AlertVariant, alert_ink};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::form_field::end_adornment;
use muxus_ui::widgets::listbox::{filter_options, listbox_popup};
use muxus_ui::widgets::menu::MenuItem;

/// A stream a tool started; it outlives the tool view, as `activeStreams` does.
pub struct ToolStream {
    pub stream: GrpcStream,
    /// Rotations: the request field that finalizes.
    pub finalize_field: Option<&'static str>,
}

/// The tools' view state that is not kept in the store: open streams, and
/// what each tool kept in its own component state (reset when another tool opens).
#[derive(Default)]
pub struct ToolsUi {
    pub streams: HashMap<&'static str, ToolStream>,
    /// The tool shown, the session generation and whether it was connected
    /// (React's effects that depend on `grpc`).
    pub mounted: Option<(&'static str, u64, bool)>,
    pub local: ToolLocal,
    /// Accounting records (kept here rather than in the store's JSON).
    pub acctz: super::acctz::AcctzRecords,
    /// The Files tool's listing and transfers.
    pub files: super::files::FilesData,
    pub transfer_counter: u64,
    /// When the reboot status was last asked for, for the 5 s refresh while one is pending.
    pub reboot_refreshed_at: Option<std::time::Instant>,
    /// The autocomplete whose option list is open.
    pub listbox: Option<FieldKey>,
}

/// Component state of the open tool.
#[derive(Default)]
pub struct ToolLocal {
    /// Ping's "More" options.
    pub more: bool,
    /// Pathz, certz and credentialz load once per mount.
    pub loaded: bool,
    /// The typed confirmation's text.
    pub typed: String,
    pub files: super::files::FilesLocal,
}

/// A gNOI/gNSI error as the tools show it (`errorOf`).
pub fn error_of(
    error: muxus_mgmt::workbench::connection::ManagementRequestError,
) -> ManagementError {
    error.error
}

/// An error with only a message.
pub fn message_error(message: impl Into<String>) -> ManagementError {
    ManagementError {
        message: message.into(),
        code: None,
        cancelled: None,
    }
}

/// `gnoi.types.Path` JSON as gNMI path text.
pub fn json_path_text(path: Option<&Value>) -> String {
    let Some(path) = path.and_then(Value::as_object) else {
        return "/".into();
    };
    let origin = path
        .get("origin")
        .and_then(Value::as_str)
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned);
    let elems = path
        .get("elem")
        .and_then(Value::as_array)
        .map(|elems| {
            elems
                .iter()
                .map(|elem| GnmiPathElem {
                    name: elem
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    keys: elem.get("key").and_then(Value::as_object).map(|keys| {
                        keys.iter()
                            .map(|(key, value)| {
                                (
                                    key.clone(),
                                    value
                                        .as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| value.to_string()),
                                )
                            })
                            .collect::<GnmiKeys>()
                    }),
                })
                .collect()
        })
        .unwrap_or_default();
    format_gnmi_path(&GnmiPath { origin, elems })
}

/// `Number(text.replace(/\D/g, '')) || fallback`.
pub fn digits_number(text: &str, fallback: f64) -> f64 {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    match digits.parse::<f64>() {
        Ok(value) if value != 0. => value,
        _ => fallback,
    }
}

/// The user part of a `user@host` address, the probes' default user.
pub fn default_user(address: Option<&str>) -> String {
    match address {
        Some(address) if address.contains('@') => {
            address.split('@').next().unwrap_or_default().to_owned()
        }
        _ => String::new(),
    }
}

/// A field of a tool (`TextField size="small"` with `compactFieldSx`), or a
/// free-solo `Autocomplete` when it has options.
pub struct ToolField {
    pub key: FieldKey,
    pub label: String,
    pub placeholder: String,
    pub value: String,
    pub width: Option<f32>,
    /// `flex: '1 1 <basis>px'`.
    pub basis: Option<f32>,
    pub min_width: Option<f32>,
    pub mono: bool,
    pub end: Option<&'static str>,
    pub options: Option<Vec<String>>,
}

impl ToolField {
    pub fn new(name: &'static str, label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: FieldKey::Tool(name),
            label: label.into(),
            placeholder: String::new(),
            value: value.into(),
            width: None,
            basis: None,
            min_width: None,
            mono: false,
            end: None,
            options: None,
        }
    }

    /// A field of a list row (rules and groups of the path policy editor).
    pub fn row(
        name: &'static str,
        index: usize,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self {
            key: FieldKey::ToolRow(name, index),
            ..Self::new(name, label, value)
        }
    }

    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.placeholder = text.into();
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn basis(mut self, basis: f32) -> Self {
        self.basis = Some(basis);
        self
    }

    /// `flex: 1`.
    pub fn grow(mut self) -> Self {
        self.basis = Some(0.);
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn end(mut self, text: &'static str) -> Self {
        self.end = Some(text);
        self
    }

    pub fn options(mut self, options: Vec<String>) -> Self {
        self.options = Some(options);
        self
    }
}

/// Wrap a flex item the way `width` or `flex: '1 1 <basis>px'` places it.
pub fn flex_item(
    element: impl IntoElement,
    width: Option<f32>,
    basis: Option<f32>,
    min_width: Option<f32>,
) -> Div {
    let item = div().flex();
    let item = match (width, basis) {
        (Some(width), _) => item.w(px(width)).flex_none(),
        (None, Some(basis)) => item
            .flex_grow(1.)
            .flex_shrink(1.)
            .flex_basis(px(basis))
            .min_w(px(min_width.unwrap_or(0.))),
        (None, None) => item.flex_none(),
    };
    item.child(element)
}

impl ManagementView {
    /// A tool's state (`useToolState`), its defaults when it has none.
    pub(in super::super) fn tool<T: DeserializeOwned + Default>(&self, key: &str) -> T {
        self.store.read(|state| {
            state
                .tools
                .get(key)
                .and_then(|value| serde_json::from_value(value.clone()).ok())
                .unwrap_or_default()
        })
    }

    /// Change a tool's state.
    pub(in super::super) fn update_tool<T: Serialize + DeserializeOwned + Default>(
        &mut self,
        key: &str,
        update: impl FnOnce(&mut T),
    ) {
        self.store.update(|state| {
            state.set_tool_state(key, |current| {
                let mut value: T = current
                    .and_then(|value| serde_json::from_value(value.clone()).ok())
                    .unwrap_or_default();
                update(&mut value);
                serde_json::to_value(value).unwrap_or(Value::Null)
            })
        });
    }

    /// The live connection for gNOI/gNSI calls (`useGrpc`), while connected.
    pub(in super::super) fn grpc(&self) -> Option<ManagementConnection> {
        if !self.connected() {
            return None;
        }
        self.controller
            .connection_for_tools()
            .map(|connection| (*connection).clone())
    }

    /// The session's address as dialed (`user@host:port`).
    pub(in super::super) fn session_address(&self) -> Option<String> {
        self.controller
            .gnmi_info()
            .map(|info| info.address.clone())
            .or_else(|| {
                self.controller
                    .netconf_info()
                    .map(|info| info.address.clone())
            })
    }

    /// A unary call; `done` gets the response or the error on the UI side.
    pub(in super::super) fn grpc_call(
        &mut self,
        method: &'static str,
        request: Value,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<GrpcJson, ManagementError>, &mut Context<Self>) + 'static,
    ) -> bool {
        let Some(connection) = self.grpc() else {
            return false;
        };
        let request = match request {
            Value::Object(map) => map,
            _ => GrpcJson::new(),
        };
        let call = self.on_runtime(async move { connection.grpc_call(method, request).await });
        cx.spawn(async move |this, cx| {
            let result = match call.await {
                Some(Ok(response)) => Ok(response),
                Some(Err(err)) => Err(error_of(err)),
                None => Err(message_error("The session closed.")),
            };
            let _ = this.update(cx, |this, cx| {
                done(this, result, cx);
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// Start a streaming call under `key`, replacing one already running there.
    #[allow(clippy::too_many_arguments)]
    pub(in super::super) fn start_stream(
        &mut self,
        key: &'static str,
        method: &str,
        request: Option<Value>,
        finalize_field: Option<&'static str>,
        cx: &mut Context<Self>,
        on_messages: impl Fn(&mut Self, Vec<GrpcJson>, &mut Context<Self>) + 'static,
        on_end: impl FnOnce(&mut Self, Option<ManagementError>, &mut Context<Self>) + 'static,
    ) -> bool {
        self.cancel_stream(key);
        let Some(connection) = self.grpc() else {
            return false;
        };
        let request = request.map(|request| match request {
            Value::Object(map) => map,
            _ => GrpcJson::new(),
        });
        let mut stream = connection.grpc_stream(method, request);
        let mut updates = std::mem::replace(
            &mut stream.updates,
            tokio::sync::mpsc::unbounded_channel().1,
        );
        let id = stream.id.clone();
        self.ui.tools.streams.insert(
            key,
            ToolStream {
                stream,
                finalize_field,
            },
        );
        cx.spawn(async move |this, cx| {
            let mut on_end = Some(on_end);
            while let Some(update) = updates.recv().await {
                let keep = this
                    .update(cx, |this, cx| match update {
                        GrpcStreamUpdate::Messages(messages) => {
                            on_messages(this, messages, cx);
                            cx.notify();
                            true
                        }
                        GrpcStreamUpdate::End(error) => {
                            if this
                                .ui
                                .tools
                                .streams
                                .get(key)
                                .is_some_and(|active| active.stream.id == id)
                            {
                                this.ui.tools.streams.remove(key);
                            }
                            if let Some(on_end) = on_end.take() {
                                on_end(this, error, cx);
                            }
                            cx.notify();
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep {
                    return;
                }
            }
            // The connection went away without an end message.
            let _ = this.update(cx, |this, cx| {
                if this
                    .ui
                    .tools
                    .streams
                    .get(key)
                    .is_some_and(|active| active.stream.id == id)
                {
                    this.ui.tools.streams.remove(key);
                }
                if let Some(on_end) = on_end.take() {
                    on_end(this, Some(message_error("The session closed.")), cx);
                }
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// `activeStream(store, key)?.cancel()`.
    pub(in super::super) fn cancel_stream(&mut self, key: &'static str) {
        if let Some(active) = self.ui.tools.streams.get(key) {
            active.stream.cancel();
        }
    }

    /// Rotations: send the finalize message and close our side.
    pub(in super::super) fn finalize_stream(&mut self, key: &'static str) -> bool {
        let Some(active) = self.ui.tools.streams.get(key) else {
            return false;
        };
        let Some(field) = active.finalize_field else {
            return false;
        };
        let mut message = GrpcJson::new();
        message.insert(field.to_owned(), Value::Object(GrpcJson::new()));
        active.stream.send(message);
        active.stream.close_send();
        true
    }

    /// Send another message on an open stream.
    pub(in super::super) fn stream_send(&mut self, key: &'static str, message: Value) {
        if let (Some(active), Value::Object(map)) = (self.ui.tools.streams.get(key), message) {
            active.stream.send(map);
        }
    }

    /// Read names over gNMI (`controller.request`), trying `paths` in turn
    /// until one yields some; `None` when every request failed.
    pub(in super::super) fn gnmi_names(
        &mut self,
        paths: &'static [&'static str],
        data_type: GnmiDataType,
        collect: fn(&ManagementResult, &mut BTreeSet<String>),
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Option<Vec<String>>, &mut Context<Self>) + 'static,
    ) {
        let controller = self.controller.clone();
        let work = self.on_runtime(async move {
            let mut failed = true;
            for path in paths {
                let draft = ManagementDraft::Gnmi(GnmiDraft {
                    paths: vec![(*path).into()],
                    data_type,
                    ..default_gnmi_draft()
                });
                let Ok(result) = controller.request(&draft, None).await else {
                    continue;
                };
                failed = false;
                let mut names = BTreeSet::new();
                collect(&result, &mut names);
                if !names.is_empty() {
                    return Some(names.into_iter().collect::<Vec<_>>());
                }
            }
            if failed { None } else { Some(Vec::new()) }
        });
        cx.spawn(async move |this, cx| {
            let names = work.await.flatten();
            let _ = this.update(cx, |this, cx| {
                done(this, names, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Network instance (VRF) names, read once over gNMI (`useNetworkInstances`).
    pub(in super::super) fn network_instances(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        #[derive(Default, Serialize, serde::Deserialize)]
        #[serde(default)]
        struct Instances {
            names: Vec<String>,
            loaded: bool,
        }
        let state: Instances = self.tool("network-instances");
        if !state.loaded && self.connected() {
            self.update_tool::<Instances>("network-instances", |state| state.loaded = true);
            self.gnmi_names(
                &[
                    "/network-instance[name=*]/name",
                    "/network-instances/network-instance[name=*]/name",
                ],
                GnmiDataType::Config,
                |result, names| {
                    let ManagementResult::GnmiGet { notifications } = result else {
                        return;
                    };
                    for notification in notifications {
                        for update in &notification.updates {
                            if let Some(name) = key_value(&update.path, "[name=") {
                                names.insert(name);
                            }
                            collect_names(&update.value.value, names, true);
                        }
                    }
                },
                cx,
                |this, names, _| {
                    if let Some(names) = names.filter(|names| !names.is_empty()) {
                        this.update_tool::<Instances>("network-instances", |state| {
                            state.names = names
                        });
                    }
                },
            );
        }
        state.names
    }

    // Fields

    /// Render a tool field.
    pub(in super::super) fn tool_field(
        &mut self,
        field: ToolField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = &theme.palette;
        let key = field.key.clone();
        let input = self.bound_input(key.clone(), &field.placeholder, &field.value, window, cx);
        let focused = self.input_focused(&key, window, cx);
        let id = SharedString::from(format!("tool-{key:?}"));
        let mut outlined = FormField::input(id.clone(), &input)
            .placeholder(field.placeholder.clone())
            .compact(30., 12.5)
            .shrink(true)
            .notch(p.background_paper)
            .flexible();
        if field.mono {
            outlined = outlined.mono(12.5);
        }
        if !field.label.is_empty() {
            outlined = outlined.label(field.label.clone());
        }
        if let Some(end) = field.end {
            outlined = outlined.end(end_adornment(end, &theme));
        }
        if field.options.is_none() {
            return flex_item(outlined, field.width, field.basis, field.min_width)
                .into_any_element();
        }
        // An Autocomplete: the clear button sits in the right padding, shown
        // on hover and while focused.
        let group: SharedString = format!("clear-{id}").into();
        let mut element = div()
            .group(group.clone())
            .relative()
            .flex()
            .flex_1()
            .min_w_0();
        if field.value.is_empty() {
            element = element.child(outlined);
        } else {
            let clear_key = key.clone();
            let mut clear = div().absolute().right(px(7.)).top(px(1.));
            clear = if focused {
                clear
            } else {
                clear.invisible().group_hover(group, |s| s.visible())
            };
            element = element.child(outlined.padding_right(43.)).child(
                clear.child(
                    IconButton::new(SharedString::from(format!("clear-{id}")), "Close")
                        .icon_size(20.)
                        .padding(4.)
                        .tooltip("Clear")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_tool_input(&clear_key, String::new(), window, cx);
                        })),
                ),
            );
        }
        let element = element.into_any_element();
        self.with_options(field, element, focused, &theme, cx)
    }

    /// Put `value` into a tool input and the tool's state.
    pub(in super::super) fn set_tool_input(
        &mut self,
        key: &FieldKey,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match key {
            FieldKey::Tool(name) => self.on_tool_field(name, value.clone(), cx),
            FieldKey::ToolRow(name, index) => {
                self.on_tool_row_field(name, *index, value.clone(), cx)
            }
            _ => {}
        }
        if let Some(state) = self.input_entity(key) {
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        cx.notify();
    }

    /// The listbox of a free-solo autocomplete (`listbox_popup`), open
    /// while its input has focus after a click or typing.
    fn with_options(
        &mut self,
        field: ToolField,
        element: AnyElement,
        focused: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let anchor = self.ui.anchor(format!("auto-{:?}", field.key));
        let mut column = flex_item(
            div()
                .relative()
                .flex()
                .flex_1()
                .min_w_0()
                .child(element)
                .child(bounds_probe(&anchor)),
            field.width,
            field.basis,
            field.min_width,
        );
        let options = field.options.unwrap_or_default();
        // An input that is exactly an option shows them all, as MUI does
        // while the input still holds the chosen value.
        let input = if options.contains(&field.value) {
            ""
        } else {
            field.value.as_str()
        };
        let shown: Vec<String> = filter_options(&options, input, Clone::clone)
            .into_iter()
            .cloned()
            .collect();
        let open =
            focused && self.ui.tools.listbox.as_ref() == Some(&field.key) && !shown.is_empty();
        if open {
            let items = shown
                .into_iter()
                .enumerate()
                .map(|(index, option)| {
                    let key = field.key.clone();
                    let chosen = option.clone();
                    MenuItem::new(("option", index), option)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ui.tools.listbox = None;
                            this.set_tool_input(&key, chosen.clone(), window, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            let dismiss = cx.closer_for_app(|this: &mut Self, cx: &mut Context<Self>| {
                this.ui.tools.listbox = None;
                cx.notify();
            });
            if let Some(popup) = listbox_popup(theme, &anchor, items, 40. * 7., dismiss) {
                column = column.child(popup);
            }
        }
        column.into_any_element()
    }
}

/// The value after `marker` up to the next `]`, as `/\[name=([^\]]+)\]/` finds it.
pub fn key_value(path: &str, marker: &str) -> Option<String> {
    let start = path.find(marker)? + marker.len();
    let end = path[start..].find(']')?;
    (end > 0).then(|| path[start..start + end].to_owned())
}

/// Every string under a `name` key (`prefixed`: also `module:name` keys).
pub fn collect_names(value: &Value, names: &mut BTreeSet<String>, prefixed: bool) {
    match value {
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_names(item, names, prefixed)),
        Value::Object(map) => {
            for (key, child) in map {
                let local = if prefixed {
                    key.rsplit(':').next().unwrap_or(key)
                } else {
                    key.as_str()
                };
                match child {
                    Value::String(text) if local == "name" => {
                        names.insert(text.clone());
                    }
                    _ => collect_names(child, names, prefixed),
                }
            }
        }
        _ => {}
    }
}

/// A click handler that outlives the render.
pub type Action = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `ErrorAlert`: a gRPC error with its code, dismissible when `on_close` is given.
pub fn error_alert(error: &ManagementError, theme: &Theme, on_close: Option<Action>) -> Div {
    let mut row = div().flex().flex_wrap().items_center().gap(px(8.));
    if let Some(code) = &error.code {
        row = row.child(Chip::new(code.clone()).height(20.).font_size(11.).mono());
    }
    row = row.child(
        div()
            .font_family(MONO)
            .text_size(px(12.5))
            .line_height(px(12.5 * 1.43))
            .child(error.message.clone()),
    );
    let status = if error.cancelled == Some(true) {
        Status::Info
    } else {
        Status::Error
    };
    let mut alert = Alert::new(status, "").outlined().content(row);
    if let Some(close) = on_close {
        alert = alert.action(
            IconButton::new("close-alert", "Close")
                .icon_size(20.)
                .padding(5.)
                .ink(alert_ink(theme, status, AlertVariant::Outlined))
                .on_click(move |event, window, cx| close(event, window, cx)),
        );
    }
    div().mb(px(16.)).child(alert)
}

/// A body2 line in a color.
pub fn body_text(text: impl Into<SharedString>, color: Hsla, theme: &Theme) -> Div {
    let body = theme.typography.body2;
    div()
        .text_size(px(body.size))
        .line_height(px(body.line_height))
        .text_color(color)
        .child(text.into())
}

/// A caption line in a color.
pub fn caption_text(text: impl Into<SharedString>, color: Hsla, theme: &Theme) -> Div {
    let caption = theme.typography.caption;
    div()
        .text_size(px(caption.size))
        .line_height(px(caption.line_height))
        .text_color(color)
        .child(text.into())
}

/// `TypedConfirmDialog`'s typed text matches.
pub fn typed_matches(typed: &str, expected: &str) -> bool {
    muxus_core::js::js_trim(typed) == expected
}
