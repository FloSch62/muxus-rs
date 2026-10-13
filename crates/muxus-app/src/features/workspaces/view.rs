//! The workspace dialog (`client/src/components/WorkspaceDialog.tsx`): the
//! window's current workspace with save, save as, lock and rename; the saved
//! catalog with search, sort, open (here or in a new window), startup pick,
//! lock and delete; and the sessions a restored layout left disconnected.
//!
//! Catalog changes go through the window's [`Workspaces`] runtime. What
//! belongs to the window (new windows, reconnecting tabs) is reported as a
//! [`WorkspaceDialogEvent`].

use std::rc::Rc;

use muxus_core::api::WorkspaceSummary;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Point, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::{MEDIUM, SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::dialog::{dialog_actions, dialog_paper, dialog_title, modal};
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::list::{ListItemButton, list_item_icon, list_item_text};
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::tooltip::AnchorBounds;
use muxus_ui::widgets::typography::{baseline, inline_caption};
use muxus_ui::widgets::{
    Alert, Button, ButtonColor, Checkbox, Chip, IconButton, Select, TextField,
};

use super::list::{WorkspaceSort, activity_label, next_workspace_name, select_workspaces};
use super::model::{Workspaces, WorkspacesState};
use crate::features::launcher::kit::{
    AskAnswer, AskDialog, AskRequest, overline, show_error_toast, show_toast, text, text_weight,
};
use crate::state::tabs::{TabStatus, TerminalTab, is_remote_session_tab};

/// What the dialog reads about its window.
#[derive(Clone, Debug, Default)]
pub struct WorkspaceDialogContext {
    pub tabs: Vec<TerminalTab>,
}

/// What the dialog asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceDialogEvent {
    /// Remove the dialog.
    Close,
    /// Open a new window that saves its layout as a new workspace
    /// (`openAppWindow({ kind: 'workspace', title })`).
    NewWindow { name: String },
    /// Open a saved workspace in a new window.
    OpenInNewWindow { id: String, name: String },
    /// Reconnect these ended tabs.
    Reconnect(Vec<String>),
    /// Reconnect every ended tab.
    ReconnectAll,
    /// `requestForceReconnectAll`: replace every remote connection (the
    /// window confirms first).
    ForceReconnectAll,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NameAction {
    SaveAs,
    NewWindow,
    Rename { id: String, previous_name: String },
}

#[derive(Clone, Debug)]
struct RowMenu {
    workspace: WorkspaceSummary,
    position: Point<Pixels>,
}

/// A confirmation in flight and what confirming does.
#[derive(Clone, Debug)]
enum Pending {
    Restore(WorkspaceSummary),
    Delete(WorkspaceSummary),
}

pub struct WorkspaceDialog {
    workspaces: Entity<Workspaces>,
    context: WorkspaceDialogContext,
    search: Entity<InputState>,
    name_input: Entity<InputState>,
    query: String,
    sort: WorkspaceSort,
    sort_open: bool,
    sort_anchor: AnchorBounds,
    name_action: Option<NameAction>,
    row_menu: Option<RowMenu>,
    pending_open: Option<String>,
    selected_tabs: Vec<String>,
    ask: Option<(Entity<AskDialog>, Pending)>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WorkspaceDialogEvent> for WorkspaceDialog {}

impl Focusable for WorkspaceDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the dialog over the window (`setWorkspacesOpen(true)`).
pub fn open_workspaces(
    workspaces: Entity<Workspaces>,
    context: WorkspaceDialogContext,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WorkspaceDialog> {
    cx.new(|cx| WorkspaceDialog::new(workspaces, context, window, cx))
}

impl WorkspaceDialog {
    pub fn new(
        workspaces: Entity<Workspaces>,
        context: WorkspaceDialogContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx));
        let name_input = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = vec![
            cx.observe(&workspaces, |_, _, cx| cx.notify()),
            cx.subscribe_in(
                &search,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        this.query = this.search.read(cx).value().to_string();
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.search_enter(cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &name_input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.commit_name_action(window, cx),
                    _ => cx.notify(),
                },
            ),
        ];
        // The catalog and the other windows' presence may be stale.
        workspaces.update(cx, |workspaces, cx| {
            if workspaces.state().ready {
                workspaces.request_presence();
                workspaces.refresh_catalog(cx).detach();
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            workspaces,
            context,
            search,
            name_input,
            query: String::new(),
            sort: WorkspaceSort::Recent,
            sort_open: false,
            sort_anchor: AnchorBounds::default(),
            name_action: None,
            row_menu: None,
            pending_open: None,
            selected_tabs: Vec::new(),
            ask: None,
            scroll: ScrollHandle::new(),
            focus,
            _subscriptions: subscriptions,
        }
    }

    /// The window's tabs changed while the dialog is open.
    pub fn set_context(&mut self, context: WorkspaceDialogContext, cx: &mut Context<Self>) {
        self.context = context;
        let alive: Vec<String> = self
            .reconnectable()
            .iter()
            .map(|tab| tab.id.clone())
            .collect();
        self.selected_tabs.retain(|id| alive.contains(id));
        cx.notify();
    }

    fn state<'a>(&self, cx: &'a App) -> &'a WorkspacesState {
        self.workspaces.read(cx).state()
    }

    fn reconnectable(&self) -> Vec<&TerminalTab> {
        self.context
            .tabs
            .iter()
            .filter(|tab| tab.profile.is_some() && tab.status == TabStatus::Closed)
            .collect()
    }

    fn live_count(&self) -> usize {
        self.context
            .tabs
            .iter()
            .filter(|tab| {
                tab.profile.is_some()
                    && matches!(tab.status, TabStatus::Connected | TabStatus::Connecting)
            })
            .count()
    }

    fn visible(&self, cx: &App) -> Vec<WorkspaceSummary> {
        let state = self.state(cx);
        select_workspaces(
            &state.workspaces,
            &self.query,
            self.sort,
            state.active_id.as_deref(),
        )
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(WorkspaceDialogEvent::Close);
    }

    fn set_query(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let value = query.to_owned();
        self.search
            .update(cx, |state, cx| state.set_value(value, window, cx));
        self.query = query.to_owned();
        cx.notify();
    }

    fn search_enter(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible(cx);
        let active = self.state(cx).active_id.clone();
        if let [only] = visible.as_slice()
            && Some(&only.id) != active.as_ref()
        {
            let id = only.id.clone();
            self.request_open(&id, cx);
        }
    }

    fn begin_name_action(
        &mut self,
        action: NameAction,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.row_menu = None;
        self.name_action = Some(action);
        self.name_input.update(cx, |state, cx| {
            state.set_value(value, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn begin_save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = format!("{} copy", self.state(cx).active_name);
        self.begin_name_action(NameAction::SaveAs, name, window, cx);
    }

    fn begin_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = next_workspace_name(&self.state(cx).workspaces);
        self.begin_name_action(NameAction::NewWindow, name, window, cx);
    }

    fn begin_rename(
        &mut self,
        workspace: &WorkspaceSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_name_action(
            NameAction::Rename {
                id: workspace.id.clone(),
                previous_name: workspace.name.clone(),
            },
            workspace.name.clone(),
            window,
            cx,
        );
    }

    fn name_value(&self, cx: &App) -> String {
        muxus_core::js::js_trim(&self.name_input.read(cx).value()).to_owned()
    }

    fn commit_name_action(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let trimmed = self.name_value(cx);
        let Some(action) = self.name_action.clone() else {
            return;
        };
        if trimmed.is_empty() {
            return;
        }
        match action {
            NameAction::NewWindow => {
                cx.emit(WorkspaceDialogEvent::NewWindow {
                    name: trimmed.clone(),
                });
                self.name_action = None;
                self.close(cx);
                show_toast(
                    Status::Info,
                    format!("Creating workspace “{trimmed}” in a new window."),
                    cx,
                );
            }
            NameAction::SaveAs => {
                let task = self
                    .workspaces
                    .update(cx, |workspaces, cx| workspaces.save_as(&trimmed, cx));
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(_) => {
                            show_toast(
                                Status::Success,
                                format!("Saved workspace “{trimmed}”."),
                                cx,
                            );
                            this.name_action = None;
                            cx.notify();
                        }
                        Err(error) => show_error_toast(error, cx),
                    });
                })
                .detach();
            }
            NameAction::Rename { id, .. } => {
                let task = self
                    .workspaces
                    .update(cx, |workspaces, cx| workspaces.rename(&id, &trimmed, cx));
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(_) => {
                            show_toast(
                                Status::Success,
                                format!("Renamed workspace to “{trimmed}”."),
                                cx,
                            );
                            this.name_action = None;
                            cx.notify();
                        }
                        Err(error) => show_error_toast(error, cx),
                    });
                })
                .detach();
            }
        }
    }

    fn perform_save(&mut self, cx: &mut Context<Self>) {
        let task = self
            .workspaces
            .update(cx, |workspaces, cx| workspaces.save(cx));
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| match result {
                Ok(workspace) => show_toast(
                    Status::Success,
                    format!("Saved workspace “{}”.", workspace.name),
                    cx,
                ),
                Err(error) => show_error_toast(error, cx),
            });
        })
        .detach();
    }

    fn perform_open(&mut self, id: &str, cx: &mut Context<Self>) {
        let task = self
            .workspaces
            .update(cx, |workspaces, cx| workspaces.open(id, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(workspace) => {
                    this.pending_open = None;
                    this.selected_tabs.clear();
                    show_toast(
                        Status::Success,
                        format!("Opened workspace “{}”.", workspace.name),
                        cx,
                    );
                    cx.notify();
                }
                Err(error) => show_error_toast(error, cx),
            });
        })
        .detach();
    }

    fn request_open(&mut self, id: &str, cx: &mut Context<Self>) {
        self.row_menu = None;
        let state = self.state(cx).clone();
        if state.active_id.as_deref() == Some(id) {
            return;
        }
        let workspace = state
            .workspaces
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned();
        if let Some(workspace) = &workspace
            && self.workspaces.read(cx).focus_open_workspace(id)
        {
            self.close(cx);
            show_toast(
                Status::Info,
                format!("Switched to the window showing “{}”.", workspace.name),
                cx,
            );
            return;
        }
        if self.live_count() > 0 {
            self.pending_open = Some(id.to_owned());
            cx.notify();
        } else {
            self.perform_open(id, cx);
        }
    }

    fn open_in_new_window(&mut self, workspace: &WorkspaceSummary, cx: &mut Context<Self>) {
        self.row_menu = None;
        if self.workspaces.read(cx).focus_open_workspace(&workspace.id) {
            self.close(cx);
            show_toast(
                Status::Info,
                format!("Switched to the window showing “{}”.", workspace.name),
                cx,
            );
            return;
        }
        cx.emit(WorkspaceDialogEvent::OpenInNewWindow {
            id: workspace.id.clone(),
            name: workspace.name.clone(),
        });
        self.close(cx);
    }

    fn ask(
        &mut self,
        request: AskRequest,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| AskDialog::new(request, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, answer: &AskAnswer, window, cx| {
                let Some((_, pending)) = this.ask.take() else {
                    return;
                };
                window.focus(&this.focus, cx);
                if *answer == AskAnswer::Confirmed {
                    this.confirmed(pending, cx);
                }
                cx.notify();
            },
        )
        .detach();
        self.ask = Some((dialog, pending));
        cx.notify();
    }

    fn confirmed(&mut self, pending: Pending, cx: &mut Context<Self>) {
        match pending {
            Pending::Restore(workspace) => {
                let task = self
                    .workspaces
                    .update(cx, |workspaces, cx| workspaces.open(&workspace.id, cx));
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(restored) => {
                            this.pending_open = None;
                            this.selected_tabs.clear();
                            show_toast(
                                Status::Success,
                                format!("Restored workspace “{}”.", restored.name),
                                cx,
                            );
                            cx.notify();
                        }
                        Err(error) => show_error_toast(error, cx),
                    });
                })
                .detach();
            }
            Pending::Delete(workspace) => {
                let was_active = self.state(cx).active_id.as_deref() == Some(workspace.id.as_str());
                let task = self
                    .workspaces
                    .update(cx, |workspaces, cx| workspaces.delete(&workspace.id, cx));
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(()) => {
                            if matches!(&this.name_action, Some(NameAction::Rename { id, .. }) if *id == workspace.id)
                            {
                                this.name_action = None;
                            }
                            show_toast(
                                Status::Success,
                                if was_active {
                                    format!(
                                        "Deleted workspace “{}”. The current layout is now unsaved.",
                                        workspace.name
                                    )
                                } else {
                                    format!("Deleted workspace “{}”.", workspace.name)
                                },
                                cx,
                            );
                            cx.notify();
                        }
                        Err(error) => show_error_toast(error, cx),
                    });
                })
                .detach();
            }
        }
    }

    fn request_restore(
        &mut self,
        workspace: &WorkspaceSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.row_menu = None;
        let live = self.live_count();
        let description = if live > 0 {
            format!(
                "This ends {live} live or connecting session{} and replaces the current layout with the locked saved layout.",
                if live == 1 { "" } else { "s" }
            )
        } else {
            "This replaces the current layout with the locked saved layout.".to_owned()
        };
        self.ask(
            AskRequest::confirm(
                format!("Restore “{}” to its saved state?", workspace.name),
                description,
                "Restore",
            ),
            Pending::Restore(workspace.clone()),
            window,
            cx,
        );
    }

    fn request_delete(
        &mut self,
        workspace: &WorkspaceSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.row_menu = None;
        self.pending_open = None;
        let state = self.state(cx);
        let others = state.other_window_count(&workspace.id);
        let description = if others > 0 {
            format!(
                "This workspace is open in {}. Its saved layout will be removed; sessions in every window stay open as an unsaved workspace.",
                if others == 1 {
                    "another window".to_owned()
                } else {
                    format!("{others} other windows")
                }
            )
        } else if state.active_id.as_deref() == Some(workspace.id.as_str()) {
            "The saved layout is removed and the current layout becomes unsaved. Open sessions keep running.".to_owned()
        } else {
            "The saved layout and its multi-execution groups are removed. This cannot be undone."
                .to_owned()
        };
        self.ask(
            AskRequest::confirm(
                format!("Delete workspace “{}”?", workspace.name),
                description,
                "Delete",
            )
            .destructive(),
            Pending::Delete(workspace.clone()),
            window,
            cx,
        );
    }

    fn toggle_startup(&mut self, id: &str, cx: &mut Context<Self>) {
        self.row_menu = None;
        let clearing = self.state(cx).startup_id.as_deref() == Some(id);
        let task = self.workspaces.update(cx, |workspaces, cx| {
            workspaces.set_startup(if clearing { None } else { Some(id) }, cx)
        });
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| match result {
                Ok(()) => show_toast(
                    Status::Success,
                    if clearing {
                        "Muxus will reopen the most recently used workspace."
                    } else {
                        "Startup workspace updated."
                    },
                    cx,
                ),
                Err(error) => show_error_toast(error, cx),
            });
        })
        .detach();
    }

    fn toggle_locked(&mut self, workspace: &WorkspaceSummary, cx: &mut Context<Self>) {
        self.row_menu = None;
        let task = self.workspaces.update(cx, |workspaces, cx| {
            workspaces.set_locked(&workspace.id, !workspace.is_locked, cx)
        });
        cx.spawn(async move |_, cx| {
            let result = task.await;
            cx.update(|cx| match result {
                Ok(updated) => show_toast(
                    Status::Success,
                    if updated.is_locked {
                        format!(
                            "Locked workspace “{}”. Changes now require an explicit save.",
                            updated.name
                        )
                    } else {
                        format!(
                            "Unlocked workspace “{}”. Changes will save automatically.",
                            updated.name
                        )
                    },
                    cx,
                ),
                Err(error) => show_error_toast(error, cx),
            });
        })
        .detach();
    }

    fn reconnect(&mut self, cx: &mut Context<Self>) {
        let count = if self.selected_tabs.is_empty() {
            self.reconnectable().len()
        } else {
            self.selected_tabs.len()
        };
        if self.selected_tabs.is_empty() {
            cx.emit(WorkspaceDialogEvent::ReconnectAll);
        } else {
            cx.emit(WorkspaceDialogEvent::Reconnect(self.selected_tabs.clone()));
        }
        show_toast(
            Status::Info,
            format!(
                "Reconnecting {count} session{}…",
                if count == 1 { "" } else { "s" }
            ),
            cx,
        );
        self.selected_tabs.clear();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.ask.is_none() {
            cx.stop_propagation();
            if self.row_menu.take().is_some() || std::mem::take(&mut self.sort_open) {
                cx.notify();
            } else {
                self.close(cx);
            }
        }
    }

    fn on_search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && !self.query.is_empty() {
            cx.stop_propagation();
            self.set_query("", window, cx);
        }
    }

    fn on_name_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.name_action = None;
            cx.notify();
        }
    }
}

/// A small text button with a start icon (`Button size="small" startIcon`),
/// optionally behind a tooltip.
fn header_button(
    id: &'static str,
    label: &str,
    icon_name: &str,
    disabled: bool,
    tooltip: Option<&str>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let mut button = Button::new(id, label.to_owned())
        .start_icon(icon_name.to_owned())
        .disabled(disabled)
        .on_click(on_click);
    if let Some(tooltip) = tooltip {
        button = button.tooltip(tooltip.to_owned());
    }
    button.into_any_element()
}

/// The chips after a workspace's name.
fn row_chips(
    workspace: &WorkspaceSummary,
    active: bool,
    startup: bool,
    others: usize,
    theme: &Theme,
) -> Vec<AnyElement> {
    let p = &theme.palette;
    let mut chips = Vec::new();
    if active {
        chips.push(
            Chip::new("Open")
                .outlined()
                .palette(p.primary)
                .into_any_element(),
        );
    }
    if startup {
        chips.push(
            Chip::new("Startup")
                .outlined()
                .palette(p.warning)
                .icon("Star")
                .into_any_element(),
        );
    }
    if workspace.is_locked {
        chips.push(
            Chip::new("Locked")
                .outlined()
                .icon("LockOutlined")
                .into_any_element(),
        );
    }
    if others > 0 {
        let label = if others == 1 {
            "Open in another window".to_owned()
        } else {
            format!("Open in {others} other windows")
        };
        chips.push(
            Chip::new(label)
                .outlined()
                .palette(p.info)
                .icon("OpenInNew")
                .into_any_element(),
        );
    }
    chips
}

impl WorkspaceDialog {
    /// The current workspace's row and its buttons.
    fn current_section(
        &self,
        state: &WorkspacesState,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = &theme.palette;
        let t = &theme.typography;
        let weak = cx.entity().downgrade();
        let busy = state.busy;
        let active = state.active().cloned();
        let locked = active.as_ref().is_some_and(|workspace| workspace.is_locked);
        let mut buttons = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(Theme::spacing(1.));
        {
            let w = weak.clone();
            buttons = buttons.child(
                Button::new("ws-new", "New workspace")
                    .contained()
                    .start_icon("Add")
                    .disabled(!state.ready || busy)
                    .on_click(move |_, window, cx| {
                        let _ = w.update(cx, |this, cx| this.begin_new_workspace(window, cx));
                    }),
            );
        }
        {
            let w = weak.clone();
            buttons = buttons.child(header_button(
                "ws-save",
                "Save",
                "SaveOutlined",
                active.is_none() || busy,
                None,
                move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.perform_save(cx));
                },
            ));
        }
        {
            let w = weak.clone();
            buttons = buttons.child(header_button(
                "ws-save-as",
                "Save as",
                "SaveAsOutlined",
                !state.ready || busy,
                None,
                move |_, window, cx| {
                    let _ = w.update(cx, |this, cx| this.begin_save_as(window, cx));
                },
            ));
        }
        if let (true, Some(workspace)) = (locked, active.clone()) {
            let w = weak.clone();
            buttons = buttons.child(header_button(
                "ws-restore",
                "Restore",
                "RestoreOutlined",
                busy,
                Some("Discard changes and reload the locked saved layout"),
                move |_, window, cx| {
                    let _ = w.update(cx, |this, cx| this.request_restore(&workspace, window, cx));
                },
            ));
        }
        {
            let w = weak.clone();
            let workspace = active.clone();
            buttons = buttons.child(header_button(
                "ws-lock",
                if locked { "Unlock" } else { "Lock" },
                if locked {
                    "LockOpenOutlined"
                } else {
                    "LockOutlined"
                },
                active.is_none() || busy,
                Some(if locked {
                    "Resume automatic saves for this workspace"
                } else {
                    "Require an explicit save before this workspace changes"
                }),
                move |_, _, cx| {
                    if let Some(workspace) = &workspace {
                        let _ = w.update(cx, |this, cx| this.toggle_locked(workspace, cx));
                    }
                },
            ));
        }
        {
            let w = weak.clone();
            let workspace = active.clone();
            buttons = buttons.child(header_button(
                "ws-rename",
                "Rename",
                "DriveFileRenameOutline",
                active.is_none() || busy,
                None,
                move |_, window, cx| {
                    if let Some(workspace) = &workspace {
                        let _ = w.update(cx, |this, cx| this.begin_rename(workspace, window, cx));
                    }
                },
            ));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(2.))
            .py(Theme::spacing(1.5))
            .bg(p.action.hover)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    // A caption span: its line is the body1 strut's.
                    .child(inline_caption("Current workspace", p.text.secondary))
                    .child(text(state.active_name.clone(), t.subtitle1, p.text.primary)),
            )
            .child(buttons)
            .into_any_element()
    }

    /// The name field of save as, new workspace and rename.
    fn name_section(&self, action: &NameAction, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        let label = match action {
            NameAction::Rename { previous_name, .. } => format!("Rename “{previous_name}”"),
            _ => "New workspace name".to_owned(),
        };
        let commit = match action {
            NameAction::NewWindow => "Create",
            NameAction::SaveAs => "Save",
            NameAction::Rename { .. } => "Rename",
        };
        let can_commit = !self.name_value(cx).is_empty() && !busy;
        let (w1, w2) = (weak.clone(), weak);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(2.))
            .py(Theme::spacing(1.5))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .capture_key_down(cx.listener(Self::on_name_key))
                    .child(
                        TextField::new("workspace-name", &self.name_input)
                            .label(label)
                            .floating_label()
                            .full_width(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(Theme::spacing(1.))
                    .child(
                        Button::new("ws-name-commit", commit)
                            .contained()
                            .medium()
                            .disabled(!can_commit)
                            .on_click(move |_, window, cx| {
                                let _ =
                                    w1.update(cx, |this, cx| this.commit_name_action(window, cx));
                            }),
                    )
                    .child(Button::new("ws-name-cancel", "Cancel").medium().on_click(
                        move |_, _, cx| {
                            let _ = w2.update(cx, |this, cx| {
                                this.name_action = None;
                                cx.notify();
                            });
                        },
                    )),
            )
            .into_any_element()
    }

    /// One saved workspace.
    fn catalog_row(
        &self,
        index: usize,
        workspace: &WorkspaceSummary,
        state: &WorkspacesState,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> ListItemButton {
        let p = &theme.palette;
        let t = &theme.typography;
        let weak = cx.entity().downgrade();
        let busy = state.busy;
        let is_active = state.active_id.as_deref() == Some(workspace.id.as_str());
        let is_startup = state.startup_id.as_deref() == Some(workspace.id.as_str());
        let others = state.other_window_count(&workspace.id);
        let primary = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(0.75))
            .min_w_0()
            .child(text_weight(
                workspace.name.clone(),
                t.body2,
                if is_active { 600. } else { t.body2.weight },
                p.text.primary,
            ))
            .children(row_chips(workspace, is_active, is_startup, others, theme));
        let open_id = workspace.id.clone();
        let (w_open, w_menu, w_new, w_right) = (weak.clone(), weak.clone(), weak.clone(), weak);
        let menu_workspace = workspace.clone();
        let right_workspace = workspace.clone();
        let new_window_workspace = workspace.clone();
        let mut row = ListItemButton::new(("workspace-row", index))
            .selected(is_active)
            .disabled(busy)
            // A dense list's 4px rows.
            .padding(Theme::spacing_css(2.), Theme::spacing_css(0.5))
            .min_height(58.)
            .divider()
            .on_click(move |_, _, cx| {
                let _ = w_open.update(cx, |this, cx| this.request_open(&open_id, cx));
            })
            .on_right_click(move |position, _, cx| {
                let _ = w_right.update(cx, |this, cx| {
                    if this.state(cx).busy {
                        return;
                    }
                    this.row_menu = Some(RowMenu {
                        workspace: right_workspace.clone(),
                        position: *position,
                    });
                    cx.notify();
                });
            })
            .child(list_item_icon(
                36.,
                icon(
                    "WorkspacesOutlined",
                    SMALL,
                    if is_active {
                        p.primary.main
                    } else {
                        p.text.primary
                    },
                ),
            ))
            .child(list_item_text(
                primary,
                Some(text(activity_label(workspace), t.body2, p.text.secondary).into_any_element()),
            ));
        if !is_active {
            let tooltip = if others > 0 {
                format!("Switch to the window showing {}", workspace.name)
            } else {
                format!("Open {} in a new window", workspace.name)
            };
            row = row.child(
                IconButton::new(("ws-open-new", index), "OpenInNew")
                    .tooltip(tooltip)
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = w_new.update(cx, |this, cx| {
                            this.open_in_new_window(&new_window_workspace, cx)
                        });
                    }),
            );
        }
        let menu_anchor = AnchorBounds::default();
        let anchor = menu_anchor.clone();
        row.child(
            IconButton::new(("ws-actions", index), "MoreVert")
                .tooltip(format!("Actions for {}", workspace.name))
                .track_bounds(&menu_anchor)
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    let position = menu::below(&anchor).unwrap_or_default();
                    let _ = w_menu.update(cx, |this, cx| {
                        this.row_menu = Some(RowMenu {
                            workspace: menu_workspace.clone(),
                            position,
                        });
                        cx.notify();
                    });
                }),
        )
    }

    /// The ⋮ menu (or right-click menu) of a workspace.
    fn row_menu_popup(
        &self,
        row_menu: &RowMenu,
        state: &WorkspacesState,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak = cx.entity().downgrade();
        let busy = state.busy;
        let workspace = row_menu.workspace.clone();
        let is_active = state.active_id.as_deref() == Some(workspace.id.as_str());
        let elsewhere = state.other_window_count(&workspace.id) > 0;
        let is_startup = state.startup_id.as_deref() == Some(workspace.id.as_str());
        let mut items = menu::paper(theme);
        if !is_active && !elsewhere {
            let (w, id) = (weak.clone(), workspace.id.clone());
            items = items.child(
                MenuItem::new("ws-menu-open", "Open")
                    .icon("PlayArrowOutlined")
                    .on_click(move |_, _, cx| {
                        let _ = w.update(cx, |this, cx| this.request_open(&id, cx));
                    }),
            );
        }
        if !is_active {
            let (w, target) = (weak.clone(), workspace.clone());
            items = items.child(
                MenuItem::new(
                    "ws-menu-new-window",
                    if elsewhere {
                        "Switch to open window"
                    } else {
                        "Open in new window"
                    },
                )
                .icon("OpenInNew")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.open_in_new_window(&target, cx));
                }),
            );
        }
        {
            let (w, target) = (weak.clone(), workspace.clone());
            items = items.child(
                MenuItem::new("ws-menu-rename", "Rename")
                    .icon("DriveFileRenameOutline")
                    .disabled(busy)
                    .on_click(move |_, window, cx| {
                        let _ = w.update(cx, |this, cx| this.begin_rename(&target, window, cx));
                    }),
            );
        }
        {
            let (w, id) = (weak.clone(), workspace.id.clone());
            items = items.child(
                MenuItem::new(
                    "ws-menu-startup",
                    if is_startup {
                        "Clear startup workspace"
                    } else {
                        "Open at startup"
                    },
                )
                .icon(if is_startup { "StarBorder" } else { "Star" })
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.toggle_startup(&id, cx));
                }),
            );
        }
        {
            let (w, target) = (weak.clone(), workspace.clone());
            items = items.child(
                MenuItem::new(
                    "ws-menu-lock",
                    if workspace.is_locked {
                        "Unlock"
                    } else {
                        "Lock"
                    },
                )
                .icon(if workspace.is_locked {
                    "LockOpenOutlined"
                } else {
                    "LockOutlined"
                })
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.toggle_locked(&target, cx));
                }),
            );
        }
        items = items.child(menu_divider(theme));
        {
            let (w, target) = (weak.clone(), workspace.clone());
            items = items.child(
                MenuItem::new("ws-menu-delete", "Delete")
                    .icon("DeleteOutlined")
                    .destructive()
                    .disabled(busy)
                    .on_click(move |_, window, cx| {
                        let _ = w.update(cx, |this, cx| this.request_delete(&target, window, cx));
                    }),
            );
        }
        let w = weak;
        menu::popup(
            row_menu.position,
            move |_, cx| {
                let _ = w.update(cx, |this, cx| {
                    this.row_menu = None;
                    cx.notify();
                });
            },
            items,
        )
        .into_any_element()
    }
}

impl Render for WorkspaceDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        let state = self.state(cx).clone();
        let busy = state.busy;
        let visible = self.visible(cx);
        let reconnectable: Vec<TerminalTab> = self.reconnectable().into_iter().cloned().collect();
        let has_live_remote = self
            .context
            .tabs
            .iter()
            .any(|tab| is_remote_session_tab(tab) && tab.status != TabStatus::Closed);
        let live = self.live_count();
        let weak = cx.entity().downgrade();

        let title = dialog_title(&theme)
            .gap(Theme::spacing(1.))
            .child(icon("WorkspacesOutlined", MEDIUM, p.primary.main))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(text("Workspaces", t.h6, p.text.primary)),
            )
            .child(Chip::new(format!("{} saved", state.workspaces.len())).outlined());

        let mut content = div()
            .id("workspaces-content")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .border_t_1()
            .border_b_1()
            .border_color(p.divider);
        if let Some(error) = &state.error {
            content = content.child(Alert::new(Status::Warning, error.clone()).square());
        }
        let active_others = state
            .active_id
            .as_deref()
            .map(|id| state.other_window_count(id))
            .unwrap_or(0);
        if active_others > 0 {
            content = content.child(
                Alert::new(
                    Status::Warning,
                    format!(
                        "“{}” is also open in {active_others} {}. Continue in one window to avoid competing saves.",
                        state.active_name,
                        if active_others == 1 { "other window" } else { "other windows" }
                    ),
                )
                .square(),
            );
        }
        content = content.child(self.current_section(&state, &theme, cx));
        if let Some(action) = self.name_action.clone() {
            content = content.child(self.name_section(&action, busy, cx));
        }
        content = content.child(divider(&theme));

        // Search and sort.
        let mut search = TextField::new("workspace-search", &self.search)
            .placeholder("Search workspaces")
            .start(icon("Search", SMALL, p.action.active))
            .full_width();
        if !self.query.is_empty() {
            let w = weak.clone();
            search = search.end(
                IconButton::new("ws-clear-search", "Clear")
                    .tooltip("Clear search")
                    .edge_end()
                    .on_click(move |_, window, cx| {
                        let _ = w.update(cx, |this, cx| this.set_query("", window, cx));
                    }),
            );
        }
        let sort_toggle = weak.clone();
        content = content.child(
            div()
                .flex()
                .flex_none()
                .gap(Theme::spacing(1.))
                .px(Theme::spacing(2.))
                .pt(Theme::spacing(1.5))
                .pb(Theme::spacing(1.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .capture_key_down(cx.listener(Self::on_search_key))
                        .child(search),
                )
                .child(
                    Select::new("workspace-sort", self.sort.label())
                        .label("Sort")
                        .open(self.sort_open)
                        .width(170.)
                        .track_bounds(&self.sort_anchor)
                        .on_click(move |_, _, cx| {
                            let _ = sort_toggle.update(cx, |this, cx| {
                                this.sort_open = !this.sort_open;
                                cx.notify();
                            });
                        }),
                ),
        );

        content = content.child(
            div()
                .flex()
                .flex_none()
                .items_start()
                .justify_between()
                .px(Theme::spacing(2.))
                .pb(Theme::spacing(0.75))
                .child(overline("Saved workspaces", &theme, p.text.secondary))
                // `alignItems: 'baseline'`: the count sits on the overline's baseline.
                .child(
                    div()
                        .mt(px(baseline(t.overline) - baseline(t.caption)))
                        .child(text(
                            if muxus_core::js::js_trim(&self.query).is_empty() {
                                state.workspaces.len().to_string()
                            } else {
                                format!("{} of {}", visible.len(), state.workspaces.len())
                            },
                            t.caption,
                            p.text.secondary,
                        )),
                ),
        );

        // The catalog.
        let vh = muxus_ui::zoom::unzoom(window.viewport_size().height);
        let mut list = div()
            .id("workspace-list")
            .flex()
            .flex_col()
            .flex_none()
            .max_h(px(440f32.min(vh * 0.48)))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .border_t_1()
            .border_color(p.divider);
        for (index, workspace) in visible.iter().enumerate() {
            list = list.child(self.catalog_row(index, workspace, &state, &theme, cx));
        }
        if !visible.is_empty() {
            // The last row's bottom margin, kept inside the scrolling list.
            list = list.child(div().h(px(1.)).flex_none());
        }
        let empty_text = |message: String| {
            div()
                .px(Theme::spacing(2.))
                .py(Theme::spacing(3.))
                .flex()
                .justify_center()
                .child(text(message, t.body2, p.text.secondary))
        };
        if !state.ready {
            list = list.child(empty_text("Loading workspaces…".into()));
        } else if state.workspaces.is_empty() {
            list = list.child(empty_text(
                "Save the current layout to create your first named workspace.".into(),
            ));
        } else if visible.is_empty() {
            let w = weak.clone();
            list = list.child(
                div()
                    .px(Theme::spacing(2.))
                    .py(Theme::spacing(3.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(text(
                        format!(
                            "No workspaces match “{}”.",
                            muxus_core::js::js_trim(&self.query)
                        ),
                        t.body2,
                        p.text.secondary,
                    ))
                    .child(div().mt(Theme::spacing(0.5)).child(
                        Button::new("ws-clear-empty", "Clear search").on_click(
                            move |_, window, cx| {
                                let _ = w.update(cx, |this, cx| this.set_query("", window, cx));
                            },
                        ),
                    )),
            );
        }
        content = content.child(list);

        if let Some(pending) = self
            .pending_open
            .as_ref()
            .and_then(|id| {
                state
                    .workspaces
                    .iter()
                    .find(|workspace| &workspace.id == id)
            })
            .cloned()
        {
            let (w1, w2) = (weak.clone(), weak.clone());
            let id = pending.id.clone();
            content = content.child(
                div().mx(Theme::spacing(2.)).mt(Theme::spacing(1.5)).child(
                    Alert::new(
                        Status::Warning,
                        format!(
                            "Open “{}”? This ends {live} live or connecting session{}.",
                            pending.name,
                            if live == 1 { "" } else { "s" }
                        ),
                    )
                    .action(
                        div()
                            .flex()
                            .gap(Theme::spacing(0.5))
                            .child(
                                Button::new("ws-cancel-open", "Cancel")
                                    .color(ButtonColor::Inherit)
                                    .on_click(move |_, _, cx| {
                                        let _ = w1.update(cx, |this, cx| {
                                            this.pending_open = None;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new("ws-confirm-open", "Open")
                                    .contained()
                                    .color(ButtonColor::Warning)
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        let _ =
                                            w2.update(cx, |this, cx| this.perform_open(&id, cx));
                                    }),
                            ),
                    ),
                ),
            );
        }

        if !reconnectable.is_empty() {
            content = content
                .child(div().mt(Theme::spacing(1.5)).child(divider(&theme)))
                .child(
                    div()
                        .px(Theme::spacing(2.))
                        .pt(Theme::spacing(1.25))
                        .child(overline("Reconnect sessions", &theme, p.text.secondary)),
                );
            let mut sessions = div()
                .id("ws-reconnect")
                .flex()
                .flex_col()
                .max_h(px(150.))
                .overflow_y_scroll()
                .pb(Theme::spacing(1.));
            for (index, tab) in reconnectable.iter().enumerate() {
                let checked = self.selected_tabs.contains(&tab.id);
                let id = tab.id.clone();
                let w = weak.clone();
                let detail = match &tab.profile {
                    Some(muxus_core::protocol::SessionProfile::Ssh(ssh)) => ssh.target.clone(),
                    Some(profile) => profile.kind().as_str().to_owned(),
                    None => String::new(),
                };
                sessions = sessions.child(
                    ListItemButton::new(("ws-reconnect-row", index))
                        .padding(Theme::spacing_css(2.), Theme::spacing_css(0.5))
                        .on_click(move |_, _, cx| {
                            let _ = w.update(cx, |this, cx| {
                                if let Some(position) = this
                                    .selected_tabs
                                    .iter()
                                    .position(|selected| *selected == id)
                                {
                                    this.selected_tabs.remove(position);
                                } else {
                                    this.selected_tabs.push(id.clone());
                                }
                                cx.notify();
                            });
                        })
                        .child(
                            div().mr(Theme::spacing(1.)).child(
                                Checkbox::new(("ws-reconnect-check", index), checked)
                                    .small()
                                    .padding(Theme::spacing_css(0.5)),
                            ),
                        )
                        .child(list_item_text(
                            text(tab.title.clone(), t.body2, p.text.primary),
                            Some(text(detail, t.body2, p.text.secondary).into_any_element()),
                        )),
                );
            }
            content = content.child(sessions);
        }

        // Actions.
        let mut actions = dialog_actions();
        if !reconnectable.is_empty() {
            let w = weak.clone();
            actions = actions.child(
                Button::new(
                    "ws-reconnect-button",
                    if self.selected_tabs.is_empty() {
                        "Reconnect all"
                    } else {
                        "Reconnect selected"
                    },
                )
                .start_icon("PlayArrowOutlined")
                .on_click(move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.reconnect(cx));
                }),
            );
        }
        if has_live_remote {
            let w = weak.clone();
            actions = actions.child(
                Button::new("ws-force-reconnect", "Force reconnect all")
                    .color(ButtonColor::Warning)
                    .start_icon("ReplayOutlined")
                    .on_click(move |_, _, cx| {
                        let _ =
                            w.update(cx, |_, cx| cx.emit(WorkspaceDialogEvent::ForceReconnectAll));
                    }),
            );
        }
        let w = weak.clone();
        actions = actions.child(div().flex_1()).child(
            Button::new("ws-done", "Done")
                .contained()
                .on_click(move |_, _, cx| {
                    let _ = w.update(cx, |this, cx| this.close(cx));
                }),
        );

        // `maxWidth="md" fullWidth`, `maxHeight: min(820px, calc(100% - 48px))`.
        let vw = muxus_ui::zoom::unzoom(window.viewport_size().width);
        let paper = dialog_paper(&theme)
            .w(px(900f32.min(vw - 64.)))
            .max_h(px(820f32.min(vh - 48.)))
            .child(title)
            .child(content)
            .child(actions);

        let close = weak.clone();
        let mut root = div()
            .id("workspace-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                paper,
                Some(Rc::new(move |_, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.close(cx));
                })),
            ));

        // The sort menu.
        if self.sort_open
            && let Some(bounds) = self.sort_anchor.get()
        {
            let mut items = menu::paper(&theme).min_w(bounds.size.width);
            for (index, sort) in WorkspaceSort::ALL.iter().enumerate() {
                let sort = *sort;
                let w = weak.clone();
                items = items.child(
                    MenuItem::new(("ws-sort-option", index), sort.label())
                        .selected(sort == self.sort)
                        .on_click(move |_, _, cx| {
                            let _ = w.update(cx, |this, cx| {
                                this.sort = sort;
                                this.sort_open = false;
                                cx.notify();
                            });
                        }),
                );
            }
            let w = weak.clone();
            root = root.child(menu::popup(
                bounds.bottom_left(),
                move |_, cx| {
                    let _ = w.update(cx, |this, cx| {
                        this.sort_open = false;
                        cx.notify();
                    });
                },
                items,
            ));
        }

        if let Some(row_menu) = self.row_menu.clone() {
            root = root.child(self.row_menu_popup(&row_menu, &state, &theme, cx));
        }
        if let Some((ask, _)) = &self.ask {
            root = root.child(ask.clone());
        }
        root
    }
}
