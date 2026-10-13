//! `components/SendSecretMenu.tsx`: a keyboard-first picker that types a
//! named secret into the focused session, and into the panes
//! multi-execution mirrors it to. Only names are listed; the backend types
//! the value.
//!
//! The window opens it on the `terminal.send-secret` command at the
//! terminal's caret (`cursor_anchor` of the command hooks) or the window's
//! centre, and drops it on [`SendSecretMenuEvent::Close`]. Sending goes
//! through `CommandHooks::send_secret` (see [`super::send::SecretSender`]).

use muxus_core::api::{PasswordVaultSecret, PasswordVaultStatus};
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Status, Theme, sizes};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};

use crate::features::command_buttons::run::{SecretRef, command_hooks, focus_terminal};
use crate::features::launcher::kit::show_toast;
use crate::features::settings::vault_secrets::filter_vault_secrets;

/// What the menu tells its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendSecretMenuEvent {
    /// Closed, after a send or without one; the terminal has its focus back.
    Close,
    /// "Manage secrets…": open the settings at Passwords.
    ManageSecrets,
}

/// The menu paper's size limits (`minWidth: 300, maxWidth: 420, maxHeight: 380`).
const MENU_MIN_WIDTH: f32 = 300.;
const MENU_MAX_WIDTH: f32 = 420.;
const MENU_MAX_HEIGHT: f32 = 380.;
/// The hint's line height (`lineHeight: 1.4`).
const HINT_LINE_HEIGHT: f32 = 1.4;

/// The hint under the search box.
pub const SEND_SECRET_HINT: &str = "Enter types it and presses Enter · Shift+Enter types it only";

/// The disabled line when nothing can be listed, if any.
pub fn empty_line(
    status: Option<&PasswordVaultStatus>,
    loading: bool,
    filtered: usize,
) -> Option<&'static str> {
    let total = status.map_or(0, |status| status.secrets.len());
    let empty = !loading && total == 0;
    if empty {
        return Some(if status.is_some_and(|status| !status.configured) {
            "No password vault yet"
        } else {
            "No secrets saved yet"
        });
    }
    (filtered == 0 && !loading).then_some("No matching secrets")
}

/// Where keyboard focus is inside the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuFocus {
    Search,
    /// A listed secret, by its position in the filtered list.
    Secret(usize),
    Manage,
}

/// Arrow keys between the search box, the results and "Manage secrets…".
/// Disabled results (no connected terminal) are skipped.
pub fn move_focus(focus: MenuFocus, down: bool, results: usize, enabled: bool) -> MenuFocus {
    let results = if enabled { results } else { 0 };
    match (focus, down) {
        (MenuFocus::Search, true) | (MenuFocus::Manage, true) => {
            if results > 0 {
                MenuFocus::Secret(0)
            } else {
                MenuFocus::Manage
            }
        }
        (MenuFocus::Search, false) => MenuFocus::Manage,
        (MenuFocus::Secret(index), true) => {
            if index + 1 < results {
                MenuFocus::Secret(index + 1)
            } else {
                MenuFocus::Manage
            }
        }
        (MenuFocus::Secret(index), false) => {
            if index > 0 {
                MenuFocus::Secret(index - 1)
            } else {
                MenuFocus::Manage
            }
        }
        (MenuFocus::Manage, false) => {
            if results > 0 {
                MenuFocus::Secret(results - 1)
            } else {
                MenuFocus::Manage
            }
        }
    }
}

pub struct SendSecretMenu {
    status: Option<PasswordVaultStatus>,
    /// A status handed in by [`Self::set_status`] wins over the backend's.
    pinned: bool,
    loading: bool,
    tab_id: Option<String>,
    connected: bool,
    position: Point<Pixels>,
    query: Entity<InputState>,
    focused: MenuFocus,
    focus: FocusHandle,
    _events: Subscription,
}

impl EventEmitter<SendSecretMenuEvent> for SendSecretMenu {}

impl Focusable for SendSecretMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SendSecretMenu {
    /// `tab_id` is the active tab, `connected` whether it is a connected
    /// terminal, `position` where the menu opens.
    pub fn new(
        backend: Backend,
        tab_id: Option<String>,
        connected: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search secrets"));
        let events = cx.subscribe_in(&query, window, |this, _, event, window, cx| match event {
            InputEvent::Change => {
                this.focused = MenuFocus::Search;
                cx.notify();
            }
            InputEvent::PressEnter { shift, .. } => {
                let shift = *shift || window.modifiers().shift;
                this.send_first(!shift, window, cx);
            }
            _ => {}
        });
        query.update(cx, |state, cx| state.focus(window, cx));
        cx.spawn(async move |this, cx| {
            let status = backend.vault_status().await.ok();
            let _ = this.update(cx, |this, cx| {
                if !this.pinned {
                    this.status = status;
                    this.loading = false;
                    cx.notify();
                }
            });
        })
        .detach();
        Self {
            status: None,
            pinned: false,
            loading: true,
            tab_id,
            connected,
            position,
            query,
            focused: MenuFocus::Search,
            focus: cx.focus_handle(),
            _events: events,
        }
    }

    /// Show a vault status without asking the backend (the documentation scene).
    pub fn set_status(&mut self, status: PasswordVaultStatus, cx: &mut Context<Self>) {
        self.pinned = true;
        self.status = Some(status);
        self.loading = false;
        cx.notify();
    }

    fn filtered(&self, cx: &App) -> Vec<PasswordVaultSecret> {
        let query = self.query.read(cx).value().to_string();
        let secrets = self
            .status
            .as_ref()
            .map_or(&[][..], |status| status.secrets.as_slice());
        filter_vault_secrets(secrets, &query)
            .into_iter()
            .cloned()
            .collect()
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        focus_terminal(self.tab_id.as_deref(), window, cx);
        cx.emit(SendSecretMenuEvent::Close);
    }

    fn send(
        &mut self,
        secret: &PasswordVaultSecret,
        enter: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sent = match (self.tab_id.as_deref(), command_hooks(cx).send_secret) {
            (Some(tab_id), Some(send)) => send(
                tab_id,
                &SecretRef {
                    id: secret.id.clone(),
                    name: secret.name.clone(),
                },
                enter,
                window,
                cx,
            ),
            _ => false,
        };
        if !sent {
            show_toast(Status::Warning, "The active terminal is not connected.", cx);
        }
        self.close(window, cx);
    }

    fn send_first(&mut self, enter: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connected {
            return;
        }
        if let Some(first) = self.filtered(cx).first().cloned() {
            self.send(&first, enter, window, cx);
        }
    }

    fn activate(&mut self, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        match self.focused {
            MenuFocus::Search => self.send_first(!shift, window, cx),
            MenuFocus::Secret(index) => {
                if let Some(secret) = self.filtered(cx).get(index).cloned()
                    && self.connected
                {
                    self.send(&secret, !shift, window, cx);
                }
            }
            MenuFocus::Manage => {
                cx.emit(SendSecretMenuEvent::ManageSecrets);
                cx.emit(SendSecretMenuEvent::Close);
            }
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.filtered(cx).len();
        match event.keystroke.key.as_str() {
            "down" | "up" => {
                cx.stop_propagation();
                self.focused = move_focus(
                    self.focused,
                    event.keystroke.key == "down",
                    count,
                    self.connected,
                );
                if self.focused == MenuFocus::Search {
                    self.query.update(cx, |state, cx| state.focus(window, cx));
                } else {
                    window.focus(&self.focus, cx);
                }
                cx.notify();
            }
            "enter" if self.focused != MenuFocus::Search => {
                cx.stop_propagation();
                self.activate(event.keystroke.modifiers.shift, window, cx);
            }
            "escape" => {
                cx.stop_propagation();
                self.close(window, cx);
            }
            _ => {}
        }
    }
}

impl Render for SendSecretMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let typography = theme.typography;
        let filtered = self.filtered(cx);
        let hint = typography.caption;
        let search = div()
            .px(Theme::spacing(2.))
            .py(Theme::spacing(1.))
            .bg(p.background_paper)
            .child(
                FormField::input("send-secret-search", &self.query)
                    .placeholder("Search secrets")
                    .start(icon("Search", sizes::ICON_CHIP, p.action.active)),
            )
            .child(
                div().mt(Theme::spacing(0.75)).child(
                    label(SEND_SECRET_HINT)
                        .variant(hint)
                        .line_height(hint.size * HINT_LINE_HEIGHT)
                        .color(p.text.secondary)
                        .no_truncate(),
                ),
            );
        let mut list = div()
            .id("send-secret-list")
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .flex_1()
            .min_h_0();
        if let Some(line) = empty_line(self.status.as_ref(), self.loading, filtered.len()) {
            list = list.child(
                MenuItem::new("send-secret-empty", line)
                    .dense()
                    .disabled(true),
            );
        }
        for (index, secret) in filtered.iter().enumerate() {
            let pick = secret.clone();
            let mut item = MenuItem::new(
                SharedString::from(format!("send-secret-{}", secret.id)),
                secret.name.clone(),
            )
            .dense()
            .icon("KeyOutlined")
            .highlighted(self.focused == MenuFocus::Secret(index))
            .disabled(!self.connected)
            .on_click(cx.listener(move |this, _, window, cx| {
                let shift = window.modifiers().shift;
                this.send(&pick, !shift, window, cx);
            }));
            if let Some(username) = &secret.username {
                item = item.trailing(
                    div().ml_auto().pl(Theme::spacing(1.5)).flex_none().child(
                        label(username.clone())
                            .size(sizes::TEXT_SMALL)
                            .line_height(typography.body2.line_height)
                            .color(p.text.secondary)
                            .no_truncate(),
                    ),
                );
            }
            list = list.child(item);
        }
        list = list.child(menu_divider(&theme)).child(
            MenuItem::new("send-secret-manage", "Manage secrets\u{2026}")
                .dense()
                .icon("SettingsOutlined")
                .highlighted(self.focused == MenuFocus::Manage)
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(SendSecretMenuEvent::ManageSecrets);
                    cx.emit(SendSecretMenuEvent::Close);
                })),
        );
        let content = menu::paper(&theme)
            .min_w(px(MENU_MIN_WIDTH))
            .max_w(px(MENU_MAX_WIDTH))
            .max_h(px(MENU_MAX_HEIGHT))
            .child(search)
            .child(list);
        let dismiss = cx.entity().downgrade();
        div()
            .id("send-secret-menu")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(menu::popup(
                self.position,
                move |window, cx| {
                    let _ = dismiss.update(cx, |this, cx| this.close(window, cx));
                },
                content,
            ))
    }
}

/// Open the menu at `position` for the active tab.
pub fn open_send_secret_menu(
    backend: Backend,
    tab_id: Option<String>,
    connected: bool,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SendSecretMenu> {
    cx.new(|cx| SendSecretMenu::new(backend, tab_id, connected, position, window, cx))
}

/// The menu's default position without a caret anchor (`menuCenter`).
pub fn menu_center(window: &Window) -> Point<Pixels> {
    let size = window.viewport_size();
    Point {
        x: muxus_ui::zoom::layout_px((f32::from(size.width) / 2.).round()),
        y: muxus_ui::zoom::layout_px((f32::from(size.height) / 2.).round()),
    }
}

/// The menu's label for a secret row, for tests and accessibility text.
pub fn secret_row_label(secret: &PasswordVaultSecret) -> SharedString {
    match &secret.username {
        Some(user) => format!("{} {user}", secret.name).into(),
        None => secret.name.clone().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(configured: bool, names: &[&str]) -> PasswordVaultStatus {
        PasswordVaultStatus {
            configured,
            unlock_policy: None,
            locked: false,
            os_key_store_available: true,
            credential_count: 0,
            credentials: Vec::new(),
            secrets: names
                .iter()
                .map(|name| PasswordVaultSecret {
                    id: name.to_string(),
                    name: name.to_string(),
                    username: None,
                    created_at: String::new(),
                    updated_at: String::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn says_why_nothing_is_listed() {
        assert_eq!(empty_line(None, true, 0), None);
        assert_eq!(
            empty_line(Some(&status(false, &[])), false, 0),
            Some("No password vault yet")
        );
        assert_eq!(
            empty_line(Some(&status(true, &[])), false, 0),
            Some("No secrets saved yet")
        );
        assert_eq!(
            empty_line(Some(&status(true, &["a"])), false, 0),
            Some("No matching secrets")
        );
        assert_eq!(empty_line(Some(&status(true, &["a"])), false, 1), None);
    }

    #[test]
    fn moves_between_search_results_and_manage() {
        use MenuFocus::*;
        assert_eq!(move_focus(Search, true, 3, true), Secret(0));
        assert_eq!(move_focus(Search, true, 0, true), Manage);
        // Results of a disconnected tab are disabled and skipped.
        assert_eq!(move_focus(Search, true, 3, false), Manage);
        assert_eq!(move_focus(Search, false, 3, true), Manage);
        assert_eq!(move_focus(Secret(0), true, 3, true), Secret(1));
        assert_eq!(move_focus(Secret(2), true, 3, true), Manage);
        assert_eq!(move_focus(Secret(0), false, 3, true), Manage);
        assert_eq!(move_focus(Manage, true, 3, true), Secret(0));
        assert_eq!(move_focus(Manage, false, 3, true), Secret(2));
    }
}
