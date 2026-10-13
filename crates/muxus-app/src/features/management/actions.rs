//! What the workbench's controls do: field edits into the draft, running a
//! request (with its confirmations and the Set review), node actions,
//! the library, "Copy as", path completion and the keyboard.

use muxus_core::management::{GnmiSetEncoding, ManagementResult};
use muxus_core::protocol::{GnmiProfile, ManagementProfile, NetconfProfile};
use muxus_mgmt::workbench::connection::RequestMessage;
use muxus_mgmt::workbench::copy_as::{gnmic_command, ncclient_script, netconf_ssh_command};
use muxus_mgmt::workbench::data_tree::{DataNode, DataNodeKind, find_node, find_node_mut};
use muxus_mgmt::workbench::requests::{
    FilterType, GnmiDraft, GnmiOperation, GnmiSetItem, GnmiSetItemOp, ManagementDraft,
    NetconfDraft, NetconfOperation, default_gnmi_draft, default_netconf_draft, gnmi_draft_problem,
    gnmi_get_message, gnmi_set_message, gnmi_subscribe_message, netconf_changes_state,
    netconf_draft_problem, netconf_rpc_body,
};
use muxus_mgmt::workbench::set_review::{canonical_json, proposed_value, set_items};
use muxus_mgmt::workbench::xml::{parse_rpc_reply, pretty_xml, rpc_envelope, serialize_element};
use muxus_ui::gpui_kit::base::input::InputEvent;
use muxus_ui::gpui_kit::{App, Context, FocusHandle, KeystrokeEvent, Window};
use muxus_ui::theme::Status;
use serde_json::Value;

use super::fields::{EditorKey, FieldKey};
use super::logic::editor::{
    copy_encoding, digits_only, netconf_confirmation, parse_confirm_seconds, parse_confirm_timeout,
    parse_depth, parse_heartbeat, parse_sample_seconds,
};
use super::logic::node_actions as node;
use super::logic::path_field::{Suggestion, accept_suggestion, suggestions_for};
use super::overlays::{NodeAction, NodeActionKind};
use super::ui::{
    ConfirmAction, Dialog, PromptAction, ReviewApply, ReviewSection, SelectField, TreeSource,
};
use super::view::ManagementView;

impl ManagementView {
    // The draft

    /// Change the gNMI draft in place.
    pub(super) fn update_gnmi(&mut self, update: impl FnOnce(&mut GnmiDraft)) {
        self.store.update(|state| {
            if let ManagementDraft::Gnmi(draft) = &mut state.draft {
                update(draft);
            }
        });
    }

    /// Change the NETCONF draft in place.
    pub(super) fn update_netconf(&mut self, update: impl FnOnce(&mut NetconfDraft)) {
        self.store.update(|state| {
            if let ManagementDraft::Netconf(draft) = &mut state.draft {
                update(draft);
            }
        });
    }

    fn update_set_item(&mut self, id: &str, update: impl FnOnce(&mut GnmiSetItem)) {
        self.update_gnmi(|draft| {
            if let Some(item) = draft.set.iter_mut().find(|item| item.id == id) {
                update(item);
            }
        });
    }

    pub(super) fn profile_id(&self) -> Option<String> {
        match self.dialed_profile() {
            ManagementProfile::Gnmi(profile) => profile.profile_id.clone(),
            ManagementProfile::Netconf(profile) => profile.profile_id.clone(),
        }
    }

    /// A focus handle by name, created on first use.
    pub(super) fn tree_focus(&mut self, name: &str, cx: &mut Context<Self>) -> FocusHandle {
        self.ui
            .focus
            .entry(name.to_owned())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// The request editor's focus (for Ctrl/Cmd+Enter anywhere inside it).
    pub(super) fn editor_focus(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.tree_focus("editor", cx)
    }

    // Field events

    pub(super) fn on_field_event(
        &mut self,
        key: FieldKey,
        event: &InputEvent,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path_field = matches!(key, FieldKey::Path(_) | FieldKey::SetPath(_))
            || Self::is_tool_path_field(&key);
        match event {
            InputEvent::Change => {
                if matches!(key, FieldKey::Tool(_)) {
                    self.ui.tools.listbox = Some(key.clone());
                }
                self.on_field_change(&key, value, cx);
                if path_field {
                    self.ui.suggest.field = Some(key.clone());
                    self.ui.suggest.open = true;
                    self.ui.suggest.highlighted = 0;
                    self.ui.path_loads.clear();
                }
            }
            InputEvent::Focus => {
                if matches!(key, FieldKey::Tool(_)) {
                    self.ui.tools.listbox = Some(key.clone());
                }
                if path_field {
                    self.ui.suggest.field = Some(key.clone());
                    self.ui.suggest.open = true;
                    self.ui.suggest.highlighted = 0;
                    self.ui.path_loads.clear();
                }
            }
            InputEvent::Blur => {
                if path_field && self.ui.suggest.field.as_ref() == Some(&key) {
                    self.ui.suggest.open = false;
                }
            }
            InputEvent::PressEnter { .. } => match key {
                FieldKey::Tool(name) if Self::is_tool_path_field(&key) => {
                    if !self.accept_highlighted(&key, window, cx) {
                        self.ui.suggest.open = false;
                        self.on_tool_enter(name, window, cx);
                    }
                }
                FieldKey::Path(_) | FieldKey::SetPath(_) => {
                    if !self.accept_highlighted(&key, window, cx) {
                        self.ui.suggest.open = false;
                        self.run_draft(window, cx);
                    }
                }
                FieldKey::XpathFilter => self.run_draft(window, cx),
                FieldKey::SaveName => self.save_request(cx),
                FieldKey::PromptText => {
                    if let Some(Dialog::Prompt { action, .. }) = self.ui.dialog.clone() {
                        self.submit_prompt(action, window, cx);
                    }
                }
                FieldKey::Tool(name) => self.on_tool_enter(name, window, cx),
                _ => {}
            },
        }
        cx.notify();
    }

    fn on_field_change(&mut self, key: &FieldKey, value: String, cx: &mut Context<Self>) {
        match key {
            FieldKey::ExplorerFilter => self.ui.explorer_filter = value,
            FieldKey::LibraryFilter => self.ui.library_filter = value,
            FieldKey::DeviceFilter => self.ui.device_filter = value,
            FieldKey::ResultFilter => self.ui.result_filter = value,
            FieldKey::LiveFilter => self.ui.live_filter = value,
            FieldKey::Path(index) => {
                let index = *index;
                self.update_gnmi(|draft| {
                    if let Some(path) = draft.paths.get_mut(index) {
                        *path = value;
                    }
                });
            }
            FieldKey::SetPath(id) => {
                let id = id.clone();
                self.update_set_item(&id, |item| item.path = value);
            }
            FieldKey::Prefix => self.update_gnmi(|draft| draft.prefix = value),
            FieldKey::Depth => self.update_gnmi(|draft| draft.depth = parse_depth(&value)),
            FieldKey::SampleSeconds => self
                .update_gnmi(|draft| draft.subscribe.sample_seconds = parse_sample_seconds(&value)),
            FieldKey::Heartbeat => self
                .update_gnmi(|draft| draft.subscribe.heartbeat_seconds = parse_heartbeat(&value)),
            FieldKey::ConfirmSeconds => self
                .update_gnmi(|draft| draft.confirm_seconds = Some(parse_confirm_seconds(&value))),
            FieldKey::XpathFilter => self.update_netconf(|draft| draft.filter = value),
            FieldKey::Persist => self.update_netconf(|draft| draft.persist = value),
            FieldKey::ConfirmTimeout => {
                self.update_netconf(|draft| draft.confirm_timeout = parse_confirm_timeout(&value))
            }
            FieldKey::SchemaIdentifier => {
                let revision = self.controller.netconf_info().and_then(|info| {
                    super::logic::device::hello_modules(&info.capabilities)
                        .into_iter()
                        .find(|module| module.name == value)
                        .and_then(|module| module.revision)
                });
                self.update_netconf(|draft| {
                    draft.schema_identifier = value;
                    draft.schema_version = revision.unwrap_or_default();
                });
            }
            FieldKey::SchemaVersion => self.update_netconf(|draft| draft.schema_version = value),
            FieldKey::SessionId => {
                self.update_netconf(|draft| draft.session_id = digits_only(&value))
            }
            FieldKey::SaveName => {}
            FieldKey::PromptText => {
                if let Some(Dialog::Prompt { action, error, .. }) = &mut self.ui.dialog {
                    *error = match action {
                        PromptAction::CommitConfirmed => {
                            let trimmed = muxus_core::js::js_trim(&value);
                            (!trimmed.is_empty()
                                && !(trimmed.chars().all(|c| c.is_ascii_digit())
                                    && trimmed.parse::<u64>().is_ok_and(|seconds| seconds > 0)))
                            .then(|| "Enter a number of seconds.".to_owned())
                        }
                        PromptAction::RenameRequest(_) | PromptAction::AddCertzProfile => None,
                    };
                }
            }
            FieldKey::Tool(name) => self.on_tool_field(name, value, cx),
            FieldKey::ToolRow(name, index) => self.on_tool_row_field(name, *index, value, cx),
        }
    }

    pub(super) fn on_editor_change(
        &mut self,
        key: EditorKey,
        value: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match key {
            EditorKey::SetValue(id) => self.update_set_item(&id, |item| item.value = value),
            EditorKey::Filter => self.update_netconf(|draft| draft.filter = value),
            EditorKey::Config => self.update_netconf(|draft| draft.config = value),
            EditorKey::Rpc => self.update_netconf(|draft| draft.rpc = value),
            EditorKey::Tool(name) => self.on_tool_field(name, value, cx),
            EditorKey::RpcPreview | EditorKey::Result(_) => {}
        }
        cx.notify();
    }

    pub(super) fn choose_select(
        &mut self,
        field: &SelectField,
        index: usize,
        _cx: &mut Context<Self>,
    ) {
        match field {
            SelectField::Encoding => {
                let offered = self
                    .controller
                    .gnmi_info()
                    .map(|info| info.encodings.clone())
                    .filter(|encodings| !encodings.is_empty())
                    .unwrap_or_else(|| {
                        use muxus_core::management::GnmiEncoding::*;
                        vec![JsonIetf, Json, Proto, Ascii]
                    });
                let encoding = index
                    .checked_sub(1)
                    .and_then(|index| offered.get(index).copied());
                self.update_gnmi(|draft| draft.encoding = encoding);
            }
            SelectField::SetOp(id) => {
                let op = [
                    GnmiSetItemOp::Update,
                    GnmiSetItemOp::Replace,
                    GnmiSetItemOp::Delete,
                ][index.min(2)];
                let id = id.clone();
                self.update_set_item(&id, |item| item.op = op);
            }
            SelectField::SetEncoding(id) => {
                let encoding = [
                    GnmiSetEncoding::JsonIetf,
                    GnmiSetEncoding::Json,
                    GnmiSetEncoding::Ascii,
                ][index.min(2)];
                let id = id.clone();
                self.update_set_item(&id, |item| item.encoding = encoding);
            }
            SelectField::WithDefaults => {
                let value =
                    ["", "report-all", "report-all-tagged", "trim", "explicit"][index.min(4)];
                self.update_netconf(|draft| draft.with_defaults = value.into());
            }
            SelectField::DefaultOperation => {
                let value = ["", "merge", "replace", "none"][index.min(3)];
                self.update_netconf(|draft| draft.default_operation = value.into());
            }
            SelectField::TestOption => {
                let value = ["", "test-then-set", "set", "test-only"][index.min(3)];
                self.update_netconf(|draft| draft.test_option = value.into());
            }
            SelectField::ErrorOption => {
                let value = [
                    "",
                    "stop-on-error",
                    "continue-on-error",
                    "rollback-on-error",
                ][index.min(3)];
                self.update_netconf(|draft| draft.error_option = value.into());
            }
            SelectField::Tool(_) | SelectField::ToolRow(..) => self.on_tool_select(field, index),
        }
    }

    // Path completion

    fn current_suggestions(&self, key: &FieldKey, cx: &Context<Self>) -> Vec<Suggestion> {
        let Some(value) = self.input_value(key, cx) else {
            return Vec::new();
        };
        self.store
            .read(|state| suggestions_for(&state.explorer.root, &value).suggestions)
    }

    /// Accept the highlighted suggestion; false when the list is closed or empty.
    fn accept_highlighted(
        &mut self,
        key: &FieldKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.ui.suggest.open || self.ui.suggest.field.as_ref() != Some(key) {
            return false;
        }
        let suggestions = self.current_suggestions(key, cx);
        let Some(suggestion) = suggestions.get(self.ui.suggest.highlighted).cloned() else {
            return false;
        };
        self.accept_suggestion(key, &suggestion, window, cx);
        true
    }

    pub(super) fn accept_suggestion(
        &mut self,
        key: &FieldKey,
        suggestion: &Suggestion,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = self.input_value(key, cx).unwrap_or_default();
        let next = accept_suggestion(&value, suggestion);
        self.on_field_change(key, next.clone(), cx);
        if let Some(state) = self.fields_input(key) {
            state.update(cx, |state, cx| {
                state.set_value(next, window, cx);
            });
        }
        self.ui.suggest.open = suggestion.branch;
        self.ui.suggest.highlighted = 0;
        self.ui.path_loads.clear();
        cx.notify();
    }

    fn fields_input(
        &mut self,
        key: &FieldKey,
    ) -> Option<muxus_ui::gpui_kit::Entity<muxus_ui::gpui_kit::base::input::InputState>> {
        self.input_entity(key)
    }

    /// Fetch the level being completed when the explorer has not loaded it.
    pub(super) fn load_path_level(&mut self, value: &str) {
        let target = self.store.read(|state| {
            let found = suggestions_for(&state.explorer.root, value);
            let node = match &found.parent {
                Some(parent) => find_node(&state.explorer.root, parent),
                None => None,
            };
            node.filter(|node| {
                !node.loaded && !node.is_leaf() && !state.explorer.loading.contains(&node.id)
            })
            .map(|node| node.id.clone())
            .or_else(|| {
                // A level the explorer never saw: create it from the parsed path.
                found.elems.filter(|elems| !elems.is_empty()).map(|elems| {
                    format!(
                        "\u{0}{}",
                        muxus_core::gnmi_path::format_gnmi_path(&muxus_core::gnmi_path::GnmiPath {
                            origin: None,
                            elems
                        })
                    )
                })
            })
        });
        let Some(target) = target else {
            return;
        };
        if !self.ui.path_loads.insert(target.clone()) {
            return;
        }
        if let Some(path) = target.strip_prefix('\u{0}') {
            // `nodeAt` creates the node, then the explorer loads it.
            let Ok(path) = muxus_core::gnmi_path::parse_gnmi_path(path) else {
                return;
            };
            let id = self.store.update(|state| {
                state.touch_explorer(|explorer| {
                    let registry = &mut explorer.registry;
                    let node = muxus_mgmt::workbench::data_tree::node_at(
                        &mut explorer.root,
                        &path,
                        Some(registry),
                    );
                    (!node.loaded && !node.is_leaf()).then(|| node.id.clone())
                })
            });
            if let Some(id) = id {
                self.controller.load_node_later(&id);
            }
        } else {
            self.controller.load_node_later(&target);
        }
    }

    /// Keys a path field's suggestion list and the editor's Ctrl/Cmd+Enter take
    /// before the focused input sees them. Returns whether the key was used.
    pub(super) fn intercept_key(
        &mut self,
        event: &KeystrokeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.focus.contains_focused(window, cx)
            && !self
                .ui
                .focus
                .values()
                .any(|focus| focus.contains_focused(window, cx))
        {
            return false;
        }
        let keystroke = &event.keystroke;
        let modifier = keystroke.modifiers.control || keystroke.modifiers.platform;
        let key = keystroke.key.as_str();
        // Ctrl/Cmd+Enter anywhere in the request editor runs it.
        if key == "enter" && modifier && self.ui.dialog.is_none() {
            let editor = self.editor_focus(cx);
            if editor.contains_focused(window, cx) {
                self.ui.suggest.open = false;
                self.run_draft(window, cx);
                return true;
            }
            return false;
        }
        let Some(field) = self.ui.suggest.field.clone() else {
            return false;
        };
        if !self.input_focused(&field, window, cx) {
            return false;
        }
        let open = self.ui.suggest.open;
        let count = if open {
            self.current_suggestions(&field, cx).len()
        } else {
            0
        };
        match key {
            "down" | "up" if !open => {
                if key == "down" {
                    self.ui.suggest.open = true;
                    cx.notify();
                    return true;
                }
                false
            }
            "down" | "up" => {
                if count > 0 {
                    let step = if key == "down" { 1 } else { count - 1 };
                    self.ui.suggest.highlighted = (self.ui.suggest.highlighted + step) % count;
                    cx.notify();
                }
                true
            }
            "tab" if open && count > 0 => self.accept_highlighted(&field, window, cx),
            "escape" if open => {
                self.ui.suggest.open = false;
                cx.notify();
                true
            }
            _ => false,
        }
    }

    // Running

    /// `run()` of the request editor: check, confirm or review, then run.
    pub(super) fn run_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connected() {
            return;
        }
        let draft = self.store.read(|state| state.draft.clone());
        let problem = match &draft {
            ManagementDraft::Gnmi(gnmi) => gnmi_draft_problem(gnmi),
            ManagementDraft::Netconf(netconf) => netconf_draft_problem(netconf),
        };
        if let Some(problem) = problem {
            self.toast(Status::Warning, problem, cx);
            return;
        }
        match &draft {
            ManagementDraft::Gnmi(gnmi) => {
                if gnmi.operation == GnmiOperation::Subscribe {
                    let streaming = self.store.read(|state| {
                        state
                            .runs
                            .iter()
                            .find(|run| Some(&run.id) == state.selected_run_id.as_ref())
                            .filter(|run| {
                                run.status == muxus_mgmt::workbench::store::RunStatus::Streaming
                            })
                            .map(|run| run.id.clone())
                    });
                    if let Some(id) = streaming {
                        self.controller.cancel(&id);
                        return;
                    }
                }
                if gnmi.operation == GnmiOperation::Set {
                    self.review_set(gnmi.clone(), cx);
                    return;
                }
            }
            ManagementDraft::Netconf(netconf) => {
                if let Some(confirmation) = netconf_confirmation(netconf) {
                    self.ui.dialog = Some(Dialog::Confirm {
                        confirmation,
                        action: ConfirmAction::RunDraft,
                    });
                    cx.notify();
                    return;
                }
            }
        }
        let _ = window;
        self.controller.run(Some(draft), false);
        cx.notify();
    }

    /// Show what a Set changes before it is sent.
    fn review_set(&mut self, draft: GnmiDraft, cx: &mut Context<Self>) {
        self.ui.dialog = Some(Dialog::Review {
            title: "Review changes".into(),
            description: Some(
                "This is how the configuration at each path changes. Nothing has been sent yet."
                    .into(),
            ),
            sections: Vec::new(),
            loading: true,
            error: None,
            apply: Some((ReviewApply::RunDraft, "Apply changes".into())),
            tab: 0,
        });
        cx.notify();
        let controller = self.controller.clone();
        let items: Vec<GnmiSetItem> = set_items(&draft).into_iter().cloned().collect();
        let work = self.on_runtime(async move {
            let mut sections = Vec::new();
            for item in items {
                let path = muxus_core::js::js_trim(&item.path).to_owned();
                let request = ManagementDraft::Gnmi(GnmiDraft {
                    paths: vec![path.clone()],
                    data_type: muxus_core::management::GnmiDataType::Config,
                    ..default_gnmi_draft()
                });
                let current = match controller.request(&request, None).await {
                    Ok(ManagementResult::GnmiGet { notifications }) => {
                        let values: Vec<Value> = notifications
                            .iter()
                            .flat_map(|notification| notification.updates.iter())
                            .map(|update| update.value.value.clone())
                            .collect();
                        match values.len() {
                            0 => None,
                            1 => values.into_iter().next(),
                            _ => Some(Value::Array(values)),
                        }
                    }
                    Ok(_) => None,
                    Err(err) if err.error.code.as_deref() == Some("NOT_FOUND") => None,
                    Err(err) => {
                        return Err(format!(
                            "Could not read the current configuration: {}",
                            err.error.message
                        ));
                    }
                };
                let note = current.is_none().then(|| {
                    if item.op == GnmiSetItemOp::Delete {
                        "Nothing is configured here."
                    } else {
                        "Not configured yet: this creates it."
                    }
                    .to_owned()
                });
                let proposed = proposed_value(&item, current.as_ref())
                    .map_err(|err| format!("Could not read the current configuration: {err}"))?;
                let op = serde_json::to_value(item.op)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default();
                sections.push((
                    format!("{op} {path}"),
                    canonical_json(current.as_ref()),
                    canonical_json(proposed.as_ref()),
                    note,
                ));
            }
            Ok(sections)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(Dialog::Review {
                    sections,
                    loading,
                    error,
                    ..
                }) = &mut this.ui.dialog
                {
                    *loading = false;
                    match result {
                        Some(Ok(found)) => {
                            *sections = found
                                .into_iter()
                                .map(|(label, original, modified, note)| {
                                    ReviewSection::new(label, original, modified, "json", note)
                                })
                                .collect();
                        }
                        Some(Err(message)) => *error = Some(message),
                        None => *error = Some("Could not read the current configuration.".into()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn apply_review(
        &mut self,
        what: ReviewApply,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui.dialog = None;
        match what {
            ReviewApply::RunDraft => {
                self.controller.run(None, false);
            }
            ReviewApply::Commit => {
                self.run_netconf(|draft| draft.operation = NetconfOperation::Commit);
            }
            ReviewApply::Rotate(key) => self.tool_review_applied(key, cx),
        }
        cx.notify();
    }

    pub(super) fn confirmed(
        &mut self,
        action: ConfirmAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            ConfirmAction::RunDraft => {
                self.controller.run(None, false);
            }
            ConfirmAction::Discard => {
                self.run_netconf(|draft| draft.operation = NetconfOperation::DiscardChanges);
            }
            ConfirmAction::DeleteRequest(id) => {
                let delete = self.backend.delete_management_request(id);
                cx.spawn(async move |this, cx| {
                    let result = delete.await;
                    let _ = this.update(cx, |this, cx| {
                        if let Err(error) = result {
                            this.toast(Status::Error, error.message, cx);
                        }
                        this.refresh_library(cx);
                    });
                })
                .detach();
            }
            ConfirmAction::Tool(action) => self.tool_confirmed(action, cx),
        }
        cx.notify();
    }

    pub(super) fn submit_prompt(
        &mut self,
        action: PromptAction,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = self
            .input_value(&FieldKey::PromptText, cx)
            .unwrap_or_default();
        let entered = muxus_core::js::js_trim(&value).to_owned();
        if entered.is_empty() {
            return;
        }
        if let Some(Dialog::Prompt { error: Some(_), .. }) = &self.ui.dialog {
            return;
        }
        self.ui.dialog = None;
        match action {
            PromptAction::RenameRequest(request) => {
                let mut body = serde_json::json!({
                    "id": request.id,
                    "protocol": request.protocol,
                    "name": entered,
                    "request": request.request,
                });
                if let Some(profile_id) = &request.profile_id {
                    body["profileId"] = Value::String(profile_id.clone());
                }
                let save = self.backend.save_management_request(body);
                cx.spawn(async move |this, cx| {
                    let result = save.await;
                    let _ = this.update(cx, |this, cx| {
                        if let Err(error) = result {
                            this.toast(Status::Error, error.message, cx);
                        }
                        this.refresh_library(cx);
                    });
                })
                .detach();
            }
            PromptAction::CommitConfirmed => {
                let seconds: f64 = entered.parse().unwrap_or(120.);
                self.run_netconf(|draft| {
                    draft.operation = NetconfOperation::Commit;
                    draft.confirmed = true;
                    draft.confirm_timeout = seconds;
                });
            }
            PromptAction::AddCertzProfile => self.add_certz_profile(entered, cx),
        }
        cx.notify();
    }

    // The library

    pub(super) fn open_save_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_profile = self.profile_id().is_some();
        self.ui.dialog = Some(Dialog::Save {
            host_only: has_profile,
            saving: false,
        });
        let state = self.input(FieldKey::SaveName, "", window, cx);
        state.update(cx, |state, cx| state.set_value("", window, cx));
        self.focus_input(&FieldKey::SaveName, window, cx);
        cx.notify();
    }

    pub(super) fn save_request(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Save {
            host_only,
            saving: false,
        }) = self.ui.dialog.clone()
        else {
            return;
        };
        let name = self
            .input_value(&FieldKey::SaveName, cx)
            .unwrap_or_default();
        let name = muxus_core::js::js_trim(&name).to_owned();
        if name.is_empty() {
            return;
        }
        if let Some(Dialog::Save { saving, .. }) = &mut self.ui.dialog {
            *saving = true;
        }
        let draft = self.store.read(|state| state.draft.clone());
        let request = serde_json::to_value(&draft).unwrap_or(Value::Null);
        let mut body = serde_json::json!({
            "protocol": self.protocol(),
            "name": name,
            "request": request,
        });
        if host_only && let Some(profile_id) = self.profile_id() {
            body["profileId"] = Value::String(profile_id);
        }
        let save = self.backend.save_management_request(body);
        cx.spawn(async move |this, cx| {
            let result = save.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(saved) => {
                        this.ui.dialog = None;
                        this.toast(Status::Success, format!("Saved “{}”.", saved.name), cx);
                        this.refresh_library(cx);
                    }
                    Err(error) => {
                        if let Some(Dialog::Save { saving, .. }) = &mut this.ui.dialog {
                            *saving = false;
                        }
                        this.toast(Status::Error, error.message, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn open_rename(
        &mut self,
        request: muxus_core::management::SavedManagementRequest,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ui.prompt_initial = Some(request.name.clone());
        self.ui.dialog = Some(Dialog::Prompt {
            title: "Rename request".into(),
            description: None,
            label: "Name".into(),
            confirm_label: "Save".into(),
            action: PromptAction::RenameRequest(request),
            error: None,
        });
        cx.notify();
    }

    pub(super) fn confirm_delete(
        &mut self,
        request: muxus_core::management::SavedManagementRequest,
        cx: &mut Context<Self>,
    ) {
        self.ui.dialog = Some(Dialog::Confirm {
            confirmation: super::logic::editor::Confirmation {
                title: format!("Delete “{}”?", request.name),
                description: String::new(),
                confirm_label: "Delete".into(),
                destructive: true,
            },
            action: ConfirmAction::DeleteRequest(request.id),
        });
        cx.notify();
    }

    // Copy as

    pub(super) fn copy_as(&mut self, index: usize, cx: &mut Context<Self>) {
        let draft = self.store.read(|state| state.draft.clone());
        let profile = self.dialed_profile().clone();
        let (text, what) = match (&draft, &profile) {
            (ManagementDraft::Gnmi(gnmi), ManagementProfile::Gnmi(profile)) => {
                if index == 0 {
                    let offered = self
                        .controller
                        .gnmi_info()
                        .map(|info| info.encodings.clone())
                        .unwrap_or_default();
                    (
                        gnmic_command(profile, Some(gnmi), copy_encoding(&offered)),
                        "gnmic command",
                    )
                } else {
                    (request_json(gnmi), "Request JSON")
                }
            }
            (ManagementDraft::Netconf(netconf), ManagementProfile::Netconf(profile)) => match index
            {
                0 => (
                    pretty_xml(&rpc_envelope(&netconf_rpc_body(netconf), "101")),
                    "RPC XML",
                ),
                1 => (ncclient_script(profile, netconf), "ncclient script"),
                _ => (netconf_ssh_command(profile), "ssh -s netconf command"),
            },
            _ => return,
        };
        self.copy(text, what, cx);
    }

    // Set items

    pub(super) fn load_current(&mut self, item: GnmiSetItem, cx: &mut Context<Self>) {
        self.ui.set_loading = Some(item.id.clone());
        cx.notify();
        let controller = self.controller.clone();
        let request = ManagementDraft::Gnmi(GnmiDraft {
            paths: vec![item.path.clone()],
            data_type: muxus_core::management::GnmiDataType::Config,
            encoding: (item.encoding == GnmiSetEncoding::Ascii)
                .then_some(muxus_core::management::GnmiEncoding::Ascii),
            ..default_gnmi_draft()
        });
        let work = self.on_runtime(async move { controller.request(&request, None).await });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.ui.set_loading = None;
                match result {
                    Some(Ok(ManagementResult::GnmiGet { notifications })) => {
                        let value = notifications
                            .iter()
                            .flat_map(|notification| notification.updates.iter())
                            .next()
                            .map(|update| update.value.value.clone());
                        let text = match value {
                            None => String::new(),
                            Some(Value::String(text))
                                if item.encoding == GnmiSetEncoding::Ascii =>
                            {
                                text
                            }
                            Some(value) => serde_json::to_string_pretty(&value).unwrap_or_default(),
                        };
                        this.update_set_item(&item.id, |entry| entry.value = text);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => this.toast(Status::Error, err.error.message, cx),
                    None => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    // Trees

    /// Run `f` on the root of a tree.
    pub(super) fn with_tree<R>(
        &self,
        source: &TreeSource,
        f: impl FnOnce(&DataNode) -> R,
    ) -> Option<R> {
        match source {
            TreeSource::Explorer => Some(self.store.read(|state| f(&state.explorer.root))),
            TreeSource::Result(id) => self.ui.results.get(id).map(|tree| f(&tree.root)),
        }
    }

    pub(super) fn tree_selected(&self, source: &TreeSource) -> Option<String> {
        match source {
            TreeSource::Explorer => self.store.read(|state| state.explorer.selected.clone()),
            TreeSource::Result(id) => self
                .ui
                .results
                .get(id)
                .and_then(|tree| tree.selected.clone()),
        }
    }

    pub(super) fn select_tree_node(
        &mut self,
        source: &TreeSource,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        match source {
            TreeSource::Explorer => {
                let id = id.to_owned();
                self.store
                    .update(|state| state.touch_explorer(|explorer| explorer.selected = Some(id)));
            }
            TreeSource::Result(run) => {
                if let Some(tree) = self.ui.results.get_mut(run) {
                    tree.selected = Some(id.to_owned());
                }
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_tree_node(
        &mut self,
        source: &TreeSource,
        id: &str,
        expand: bool,
        kind: DataNodeKind,
        cx: &mut Context<Self>,
    ) {
        match source {
            TreeSource::Explorer => {
                let loaded = self.store.read(|state| {
                    find_node(&state.explorer.root, id).is_some_and(|node| node.loaded)
                });
                if expand
                    && !loaded
                    && self.connected()
                    && !matches!(kind, DataNodeKind::Leaf | DataNodeKind::LeafList)
                {
                    self.controller.load_node_later(id);
                    return;
                }
                let id = id.to_owned();
                self.store.update(|state| {
                    state.touch_explorer(|explorer| {
                        if expand {
                            explorer.expanded.insert(id);
                        } else {
                            explorer.expanded.remove(&id);
                        }
                    })
                });
            }
            TreeSource::Result(run) => {
                if let Some(tree) = self.ui.results.get_mut(run) {
                    if expand {
                        tree.expanded.insert(id.to_owned());
                    } else {
                        tree.expanded.remove(id);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Double-click or Enter on a leaf: the explorer runs a Get of it.
    pub(super) fn activate_tree_node(
        &mut self,
        source: &TreeSource,
        id: &str,
        cx: &mut Context<Self>,
    ) {
        if *source == TreeSource::Explorer && self.connected() {
            self.node_get(source, id, cx);
        }
    }

    pub(super) fn node_get(&mut self, source: &TreeSource, id: &str, cx: &mut Context<Self>) {
        self.node_action(
            NodeAction {
                kind: NodeActionKind::Get,
                source: source.clone(),
                node_id: id.to_owned(),
            },
            cx,
        );
    }

    pub(super) fn node_watch(&mut self, source: &TreeSource, id: &str, cx: &mut Context<Self>) {
        self.node_action(
            NodeAction {
                kind: NodeActionKind::Watch,
                source: source.clone(),
                node_id: id.to_owned(),
            },
            cx,
        );
    }

    /// `nodeActions`: what the node menu and the row buttons do.
    pub(super) fn node_action(&mut self, action: NodeAction, cx: &mut Context<Self>) {
        let Some(root) = self.with_tree(&action.source, |root| root.clone()) else {
            return;
        };
        let Some(target) = find_node(&root, &action.node_id).cloned() else {
            return;
        };
        let (gnmi_draft, netconf_draft) = self.store.read(|state| match &state.draft {
            ManagementDraft::Gnmi(draft) => (draft.clone(), default_netconf_draft()),
            ManagementDraft::Netconf(draft) => (default_gnmi_draft(), draft.clone()),
        });
        let has_candidate = self.controller.has_capability(":candidate");
        let gnmi = self.is_gnmi();
        let replace_and_run = |this: &mut Self, draft: ManagementDraft, run: bool| {
            this.store.update(|state| state.draft = draft.clone());
            if run {
                this.controller.run(Some(draft), false);
            }
        };
        match action.kind {
            NodeActionKind::Get if gnmi => replace_and_run(
                self,
                ManagementDraft::Gnmi(node::gnmi_get(&gnmi_draft, &target)),
                true,
            ),
            NodeActionKind::Get => replace_and_run(
                self,
                ManagementDraft::Netconf(node::netconf_get(&netconf_draft, &root, &target.id)),
                true,
            ),
            NodeActionKind::GetConfig => replace_and_run(
                self,
                ManagementDraft::Netconf(node::netconf_get_config(
                    &netconf_draft,
                    &root,
                    &target.id,
                )),
                true,
            ),
            NodeActionKind::Watch => replace_and_run(
                self,
                ManagementDraft::Gnmi(node::watch(&gnmi_draft, &target)),
                true,
            ),
            NodeActionKind::SetValue => replace_and_run(
                self,
                ManagementDraft::Gnmi(node::set_value(&gnmi_draft, &target)),
                false,
            ),
            NodeActionKind::Remove if gnmi => replace_and_run(
                self,
                ManagementDraft::Gnmi(node::gnmi_remove(&gnmi_draft, &target)),
                false,
            ),
            NodeActionKind::Remove => replace_and_run(
                self,
                ManagementDraft::Netconf(node::netconf_remove(&root, &target.id, has_candidate)),
                false,
            ),
            NodeActionKind::UseAsFilter => replace_and_run(
                self,
                ManagementDraft::Netconf(node::use_as_filter(&netconf_draft, &root, &target.id)),
                false,
            ),
            NodeActionKind::EditConfig => self.edit_config(root, target, has_candidate, cx),
            NodeActionKind::CopyPath => {
                let (text, what) = node::copy_path(gnmi, &root, &target);
                self.copy(text, what, cx);
            }
            NodeActionKind::CopyFilter => {
                self.copy(
                    node::filter_for(&root, &target.id),
                    "the subtree filter",
                    cx,
                );
            }
            NodeActionKind::CopyValue => self.copy(node::copy_value(&target), "the value", cx),
            NodeActionKind::CopyGnmic => {
                if let ManagementProfile::Gnmi(profile) = self.dialed_profile().clone() {
                    self.copy(
                        node::copy_gnmic(&profile, &gnmi_draft, &target),
                        "the gnmic command",
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }

    /// Fetch the node's configuration and open it in an edit-config.
    fn edit_config(
        &mut self,
        root: DataNode,
        target: DataNode,
        has_candidate: bool,
        cx: &mut Context<Self>,
    ) {
        let filter =
            muxus_mgmt::workbench::xml::subtree_filter_for(&node::chain(&root, &target.id), "");
        let request = ManagementDraft::Netconf(NetconfDraft {
            operation: NetconfOperation::GetConfig,
            source: node::writable_target(has_candidate),
            filter_type: FilterType::Subtree,
            filter,
            ..default_netconf_draft()
        });
        let controller = self.controller.clone();
        let work = self.on_runtime(async move { controller.request(&request, None).await });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Some(Ok(ManagementResult::NetconfRpc { xml, .. })) => {
                        let reply = parse_rpc_reply(&xml);
                        if let Some(error) = reply.as_ref().and_then(|reply| reply.errors.first()) {
                            this.toast(
                                Status::Error,
                                error
                                    .message
                                    .clone()
                                    .unwrap_or_else(|| "get-config failed".into()),
                                cx,
                            );
                            return;
                        }
                        let config = reply
                            .and_then(|reply| reply.data)
                            .map(|data| {
                                data.elements()
                                    .map(|child| pretty_xml(&serialize_element(child)))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .unwrap_or_default();
                        let draft = node::netconf_edit(config, &root, &target.id, has_candidate);
                        this.store
                            .update(|state| state.draft = ManagementDraft::Netconf(draft));
                    }
                    Some(Ok(_)) | None => {}
                    Some(Err(err)) => this.toast(Status::Error, err.error.message, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The compare dialog of a run against the previous one of the same request.
    pub(super) fn open_compare(
        &mut self,
        run: &muxus_mgmt::workbench::store::RunRecord,
        cx: &mut Context<Self>,
    ) {
        let previous = self
            .store
            .read(|state| super::logic::results::previous_run(&state.runs, run).cloned());
        let Some(previous) = previous else {
            return;
        };
        let sections = match (&previous.result, &run.result) {
            (
                Some(ManagementResult::GnmiGet {
                    notifications: before,
                }),
                Some(ManagementResult::GnmiGet {
                    notifications: after,
                }),
            ) => {
                let registry = self.store.read(|state| state.explorer.registry.clone());
                let before = super::logic::tree::tree_from_notifications(before, &registry);
                let after = super::logic::tree::tree_from_notifications(after, &registry);
                vec![ReviewSection::new(
                    run.summary.clone(),
                    canonical_json(Some(&muxus_mgmt::workbench::data_tree::node_to_json(
                        &before,
                    ))),
                    canonical_json(Some(&muxus_mgmt::workbench::data_tree::node_to_json(
                        &after,
                    ))),
                    "json",
                    None,
                )]
            }
            (
                Some(ManagementResult::NetconfRpc { xml: before, .. }),
                Some(ManagementResult::NetconfRpc { xml: after, .. }),
            ) => {
                vec![ReviewSection::new(
                    run.label.clone(),
                    Self::data_xml(before),
                    Self::data_xml(after),
                    "xml",
                    None,
                )]
            }
            _ => return,
        };
        self.ui.dialog = Some(Dialog::Review {
            title: "Compare with the previous run".into(),
            description: Some(format!(
                "Left: {}. Right: {}.",
                super::logic::format::time_of(previous.started_at),
                super::logic::format::time_of(run.started_at)
            )),
            sections,
            loading: false,
            error: None,
            apply: None,
            tab: 0,
        });
        cx.notify();
    }
}

/// `JSON.stringify(gnmiMessage(draft), null, 2)`: the request as the backend receives it.
fn request_json(draft: &GnmiDraft) -> String {
    let message = match draft.operation {
        GnmiOperation::Set => {
            let set = gnmi_set_message(draft, None);
            RequestMessage::GnmiSet {
                prefix: set.prefix,
                updates: set.updates,
                replaces: set.replaces,
                deletes: set.deletes,
                commit: set.commit,
            }
            .into_client_message(String::new())
        }
        GnmiOperation::Subscribe => {
            let (request, encoding) = gnmi_subscribe_message(draft);
            muxus_core::protocol::ManagementClientMessage::GnmiSubscribe {
                id: String::new(),
                prefix: request.prefix,
                mode: request.mode,
                encoding,
                updates_only: request.updates_only.then_some(true),
                subscriptions: request.subscriptions,
            }
        }
        GnmiOperation::Get => {
            let (request, encoding) = gnmi_get_message(draft);
            RequestMessage::GnmiGet {
                prefix: request.prefix,
                paths: request.paths,
                data_type: request.data_type,
                encoding,
                depth: request.depth,
                max_bytes: None,
            }
            .into_client_message(String::new())
        }
    };
    let mut value = serde_json::to_value(&message).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.remove("id");
    }
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

#[allow(dead_code)]
fn unused(_: &mut App, _: Option<GnmiProfile>, _: Option<NetconfProfile>, _: &mut DataNode) {
    let _ = find_node_mut;
    let _ = netconf_changes_state;
}
