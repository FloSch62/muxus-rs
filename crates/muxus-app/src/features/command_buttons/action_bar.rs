//! The command bar (`client/src/components/ActionBar.tsx`): the saved
//! commands of the shown group as one-click buttons above (or below) the
//! terminals, the group picker when there is more than one group, and the
//! way to the editor.
//!
//! The window mounts [`ActionBar`] where the bar goes when
//! [`ActionBar::visible`] says so, and opens the editor on
//! [`ActionBarEvent::ManageCommands`].

use muxus_store::prefs::command_buttons::{CommandButton, command_button_label};
use muxus_store::prefs::state::CommandBarPosition;
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    Anchor, AnyElement, App, AppContext, Context, Entity, EventEmitter, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Pixels, Point, Render, StatefulInteractiveElement,
    Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::button::ButtonTint;
use muxus_ui::widgets::display::vertical_divider;
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::tooltip::{AnchorBounds, Placement, bounds_probe};
use muxus_ui::widgets::{Button, IconButton};

use super::model::{CommandButtons, prefs, update_prefs};
use super::palette::command_button_ink;
use super::run::{button_tooltip, run_command_button};
use crate::features::launcher::kit::{follow_anchor, show_toast, text};
use crate::features::settings::SettingsPrefs;
use crate::features::settings::vault_secrets::{find_vault_secret, vault_secret_missing};

/// What the bar asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionBarEvent {
    /// "Manage command buttons…": open the editor.
    ManageCommands,
}

pub struct ActionBar {
    model: Entity<CommandButtons>,
    group_menu: bool,
    group_anchor: AnchorBounds,
    context_menu: Option<Point<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ActionBarEvent> for ActionBar {}

/// The bar for the window's command state.
pub fn action_bar(model: Entity<CommandButtons>, cx: &mut App) -> Entity<ActionBar> {
    cx.new(|cx| ActionBar::new(model, cx))
}

/// `commandButtonColorSx`: an accented button's ink, border and wash (and
/// their hover and disabled forms); a plain button keeps the outlined
/// primary look.
fn button_tint(button: &CommandButton, disabled: bool, theme: &Theme) -> Option<ButtonTint> {
    let dark = theme.mode.is_dark();
    let ink = command_button_ink(button.color?, theme);
    Some(if disabled {
        ButtonTint {
            ink: alpha(ink, 0.45),
            border: Some(alpha(ink, 0.2)),
            background: Some(alpha(ink, 0.04)),
            hover_border: None,
            hover_background: None,
        }
    } else {
        ButtonTint {
            ink,
            border: Some(alpha(ink, 0.45)),
            background: Some(alpha(ink, if dark { 0.1 } else { 0.07 })),
            hover_border: Some(ink),
            hover_background: Some(alpha(ink, if dark { 0.18 } else { 0.13 })),
        }
    })
}

impl ActionBar {
    pub fn new(model: Entity<CommandButtons>, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = vec![cx.observe(&model, |_, _, cx| cx.notify())];
        if let Some(prefs) = SettingsPrefs::global(cx) {
            subscriptions.push(cx.observe(&prefs, |_, _, cx| cx.notify()));
        }
        Self {
            model,
            group_menu: false,
            group_anchor: AnchorBounds::default(),
            context_menu: None,
            _subscriptions: subscriptions,
        }
    }

    /// Whether the window should show the bar at all: it is switched on and
    /// there is something to show (a button, or a group to pick).
    pub fn visible(cx: &App) -> bool {
        let prefs = prefs(cx);
        prefs.show_command_bar
            && (!prefs.command_buttons.is_empty() || prefs.command_button_groups.len() > 1)
    }

    /// Where the window docks the bar.
    pub fn position(cx: &App) -> CommandBarPosition {
        prefs(cx).command_bar_position
    }

    /// Open the group picker (the examples; the window may bind it to a key).
    pub fn open_group_menu(&mut self, cx: &mut Context<Self>) {
        self.group_menu = true;
        cx.notify();
    }

    fn manage(&mut self, cx: &mut Context<Self>) {
        self.group_menu = false;
        self.context_menu = None;
        cx.emit(ActionBarEvent::ManageCommands);
        cx.notify();
    }

    fn run(&mut self, button: &CommandButton, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.model.read(cx);
        let secret_name = button
            .secret_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .and_then(|id| find_vault_secret(model.vault(), id))
            .map(|secret| secret.name.clone());
        let tab = model.active_id().map(str::to_owned);
        if !run_command_button(tab.as_deref(), button, secret_name.as_deref(), window, cx) {
            show_toast(Status::Warning, "The active terminal is not connected.", cx);
        }
    }
}

impl Render for ActionBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        let prefs = prefs(cx);
        let model = self.model.read(cx);
        let active = model.active_group(cx);
        let connected = model.connected();
        let vault = model.vault().cloned();
        let at_bottom = prefs.command_bar_position == CommandBarPosition::Bottom;
        // Tooltips and menus open toward the panes rather than off the window edge.
        let placement = if at_bottom {
            Placement::Top
        } else {
            Placement::Bottom
        };
        let weak = cx.entity().downgrade();

        let mut bar = div()
            .id("action-bar")
            .relative()
            .h(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .gap(Theme::spacing(0.75))
            .px(Theme::spacing(1.))
            .bg(p.sidebar)
            .border_color(p.divider)
            .on_mouse_down(MouseButton::Right, {
                let weak = weak.clone();
                move |event, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.context_menu = Some(event.position);
                        cx.notify();
                    });
                }
            });
        bar = if at_bottom {
            bar.border_t_1()
        } else {
            bar.border_b_1()
        };

        if active.grouped() {
            let shown = active
                .shown_group()
                .map(|group| group.name.clone())
                .unwrap_or_default();
            let tooltip = match (
                &active.host_name,
                active.host_group_id.as_deref() == Some(active.shown_id.as_str()),
            ) {
                (Some(host), true) => format!("Command group · opened by {host}"),
                _ => "Command group".to_owned(),
            };
            let toggle = weak.clone();
            let ink = p.sidebar_ink;
            bar = bar
                .child(
                    // A flex box, so the probe lies over the button rather than
                    // under it.
                    div()
                        .relative()
                        .flex()
                        .flex_none()
                        .child(
                            Button::new("command-group-button", shown)
                                .tint(ButtonTint {
                                    ink,
                                    border: None,
                                    background: None,
                                    hover_border: None,
                                    hover_background: Some(alpha(ink, p.action.hover_opacity)),
                                })
                                .min_width(0.)
                                .max_width(200.)
                                .padding_x(Theme::spacing_css(1.))
                                .padding_y(Theme::spacing_css(0.25))
                                .end_icon("ExpandMore", Theme::spacing_css(0.25))
                                .tooltip(tooltip)
                                .on_click(move |_, _, cx| {
                                    let _ = toggle.update(cx, |this, cx| {
                                        this.group_menu = !this.group_menu;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(bounds_probe(&self.group_anchor)),
                )
                .child(vertical_divider(&theme, Theme::spacing_css(1.), 0.));
        }

        let shown_buttons: Vec<CommandButton> = active
            .shown_buttons(&prefs.command_buttons)
            .into_iter()
            .cloned()
            .collect();
        let mut strip = div()
            .id("action-bar-buttons")
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .gap(Theme::spacing(0.75))
            .overflow_x_scroll();
        if shown_buttons.is_empty() {
            strip = strip.child(div().px(Theme::spacing(0.5)).child(text(
                "No commands in this group yet.",
                t.caption,
                p.text.secondary,
            )));
        }
        for (index, button) in shown_buttons.into_iter().enumerate() {
            let secret_button = button.secret_id.is_some();
            let secret_id = button.secret_id.clone().unwrap_or_default();
            let secret = (!secret_id.is_empty())
                .then(|| find_vault_secret(vault.as_ref(), &secret_id))
                .flatten();
            let missing = !secret_id.is_empty()
                && vault_secret_missing(vault.as_ref(), &secret_id) == Some(true);
            let disabled = !connected
                || if secret_button {
                    secret_id.is_empty() || missing
                } else {
                    button.command.is_empty()
                };
            let tooltip =
                button_tooltip(&button, secret.map(|secret| secret.name.as_str()), missing);
            let mut element = Button::new(("command-button", index), command_button_label(&button))
                .outlined()
                .min_width(0.)
                .padding_y(Theme::spacing_css(0.25))
                .disabled(disabled)
                .dashed(missing)
                .tooltip(tooltip);
            if let Some(tint) = button_tint(&button, disabled, &theme) {
                element = element.tint(tint);
            }
            if secret_button {
                element = element
                    .start_icon(if missing {
                        "WarningAmberOutlined"
                    } else {
                        "KeyOutlined"
                    })
                    .start_icon_style(16., Theme::spacing_css(0.5));
            }
            let run = weak.clone();
            element = element.on_click(move |_, window, cx| {
                let button = button.clone();
                let _ = run.update(cx, |this, cx| this.run(&button, window, cx));
            });
            strip = strip.child(element);
        }
        bar = bar.child(strip);
        let manage = weak.clone();
        bar = bar.child(
            IconButton::new("manage-command-buttons", "SettingsOutlined")
                .tooltip("Manage command buttons")
                .tooltip_placement(placement)
                .on_click(move |_, _, cx| {
                    let _ = manage.update(cx, |this, cx| this.manage(cx));
                }),
        );

        let mut root = div().relative().flex_none().child(bar);

        // The group picker.
        if self.group_menu
            && let Some(bounds) = self.group_anchor.get()
        {
            follow_anchor(&self.group_anchor, bounds, window, cx);
            let mut items = menu::paper(&theme).min_w(px(220.)).max_w(px(320.));
            for (index, group) in active.groups.iter().enumerate() {
                let id = group.id.clone();
                let pick = weak.clone();
                let model = self.model.clone();
                let count = active.count_in(&prefs.command_buttons, &group.id);
                let opens_with = (active.host_group_id.as_deref() == Some(group.id.as_str()))
                    .then(|| active.host_name.clone())
                    .flatten();
                let check: AnyElement = if group.id == active.shown_id {
                    icon("Check", t.px_to_rem(18.) * 14. / 13., p.text.primary).into_any_element()
                } else {
                    div().into_any_element()
                };
                let mut item = MenuItem::new(("command-group-item", index), group.name.clone())
                    .dense()
                    .trailing(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .ml(Theme::spacing(2.))
                                    .min_w(Theme::spacing(2.))
                                    .flex()
                                    .justify_end()
                                    .child(text(count.to_string(), t.caption, p.text.secondary)),
                            )
                            .child(
                                div()
                                    .w(Theme::spacing(2.25))
                                    .ml(Theme::spacing(1.))
                                    .flex()
                                    .flex_none()
                                    .child(check),
                            ),
                    );
                if let Some(host) = opens_with {
                    item = item.detail(format!("Opens with {host}"));
                }
                items = items.child(item.on_click(move |_, _, cx| {
                    let _ = pick.update(cx, |this, cx| {
                        this.group_menu = false;
                        cx.notify();
                    });
                    model.update(cx, |model, cx| model.select_group(&id, cx));
                }));
            }
            items = items.child(menu_divider(&theme));
            if active.host.is_some() {
                let shown_for_host = active.shown_for_host();
                let model = self.model.clone();
                let close = weak.clone();
                items = items.child(
                    MenuItem::new(
                        "command-group-host",
                        format!(
                            "Always show {} for {}",
                            active
                                .shown_group()
                                .map(|group| group.name.clone())
                                .unwrap_or_else(|| "this group".into()),
                            active.host_name.clone().unwrap_or_default()
                        ),
                    )
                    .dense()
                    .icon(if shown_for_host {
                        "CheckBox"
                    } else {
                        "CheckBoxOutlineBlank"
                    })
                    .icon_color(if shown_for_host {
                        p.primary.main
                    } else {
                        p.action.active
                    })
                    .on_click(move |_, _, cx| {
                        let _ = close.update(cx, |this, cx| {
                            this.group_menu = false;
                            cx.notify();
                        });
                        model.update(cx, |model, cx| model.toggle_host_group(cx));
                    }),
                );
            }
            let manage = weak.clone();
            items = items.child(
                MenuItem::new("command-group-manage", "Manage command buttons…")
                    .dense()
                    .icon("SettingsOutlined")
                    .on_click(move |_, _, cx| {
                        let _ = manage.update(cx, |this, cx| this.manage(cx));
                    }),
            );
            let close = weak.clone();
            let position = if at_bottom {
                bounds.origin
            } else {
                bounds.bottom_left()
            };
            root = root.child(menu::popup_at(
                position,
                if at_bottom {
                    Anchor::BottomLeft
                } else {
                    Anchor::TopLeft
                },
                move |_, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.group_menu = false;
                        cx.notify();
                    });
                },
                items,
            ));
        }

        // Right click on the bar.
        if let Some(position) = self.context_menu {
            let (move_bar, manage, close) = (weak.clone(), weak.clone(), weak.clone());
            let items = menu::paper(&theme)
                .child(
                    MenuItem::new(
                        "command-bar-move",
                        if at_bottom {
                            "Move bar to the top"
                        } else {
                            "Move bar to the bottom"
                        },
                    )
                    .icon(if at_bottom {
                        "VerticalAlignTop"
                    } else {
                        "VerticalAlignBottom"
                    })
                    .on_click(move |_, _, cx| {
                        let _ = move_bar.update(cx, |this, cx| {
                            this.context_menu = None;
                            cx.notify();
                        });
                        update_prefs(cx, |prefs| {
                            prefs.command_bar_position = if at_bottom {
                                CommandBarPosition::Top
                            } else {
                                CommandBarPosition::Bottom
                            };
                        });
                    }),
                )
                .child(
                    MenuItem::new("command-bar-manage", "Manage command buttons…")
                        .icon("SettingsOutlined")
                        .on_click(move |_, _, cx| {
                            let _ = manage.update(cx, |this, cx| this.manage(cx));
                        }),
                );
            root = root.child(menu::popup(
                position,
                move |_, cx| {
                    let _ = close.update(cx, |this, cx| {
                        this.context_menu = None;
                        cx.notify();
                    });
                },
                items,
            ));
        }
        root
    }
}
