//! The settings window: `client/src/layout/SettingsWindow.tsx` around
//! `SettingsBody` of `SettingsDialog.tsx`. A title bar that is the window's
//! drag region, the section list on the left, the open section on the right.
//! Every change applies live through [`SettingsPrefs`], except session
//! logging, which saves explicitly and holds the window while it has
//! unsaved edits.

use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::time::Duration;

use muxus_core::api::{
    AppInfo, PasswordVaultStatus, SessionHistoryStorageStatus, SessionLogFileStatus,
    SessionLoggingPolicy, X11Status,
};
use muxus_session::{ApiResult, Backend};
use muxus_store::prefs::PrefsState;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div,
    point, px, relative,
};
use muxus_ui::icon::icon;
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts, layout};
use muxus_ui::widgets::menu::{below, paper as menu_paper, popup};
use muxus_ui::widgets::{
    Alert, AnchorBounds, Button, ButtonColor, Dialog, DialogWidth, IconButton, MenuItem, Tooltip,
};

use super::super::prefs_model::{PrefsChanged, SettingsPrefs};
use super::super::section::{Navigation, SettingsNavigation, SettingsSection, SettingsTarget};
use super::kit::{BODY1, snap};
use super::shortcuts::ShortcutsDialog;
use muxus_ui::widgets::window_frame;

/// One choice of an open select menu.
#[derive(Clone)]
pub enum MenuEntry {
    /// `ListSubheader`.
    Header(SharedString),
    Item {
        value: String,
        label: SharedString,
        /// A second line (`ListItemText secondary`).
        secondary: Option<SharedString>,
        /// Drawn instead of the label (scheme previews).
        rich: Option<RichLabel>,
    },
}

impl MenuEntry {
    pub fn item(value: impl Into<String>, label: impl Into<SharedString>) -> Self {
        MenuEntry::Item {
            value: value.into(),
            label: label.into(),
            secondary: None,
            rich: None,
        }
    }
}

/// Draws a menu option (scheme previews).
pub type RichLabel = Rc<dyn Fn(&Theme) -> AnyElement>;
/// A slider drag's new position, 0–1.
pub type DragHandler = Rc<dyn Fn(&mut SettingsWindow, f32, &mut Context<SettingsWindow>)>;
/// A two-dimensional drag's new position.
pub type Drag2dHandler = Rc<dyn Fn(&mut SettingsWindow, f32, f32, &mut Context<SettingsWindow>)>;

pub type PickHandler =
    Rc<dyn Fn(&mut SettingsWindow, String, &mut Window, &mut Context<SettingsWindow>)>;

/// A select or button menu that is open.
pub struct OpenMenu {
    pub key: String,
    pub anchor: AnchorBounds,
    pub min_width: f32,
    pub max_height: Option<f32>,
    pub entries: Vec<MenuEntry>,
    pub selected: Option<String>,
    pub on_pick: PickHandler,
}

/// A slider being dragged.
pub struct SliderDrag {
    /// A two-dimensional drag (the color square) also gets the y fraction.
    pub on_move_2d: Option<Drag2dHandler>,
    pub anchor: AnchorBounds,
    pub on_move: DragHandler,
}

pub type InputHandler =
    Rc<dyn Fn(&mut SettingsWindow, String, &mut Window, &mut Context<SettingsWindow>)>;

pub type ConfirmHandler =
    Rc<dyn Fn(&mut SettingsWindow, &mut Window, &mut Context<SettingsWindow>)>;

/// `confirmAction`'s options.
pub struct Confirm {
    pub title: String,
    pub description: String,
    pub confirm_label: String,
    pub destructive: bool,
    pub on_confirm: ConfirmHandler,
}

#[derive(Clone)]
pub struct Toast {
    pub id: u64,
    pub status: Status,
    pub message: SharedString,
}

/// A value read from the backend: not yet, failed, or there.
#[derive(Clone, Debug, Default)]
pub enum Load<T> {
    #[default]
    Pending,
    Failed(String),
    Ready(T),
}

impl<T> Load<T> {
    pub fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// What the sections read from the backend.
#[derive(Default)]
pub struct SettingsData {
    pub app_info: Option<AppInfo>,
    pub x11: Option<X11Status>,
    pub vault: Load<PasswordVaultStatus>,
    pub logging_default: Option<SessionLoggingPolicy>,
    pub logging_local: Option<SessionLoggingPolicy>,
    pub history_storage: Option<SessionHistoryStorageStatus>,
    pub log_files: Option<SessionLogFileStatus>,
    /// A fixed X11 status (previews and tests); not refetched.
    pub x11_pinned: bool,
}

/// The settings window's view.
pub struct SettingsWindow {
    pub self_handle: Option<muxus_ui::gpui_kit::WeakEntity<SettingsWindow>>,
    /// A value the backend revealed, for the input under the key (set at render).
    pub pending_reveal: Option<(String, String)>,
    /// A masked input to show or hide (set at render).
    pub pending_mask: Option<(String, bool)>,
    pub prefs: Entity<SettingsPrefs>,
    pub backend: Backend,
    pub nav: SettingsNavigation,
    pub scroll: ScrollHandle,
    pub data: SettingsData,
    pub menu: Option<OpenMenu>,
    pub drag: Option<SliderDrag>,
    pub confirm: Option<Confirm>,
    pub toast: Option<Toast>,
    pub shortcuts: Option<Entity<ShortcutsDialog>>,
    pub state: super::state::SectionState,
    anchors: HashMap<String, AnchorBounds>,
    inputs: HashMap<String, Entity<InputState>>,
    textareas: HashMap<String, Entity<TextareaState>>,
    input_blur: HashMap<String, InputHandler>,
    input_enter: HashMap<String, InputHandler>,
    toast_seq: u64,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsWindow {
    pub fn new(
        prefs: Entity<SettingsPrefs>,
        target: Option<SettingsTarget>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let backend = prefs.read(cx).backend().clone();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let subscriptions = vec![
            cx.subscribe_in(&prefs, window, Self::on_prefs_changed),
            cx.observe_window_appearance(window, |this, _, cx| {
                let prefs = this.prefs.clone();
                prefs.update(cx, |model, cx| model.apply_theme(cx));
            }),
        ];
        let mut this = Self {
            self_handle: Some(cx.weak_entity()),
            pending_reveal: None,
            pending_mask: None,
            prefs,
            backend,
            nav: SettingsNavigation::new(target),
            scroll: ScrollHandle::new(),
            data: SettingsData::default(),
            menu: None,
            drag: None,
            confirm: None,
            toast: None,
            shortcuts: None,
            state: Default::default(),
            anchors: HashMap::new(),
            inputs: HashMap::new(),
            textareas: HashMap::new(),
            input_blur: HashMap::new(),
            input_enter: HashMap::new(),
            toast_seq: 0,
            focus,
            _subscriptions: subscriptions,
        };
        this.load_common(cx);
        this.load_section(cx);
        this
    }

    pub fn focus_handle_ref(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn keep_subscription(&mut self, subscription: Subscription) {
        self._subscriptions.push(subscription);
    }

    pub fn prefs<'a>(&self, cx: &'a App) -> &'a PrefsState {
        self.prefs.read(cx).prefs()
    }

    /// Change the preferences; every window hears about it.
    pub fn set_prefs(&self, cx: &mut App, change: impl FnOnce(&mut PrefsState)) {
        self.prefs.update(cx, |model, cx| model.update(cx, change));
    }

    fn on_prefs_changed(
        &mut self,
        _: &Entity<SettingsPrefs>,
        _: &PrefsChanged,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
    }

    /// Run a backend call and hand its result to `apply` on the view.
    pub fn fetch<T: 'static>(
        &mut self,
        cx: &mut Context<Self>,
        future: impl Future<Output = ApiResult<T>> + 'static,
        apply: impl FnOnce(&mut Self, ApiResult<T>, &mut Context<Self>) + 'static,
    ) {
        cx.spawn(async move |this, cx| {
            let result = future.await;
            let _ = this.update(cx, |this, cx| {
                apply(this, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_common(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move { backend.app_info().await },
            |this, result, _| {
                this.data.app_info = result.ok();
            },
        );
    }

    /// Read what the open section shows (the TypeScript queries refetch
    /// when a section mounts).
    pub fn load_section(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        match self.nav.section {
            SettingsSection::X11 if !self.data.x11_pinned => {
                self.fetch(
                    cx,
                    async move { backend.x11_status().await },
                    |this, result, _| {
                        if let Ok(status) = result
                            && !this.data.x11_pinned
                        {
                            this.data.x11 = Some(status);
                        }
                    },
                );
            }
            SettingsSection::Passwords => self.load_vault(cx),
            SettingsSection::Logging => self.load_logging(cx),
            _ => {}
        }
    }

    pub fn load_vault(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move { backend.vault_status().await },
            |this, result, _| {
                this.data.vault = match result {
                    Ok(status) => Load::Ready(status),
                    Err(error) => Load::Failed(error.to_string()),
                };
            },
        );
    }

    pub fn load_logging(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move {
                let default = backend.session_logging_policy("*").await?;
                let local = backend.session_logging_policy("local").await?;
                let storage = backend.session_history_storage().await?;
                let files = backend.session_log_files().await?;
                Ok((default, local, storage, files))
            },
            |this, result, cx| {
                if let Ok((default, local, storage, files)) = result {
                    this.data.logging_default = Some(default);
                    this.data.logging_local = Some(local);
                    this.data.history_storage = Some(storage);
                    this.data.log_files = Some(files);
                    this.logging_loaded(cx);
                }
            },
        );
    }

    /// Show a fixed X11 status instead of the backend's (previews and tests).
    pub fn pin_x11_status(&mut self, status: X11Status) {
        self.data.x11 = Some(status);
        self.data.x11_pinned = true;
    }

    /// The painted bounds slot for a control (menus and sliders anchor to it).
    pub fn anchor(&mut self, key: &str) -> AnchorBounds {
        self.anchors.entry(key.to_string()).or_default().clone()
    }

    /// A single-line input bound to `key`, created with `initial` the first
    /// time; `on_change` runs with every edit.
    #[allow(clippy::too_many_arguments)]
    pub fn input(
        &mut self,
        key: &str,
        initial: impl FnOnce() -> String,
        placeholder: Option<&str>,
        masked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_change: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        if let Some(state) = self.inputs.get(key) {
            return state.clone();
        }
        let value = initial();
        let placeholder = placeholder.map(str::to_string);
        let state = cx.new(|cx| {
            let mut state = InputState::new(window, cx).masked(masked);
            if let Some(placeholder) = placeholder {
                state = state.placeholder(placeholder);
            }
            state.set_value(value, window, cx);
            state
        });
        let event_key = key.to_string();
        let subscription =
            cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                let value = state.read(cx).value().to_string();
                match event {
                    InputEvent::Change => on_change(this, value, window, cx),
                    InputEvent::Blur => {
                        if let Some(handler) = this.input_blur.get(&event_key).cloned() {
                            handler(this, value, window, cx);
                        }
                    }
                    InputEvent::PressEnter { .. } => {
                        if let Some(handler) = this.input_enter.get(&event_key).cloned() {
                            handler(this, value, window, cx);
                        }
                    }
                    InputEvent::Focus => {}
                }
                cx.notify();
            });
        self._subscriptions.push(subscription);
        self.inputs.insert(key.to_string(), state.clone());
        state
    }

    /// Run `handler` when the input under `key` loses focus.
    pub fn on_input_blur(
        &mut self,
        key: &str,
        handler: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.input_blur.insert(key.to_string(), Rc::new(handler));
    }

    /// Run `handler` when Enter is pressed in the input under `key`.
    pub fn on_input_enter(
        &mut self,
        key: &str,
        handler: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.input_enter.insert(key.to_string(), Rc::new(handler));
    }

    /// A multi-line editor bound to `key` (see [`SettingsWindow::input`]).
    #[allow(clippy::too_many_arguments)]
    pub fn textarea(
        &mut self,
        key: &str,
        initial: impl FnOnce() -> String,
        placeholder: Option<&str>,
        rows: (usize, usize),
        window: &mut Window,
        cx: &mut Context<Self>,
        on_change: impl Fn(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) -> Entity<TextareaState> {
        if let Some(state) = self.textareas.get(key) {
            return state.clone();
        }
        let value = initial();
        let placeholder = placeholder.map(str::to_string);
        let state = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).auto_grow(rows.0, rows.1);
            if let Some(placeholder) = placeholder {
                state = state.placeholder(placeholder);
            }
            state.set_value(value, window, cx);
            state
        });
        let subscription =
            cx.subscribe_in(&state, window, move |this, state, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = state.read(cx).value().to_string();
                    on_change(this, value, window, cx);
                    cx.notify();
                }
            });
        self._subscriptions.push(subscription);
        self.textareas.insert(key.to_string(), state.clone());
        state
    }

    /// Show `value` in an input unless the user is typing in it.
    pub fn sync_input(&mut self, key: &str, value: &str, window: &mut Window, cx: &mut App) {
        if let Some(state) = self.inputs.get(key).cloned() {
            let focused = state.read(cx).focus_handle(cx).is_focused(window);
            if !focused && state.read(cx).value().as_ref() != value {
                let value = value.to_string();
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
        }
    }

    /// Forget inputs whose key starts with `prefix` (a removed list entry).
    pub fn drop_inputs(&mut self, prefix: &str) {
        self.inputs.retain(|key, _| !key.starts_with(prefix));
        self.textareas.retain(|key, _| !key.starts_with(prefix));
    }

    pub fn input_value(&self, key: &str, cx: &App) -> Option<String> {
        self.inputs
            .get(key)
            .map(|state| state.read(cx).value().to_string())
    }

    pub fn input_entity(&self, key: &str) -> Option<Entity<InputState>> {
        self.inputs.get(key).cloned()
    }

    pub fn textarea_value(&self, key: &str, cx: &App) -> Option<String> {
        self.textareas
            .get(key)
            .map(|state| state.read(cx).value().to_string())
    }

    pub fn set_input_value(&self, key: &str, value: &str, window: &mut Window, cx: &mut App) {
        if let Some(state) = self.inputs.get(key).cloned() {
            let value = value.to_string();
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    /// Open a menu of `entries` below the control recorded under `key`.
    #[allow(clippy::too_many_arguments)]
    pub fn open_menu(
        &mut self,
        key: &str,
        min_width: f32,
        max_height: Option<f32>,
        entries: Vec<MenuEntry>,
        selected: Option<String>,
        on_pick: PickHandler,
        cx: &mut Context<Self>,
    ) {
        let anchor = self.anchor(key);
        self.menu = Some(OpenMenu {
            key: key.to_string(),
            anchor,
            min_width,
            max_height,
            entries,
            selected,
            on_pick,
        });
        cx.notify();
    }

    pub fn menu_open(&self, key: &str) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.key == key)
    }

    /// `showToast`: errors stay until dismissed, the rest hide after five seconds.
    pub fn show_toast(
        &mut self,
        status: Status,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toast = Some(Toast {
            id,
            status,
            message: message.into(),
        });
        if status != Status::Error {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.toast.as_ref().is_some_and(|toast| toast.id == id) {
                        this.toast = None;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// `confirmAction`.
    pub fn confirm(
        &mut self,
        title: impl Into<String>,
        description: impl Into<String>,
        confirm_label: impl Into<String>,
        destructive: bool,
        on_confirm: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.confirm = Some(Confirm {
            title: title.into(),
            description: description.into(),
            confirm_label: confirm_label.into(),
            destructive,
            on_confirm: Rc::new(on_confirm),
        });
        cx.notify();
    }

    /// Bring a section (and entry) into view (`show`).
    pub fn show(&mut self, target: SettingsTarget, window: &mut Window, cx: &mut Context<Self>) {
        match self.nav.show(target) {
            Navigation::Go(_) => self.section_changed(window, cx),
            Navigation::Focus(_) => {
                self.state.focus_started(&self.nav, cx);
                cx.notify();
            }
            Navigation::Confirm(target) => self.confirm_leave_logging(target, cx),
        }
    }

    fn pick(&mut self, section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) {
        match self.nav.pick(section) {
            Some(Navigation::Go(_)) => self.section_changed(window, cx),
            Some(Navigation::Confirm(target)) => self.confirm_leave_logging(target, cx),
            _ => {}
        }
    }

    fn confirm_leave_logging(&mut self, target: SettingsTarget, cx: &mut Context<Self>) {
        self.confirm(
            "Discard unsaved logging settings?",
            "Session logging changes are not applied until you save them. Leaving now loses your edits.",
            "Discard changes",
            true,
            move |this, window, cx| {
                this.nav.discard_and_go(target.clone());
                this.state.reset_logging();
                this.section_changed(window, cx);
            },
            cx,
        );
    }

    fn section_changed(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.state.focus_started(&self.nav, cx);
        self.load_section(cx);
        cx.notify();
    }

    /// Esc closes the window as it closes the dialog; menus, dialogs and the
    /// shortcut recorder take their Esc first.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        if self.menu.take().is_some() {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.confirm.take().is_some() {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.state.close_dialog(cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.shortcuts.is_some() {
            return;
        }
        cx.stop_propagation();
        self.request_close(window, cx);
    }

    /// Close the window, asking first while session logging holds unsaved edits.
    pub fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.nav.holds_unsaved_logging() {
            super::save_placement(window, cx);
            window.remove_window();
            return;
        }
        self.ask_discard_on_close(window, cx);
    }

    /// The desktop shell's question when a window with unsaved logging edits closes.
    pub fn ask_discard_on_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            muxus_ui::gpui_kit::PromptLevel::Info,
            "Discard unsaved logging settings?",
            Some("Session logging changes are not applied until you save them. Leaving now loses your edits."),
            &["Discard changes", "Keep editing"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.nav.logging_dirty = false;
                    super::save_placement(window, cx);
                    window.remove_window();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn render_title_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let (bar, _) = theme.title_bar_colors(0.);
        window_frame::title_bar(
            div()
                .id("settings-title-bar")
                .flex()
                .flex_none()
                .items_center()
                .h(px(layout::TOP_BAR_HEIGHT + 1.))
                .pl(px(window_frame::traffic_light_inset(window).unwrap_or(20.)))
                .bg(bar)
                .border_b_1()
                .border_color(p.divider),
        )
        .child(
            label("Settings")
                .size(16.)
                .weight(650.)
                .line_height(snap(24.))
                .color(p.text.primary)
                .grow(),
        )
        .when_some(
            window_frame::window_controls(window, cx),
            |bar, controls| bar.child(controls),
        )
        .when(!window_frame::draws_window_controls(window), |bar| {
            bar.pr(px(16.))
        })
        .into_any_element()
    }

    fn render_nav(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let dark = theme.mode.is_dark();
        let bg = if dark {
            alpha(Hsla::black(), 0.12)
        } else {
            alpha(p.text.primary, 0.02)
        };
        let selected_bg = alpha(p.primary.main, if dark { 0.16 } else { 0.09 });
        let mut list = div().flex().flex_col().py(px(8.));
        for section in SettingsSection::ALL {
            let selected = section == self.nav.section;
            let ink = if selected {
                p.primary.main
            } else {
                p.text.secondary
            };
            let hover_ink = p.text.primary;
            let mut tab = div()
                .id(SharedString::from(format!("settings-nav-{}", section.id())))
                .group(SharedString::from(format!("nav-{}", section.id())))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .h(px(34.))
                .mx(px(8.))
                .my(px(1.))
                .px(px(10.))
                .rounded(px(12.))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| this.pick(section, window, cx)));
            if selected {
                tab = tab.bg(selected_bg);
            } else {
                tab = tab.hover(move |s| s.bg(p.action.hover));
            }
            let group_name = SharedString::from(format!("nav-{}", section.id()));
            tab = tab
                .child(
                    div()
                        .flex_none()
                        .text_color(ink)
                        .when(!selected, |d| {
                            d.group_hover(group_name.clone(), move |s| s.text_color(hover_ink))
                        })
                        .child(icon(section.icon(), 18., ink)),
                )
                .child(
                    label(section.label())
                        .size(13.)
                        .weight(500.)
                        .line_height(snap(16.25))
                        .color(ink)
                        .grow(),
                );
            if section == SettingsSection::Logging && self.nav.logging_dirty {
                let anchor = self.anchor("nav-unsaved");
                tab = tab.child(
                    div()
                        .id("settings-nav-unsaved")
                        .size(px(7.))
                        .rounded(px(3.5))
                        .bg(p.warning.main)
                        .child(muxus_ui::widgets::bounds_probe(&anchor))
                        .tooltip(Tooltip::anchored(
                            "Unsaved changes",
                            anchor,
                            muxus_ui::widgets::Placement::Bottom,
                        )),
                );
            }
            list = list.child(tab);
        }
        div()
            .id("settings-nav")
            .flex_none()
            .w(px(196.))
            .h_full()
            .border_r_1()
            .border_color(p.divider)
            .bg(bg)
            .overflow_y_scroll()
            .child(list)
            .into_any_element()
    }

    fn render_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.nav.section {
            SettingsSection::Appearance => self.appearance(window, cx),
            SettingsSection::Terminal => self.terminal(window, cx),
            SettingsSection::LocalShells => self.local_shells(window, cx),
            SettingsSection::Logging => self.logging(window, cx),
            SettingsSection::Highlighting => self.highlighting(window, cx),
            SettingsSection::Behavior => self.behavior(window, cx),
            SettingsSection::X11 => self.x11(window, cx),
            SettingsSection::Keyboard => self.keyboard(window, cx),
            SettingsSection::Passwords => self.passwords(window, cx),
            SettingsSection::Data => self.data_section(window, cx),
            SettingsSection::Debug => self.debug(window, cx),
            SettingsSection::About => self.about(window, cx),
        }
    }

    fn render_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let position = below(&menu.anchor)?;
        let theme = *cx.theme();
        let width = menu.anchor.get().map_or(menu.min_width, |bounds| {
            muxus_ui::zoom::unzoom(bounds.size.width).max(menu.min_width)
        });
        let mut list = menu_paper(&theme).min_w(px(width));
        if let Some(max) = menu.max_height {
            list = list.max_h(px(max));
        }
        let mut items = div()
            .id("settings-menu-items")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .when_some(menu.max_height, |items, max| items.max_h(px(max - 16.)));
        for (index, entry) in menu.entries.iter().enumerate() {
            match entry {
                MenuEntry::Header(text) => {
                    items = items.child(
                        div()
                            .flex()
                            .items_center()
                            .h(px(48.))
                            .px(px(16.))
                            .bg(theme.palette.background_paper)
                            .child(
                                label(text.clone())
                                    .size(BODY2_HEADER)
                                    .weight(500.)
                                    .line_height(snap(48.))
                                    .color(theme.palette.text.secondary),
                            ),
                    );
                }
                MenuEntry::Item {
                    value,
                    label: text,
                    secondary,
                    rich,
                } => {
                    let picked = value.clone();
                    let on_pick = menu.on_pick.clone();
                    let selected = menu.selected.as_deref() == Some(value.as_str());
                    let mut item = MenuItem::new(
                        SharedString::from(format!("settings-menu-{index}")),
                        if rich.is_some() || secondary.is_some() {
                            SharedString::default()
                        } else {
                            text.clone()
                        },
                    )
                    .selected(selected)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        on_pick(this, picked.clone(), window, cx);
                        cx.notify();
                    }));
                    if let Some(rich) = rich {
                        item = item.leading(div().flex_1().child(rich(&theme)));
                    } else if let Some(secondary) = secondary {
                        item = item.leading(
                            div()
                                .flex()
                                .flex_col()
                                .py(px(6.))
                                .child(
                                    label(text.clone())
                                        .size(BODY1)
                                        .line_height(snap(BODY1 * 1.5))
                                        .color(theme.palette.text.primary),
                                )
                                .child(
                                    label(secondary.clone())
                                        .size(13.)
                                        .line_height(snap(13. * 1.43))
                                        .color(theme.palette.text.secondary),
                                ),
                        );
                    }
                    items = items.child(item);
                }
            }
        }
        list = list.child(items);
        let dismiss = cx.entity().downgrade();
        Some(
            popup(
                position,
                move |_, cx| {
                    let _ = dismiss.update(cx, |this, cx| {
                        this.menu = None;
                        cx.notify();
                    });
                },
                list,
            )
            .into_any_element(),
        )
    }

    fn render_confirm(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let confirm = self.confirm.as_ref()?;
        let theme = *cx.theme();
        let p = theme.palette;
        let on_confirm = confirm.on_confirm.clone();
        let dialog = Dialog::new()
            .title(confirm.title.clone())
            .max_width(DialogWidth::Xs)
            .full_width()
            .on_close({
                let this = cx.entity().downgrade();
                move |_, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.confirm = None;
                        cx.notify();
                    });
                }
            })
            .content(
                paragraph(confirm.description.clone())
                    .size(13.)
                    .line_height(snap(13. * 1.43))
                    .color(p.text.secondary),
            )
            .action(
                Button::new("settings-confirm-cancel", "Cancel").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.confirm = None;
                        cx.notify();
                    },
                )),
            )
            .action(
                Button::new("settings-confirm-commit", confirm.confirm_label.clone())
                    .contained()
                    .color(if confirm.destructive {
                        ButtonColor::Error
                    } else {
                        ButtonColor::Primary
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm = None;
                        on_confirm(this, window, cx);
                        cx.notify();
                    })),
            );
        Some(dialog.into_any_element())
    }

    fn render_toast(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast = self.toast.clone()?;
        Some(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(24.))
                .flex()
                .justify_center()
                .child(
                    Alert::new(toast.status, toast.message)
                        .filled()
                        .max_width(560.)
                        .action(
                            IconButton::new("settings-close-toast", "Close")
                                .color(ButtonColor::Inherit)
                                .tooltip("Close notification")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.toast = None;
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            return;
        }
        let Some(bounds) = drag.anchor.get() else {
            return;
        };
        let fraction = (muxus_ui::zoom::unzoom(event.position.x - bounds.origin.x)
            / muxus_ui::zoom::unzoom(bounds.size.width))
        .clamp(0., 1.);
        if let Some(on_move) = drag.on_move_2d.clone() {
            let fraction_y = (muxus_ui::zoom::unzoom(event.position.y - bounds.origin.y)
                / muxus_ui::zoom::unzoom(bounds.size.height))
            .clamp(0., 1.);
            on_move(self, fraction, fraction_y, cx);
        } else {
            let on_move = drag.on_move.clone();
            on_move(self, fraction, cx);
        }
        cx.notify();
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.drag = None;
    }
}

/// `subheader` text of a select menu (MUI ListSubheader: 14px, 48px line).
const BODY2_HEADER: f32 = 14. * 13. / 14.;

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        muxus_ui::zoom::apply_to_window(window);
        let theme = *cx.theme();
        let p = theme.palette;
        self.state.before_render(self.nav.section, cx);
        let title_bar = self.render_title_bar(window, cx);
        let nav = self.render_nav(cx);
        let section = self.render_section(window, cx);
        let content = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .id("settings-content")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .bg(p.background_default)
                    .px(px(28.))
                    .py(px(24.))
                    .child(div().max_w(px(760.)).child(section)),
            )
            .child(muxus_ui::widgets::scrollbar::vertical(&self.scroll, &theme));
        let menu = self.render_menu(cx);
        let confirm = self.render_confirm(cx);
        let toast = self.render_toast(cx);
        let dialogs = self.render_dialogs(window, cx);
        let shortcuts = self.shortcuts.clone();
        let root = div()
            .id("settings-window")
            .key_context("SettingsWindow")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(p.background_default)
            .font_family(fonts::UI)
            .text_color(p.text.primary)
            .child(title_bar)
            .child(div().flex().flex_1().min_h_0().child(nav).child(content))
            .children(dialogs)
            .children(self.render_rule_colors(cx))
            .children(self.render_color_picker(window, cx))
            .children(shortcuts)
            .children(menu)
            .children(confirm)
            .children(toast);
        window_frame::decorate_window(root, window, cx)
    }
}

/// A width that is never narrower than its content: `relative(1.)` in a column.
pub fn full() -> muxus_ui::gpui_kit::DefiniteLength {
    relative(1.)
}
