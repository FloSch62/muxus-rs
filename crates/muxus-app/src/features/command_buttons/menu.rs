//! The keyboard command menu (`client/src/components/CommandButtonMenu.tsx`,
//! Ctrl+Space by default): a compact, keyboard-first picker at the
//! terminal's caret. The group the bar shows comes first; the others follow
//! in their own sections, so a search reaches every saved command.
//!
//! The window mounts the view and removes it on [`CommandMenuEvent::Close`].

use muxus_store::prefs::command_buttons::{
    CommandButton, CommandButtonGroup, command_button_label, command_buttons_in_group,
    filter_command_buttons,
};
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Point, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, div, point, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::TextField;
use muxus_ui::widgets::list::ListSubheader;
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};

use super::model::{CommandButtons, prefs};
use super::palette::command_button_ink;
use super::run::{command_hooks, focus_terminal, run_command_button};
use crate::features::launcher::kit::show_toast;
use crate::features::settings::vault_secrets::{find_vault_secret, vault_secret_missing};

/// What the menu asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandMenuEvent {
    /// Remove the menu (the terminal has its focus back).
    Close,
    /// "Manage command buttons…": open the editor.
    ManageCommands,
}

pub struct CommandButtonMenu {
    model: Entity<CommandButtons>,
    search: Entity<InputState>,
    focus: FocusHandle,
    /// Where the menu opened: the terminal's caret, else the window's centre.
    anchor: Option<Point<Pixels>>,
    /// The item keyboard focus is on, once it left the search box: an index
    /// into the enabled commands, or `usize::MAX` for "Manage…".
    focused: Option<usize>,
    /// Keyboard focus moved: scroll its item into view on the next render.
    reveal_focus: bool,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CommandMenuEvent> for CommandButtonMenu {}

impl Focusable for CommandButtonMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open the menu at the active terminal's caret.
pub fn open_command_menu(
    model: Entity<CommandButtons>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<CommandButtonMenu> {
    cx.new(|cx| CommandButtonMenu::new(model, window, cx))
}

/// The manage item's slot in `focused`.
const MANAGE: usize = usize::MAX;

impl CommandButtonMenu {
    pub fn new(model: Entity<CommandButtons>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search commands"));
        let subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.focused = None;
                    cx.notify();
                }
            }),
        ];
        let tab = model.read(cx).active_id().map(str::to_owned);
        let anchor = match (tab, command_hooks(cx).cursor_anchor) {
            (Some(tab), Some(anchor)) => anchor(&tab, window, cx),
            _ => None,
        };
        search.update(cx, |state, cx| state.focus(window, cx));
        Self {
            model,
            search,
            focus: cx.focus_handle(),
            anchor,
            focused: None,
            reveal_focus: false,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The sections: the shown group first, each with the commands that
    /// match the search.
    fn sections(&self, cx: &App) -> Vec<(CommandButtonGroup, Vec<CommandButton>)> {
        let prefs = prefs(cx);
        let model = self.model.read(cx);
        let active = model.active_group(cx);
        let vault = model.vault();
        let query = self.search.read(cx).value().to_string();
        let groups = &prefs.command_button_groups;
        let shown = groups.iter().find(|group| group.id == active.shown_id);
        let mut ordered: Vec<&CommandButtonGroup> = Vec::new();
        if let Some(shown) = shown {
            ordered.push(shown);
        }
        ordered.extend(groups.iter().filter(|group| Some(*group) != shown));
        ordered
            .into_iter()
            .map(|group| {
                let in_group = command_buttons_in_group(&prefs.command_buttons, groups, &group.id);
                let found: Vec<CommandButton> = filter_command_buttons(&in_group, &query, |id| {
                    find_vault_secret(vault, id).map(|secret| secret.name.clone())
                })
                .into_iter()
                .cloned()
                .collect();
                (group.clone(), found)
            })
            .filter(|(_, found)| !found.is_empty())
            .collect()
    }

    fn usable(&self, button: &CommandButton, cx: &App) -> bool {
        match &button.secret_id {
            Some(id) => {
                !id.is_empty()
                    && vault_secret_missing(self.model.read(cx).vault(), id) != Some(true)
            }
            None => !muxus_core::js::js_trim(&button.command).is_empty(),
        }
    }

    /// The commands keyboard focus can reach, in order.
    fn enabled(&self, cx: &App) -> Vec<CommandButton> {
        if !self.model.read(cx).connected() {
            return Vec::new();
        }
        self.sections(cx)
            .into_iter()
            .flat_map(|(_, buttons)| buttons)
            .filter(|button| self.usable(button, cx))
            .collect()
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.model.read(cx).active_id().map(str::to_owned);
        cx.emit(CommandMenuEvent::Close);
        focus_terminal(tab.as_deref(), window, cx);
    }

    fn send(&mut self, button: &CommandButton, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let tab = model.active_id().map(str::to_owned);
        let secret_name = button
            .secret_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .and_then(|id| find_vault_secret(model.vault(), id))
            .map(|secret| secret.name.clone());
        cx.emit(CommandMenuEvent::Close);
        if !run_command_button(tab.as_deref(), button, secret_name.as_deref(), window, cx) {
            show_toast(Status::Warning, "The active terminal is not connected.", cx);
        }
        focus_terminal(tab.as_deref(), window, cx);
    }

    fn manage(&mut self, cx: &mut Context<Self>) {
        // The request first: an owner that drops its subscription on `Close` would miss it.
        cx.emit(CommandMenuEvent::ManageCommands);
        cx.emit(CommandMenuEvent::Close);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = self.enabled(cx);
        match (event.keystroke.key.as_str(), self.focused) {
            ("escape", _) => self.close(window, cx),
            // In the search box.
            ("down", None) => {
                self.focused = Some(if enabled.is_empty() { MANAGE } else { 0 });
                window.focus(&self.focus, cx);
            }
            ("up", None) => {
                self.focused = Some(MANAGE);
                window.focus(&self.focus, cx);
            }
            ("enter", None) => {
                let Some(first) = enabled.first().cloned() else {
                    return;
                };
                self.send(&first, window, cx);
            }
            // On an item (MenuList wraps around).
            ("down", Some(index)) => {
                self.focused = Some(match index {
                    MANAGE if enabled.is_empty() => MANAGE,
                    MANAGE => 0,
                    index if index + 1 < enabled.len() => index + 1,
                    _ => MANAGE,
                });
            }
            ("up", Some(index)) => {
                self.focused = Some(match index {
                    MANAGE if enabled.is_empty() => MANAGE,
                    MANAGE => enabled.len() - 1,
                    0 => MANAGE,
                    index => index - 1,
                });
            }
            ("enter", Some(MANAGE)) => self.manage(cx),
            ("enter", Some(index)) => {
                if let Some(button) = enabled.get(index).cloned() {
                    self.send(&button, window, cx);
                }
            }
            _ => return,
        }
        self.reveal_focus = true;
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for CommandButtonMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let prefs = prefs(cx);
        let connected = self.model.read(cx).connected();
        let vault = self.model.read(cx).vault().cloned();
        let sections = self.sections(cx);
        let show_sections = prefs.command_button_groups.len() > 1;
        let all: Vec<&CommandButton> = sections.iter().flat_map(|(_, buttons)| buttons).collect();
        // Keep labels aligned: uncolored commands get an empty slot beside colored ones.
        let any_color = all.iter().any(|button| button.color.is_some());
        let weak = cx.entity().downgrade();
        let dot = Theme::spacing(1.);

        let mut items: Vec<AnyElement> = Vec::new();
        items.push(
            div()
                .flex_none()
                .px(Theme::spacing(2.))
                .py(Theme::spacing(1.))
                .bg(p.background_paper)
                .child(
                    TextField::new("command-menu-search", &self.search)
                        // `<SearchIcon sx={{ fontSize: 18 }} />`.
                        .start(icon("Search", 18., p.action.active))
                        .full_width(),
                )
                .into_any_element(),
        );
        if prefs.command_buttons.is_empty() {
            items.push(
                MenuItem::new("command-menu-none", "No saved commands yet")
                    .dense()
                    .disabled(true)
                    .into_any_element(),
            );
        } else if all.is_empty() {
            items.push(
                MenuItem::new("command-menu-nomatch", "No matching commands")
                    .dense()
                    .disabled(true)
                    .into_any_element(),
            );
        }
        // The focused item's place in the scrolling list (the search, the
        // first entry here, stays out of it).
        let mut focused_child = None;
        let mut enabled_index = 0;
        for (group, buttons) in &sections {
            if show_sections {
                items.push(
                    ListSubheader::new(group.name.clone())
                        .compact()
                        .into_any_element(),
                );
            }
            for button in buttons {
                let usable = connected && self.usable(button, cx);
                let focused = usable && self.focused == Some(enabled_index);
                if usable {
                    enabled_index += 1;
                }
                let secret_button = button.secret_id.is_some();
                let missing = button.secret_id.as_deref().is_some_and(|id| {
                    !id.is_empty() && vault_secret_missing(vault.as_ref(), id) == Some(true)
                });
                let mut item = MenuItem::new(
                    ("command-menu-item", enabled_index + items.len()),
                    command_button_label(button),
                )
                .dense()
                .disabled(!usable)
                .focused(focused);
                if any_color {
                    let slot = div().size(dot).flex_none().rounded_full();
                    let slot = match button.color {
                        Some(color) => slot.bg(command_button_ink(color, &theme)),
                        None => slot,
                    };
                    item = item.prefix(slot, Theme::spacing_css(1.25));
                }
                if secret_button {
                    let (name, ink) = if missing {
                        ("WarningAmberOutlined", p.warning.main)
                    } else {
                        ("KeyOutlined", p.text.secondary)
                    };
                    item =
                        item.trailing(div().ml(Theme::spacing(1.25)).child(icon(name, 16., ink)));
                }
                let send = weak.clone();
                let button = button.clone();
                item = item.on_click(move |_, window, cx| {
                    let button = button.clone();
                    let _ = send.update(cx, |this, cx| this.send(&button, window, cx));
                });
                if focused {
                    focused_child = Some(items.len() - 1);
                }
                items.push(item.into_any_element());
            }
        }
        items.push(menu_divider(&theme).into_any_element());
        if self.focused == Some(MANAGE) {
            focused_child = Some(items.len() - 1);
        }
        let manage = weak.clone();
        items.push(
            MenuItem::new("command-menu-manage", "Manage command buttons…")
                .icon("SettingsOutlined")
                .dense()
                .focused(self.focused == Some(MANAGE))
                .on_click(move |_, _, cx| {
                    let _ = manage.update(cx, |this, cx| this.manage(cx));
                })
                .into_any_element(),
        );

        if std::mem::take(&mut self.reveal_focus)
            && let Some(child) = focused_child
        {
            self.scroll.scroll_to_item(child);
        }
        // The search sits at the top (MUI keeps the sticky subheader there);
        // the commands scroll under it, out to the paper's bottom edge.
        let mut items = items.into_iter();
        let header = items.next();
        let paper = menu::paper(&theme)
            .py(px(0.))
            .min_w(px(280.))
            .max_w(px(420.))
            .max_h(px(360.))
            .child(div().flex_none().pt(Theme::spacing(1.)).children(header))
            .child(
                div()
                    .id("command-menu-list")
                    .flex()
                    .flex_col()
                    .pb(Theme::spacing(1.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(items),
            );
        let viewport = window.viewport_size();
        let position = self
            .anchor
            .unwrap_or_else(|| point(viewport.width / 2., viewport.height / 2.));
        let close = weak.clone();
        div()
            .id("command-menu")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key_down))
            .child(menu::popup(
                position,
                move |window, cx| {
                    let _ = close.update(cx, |this, cx| this.close(window, cx));
                },
                paper,
            ))
    }
}
