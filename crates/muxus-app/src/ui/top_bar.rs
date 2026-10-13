//! `client/src/layout/TopBar.tsx`: the 52px toolbar that doubles as the
//! window's title bar. It is a drag region; a double-click maximizes; the
//! window controls sit at its right edge, as Electron's title bar overlay
//! puts them on Linux.

use muxus_app::keymap::bindings::KeybindingOverrides;
use muxus_app::keymap::hints::chord_label;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, InteractiveElement, IntoElement, ParentElement, Styled, Window, div,
    px,
};
use muxus_ui::icon::SMALL;
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, layout};
use muxus_ui::widgets::display::badge;
use muxus_ui::widgets::menu::{self, MenuItem};
use muxus_ui::widgets::{Button, ButtonColor, IconButton};

use crate::ui::prefs::{THEME_MODE_OPTIONS, theme_mode_icon, theme_mode_label};
use crate::ui::shell::{AppShell, ShellMenu};
use muxus_ui::widgets::window_frame::{title_bar, window_controls, window_corner_radius};

/// `withChord`: "New tab · Ctrl+Shift+T".
pub fn with_chord(label: &str, command: &str) -> String {
    match chord_label(command, &KeybindingOverrides::default()) {
        Some(chord) => format!("{label} · {chord}"),
        None => label.to_string(),
    }
}

impl AppShell {
    pub fn render_top_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let height = if self.focus_mode {
            layout::FOCUS_TOP_BAR_HEIGHT
        } else {
            layout::TOP_BAR_HEIGHT
        };
        let active_tab = self.tabs.active_tab().cloned();
        let ssh_ready = active_tab
            .as_ref()
            .is_some_and(|tab| tab.conn_id.is_some() && tab.sftp_available != Some(false));
        let terminal_ready = active_tab.as_ref().is_some_and(|tab| tab.profile.is_some());
        // With the file browser docked in it, the sidebar is more than the hosts.
        let sidebar_name = "hosts";
        let collapsed = self.prefs.sidebar_collapsed;
        let mode = self.prefs.theme_mode;

        let bar = div()
            .id("top-bar")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(height))
            .pl(px(16.))
            .pr(px(16.));
        let mut bar = title_bar(bar);

        if self.focus_mode {
            bar = bar.child(div().flex_1());
        } else {
            bar = bar
                .child(
                    div().mr(px(4.)).child(
                        IconButton::new("toggle-sidebar", "Menu")
                            .tooltip(with_chord(
                                &format!(
                                    "{} {sidebar_name}",
                                    if collapsed { "Show" } else { "Hide" }
                                ),
                                "app.sidebar",
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.prefs.sidebar_collapsed = !this.prefs.sidebar_collapsed;
                                this.prefs_changed(cx);
                                cx.notify();
                            })),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .mr(px(12.))
                        .child(muxus_ui::icon::logo(28., window, cx))
                        .child(
                            label("Muxus")
                                .size(theme.typography.h6.size)
                                .weight(700.)
                                .line_height(theme.typography.h6.line_height)
                                .color(p.text.primary)
                                .no_truncate(),
                        ),
                )
                .child(
                    Button::new("workspaces", self.workspace_name.clone())
                        .color(ButtonColor::Inherit)
                        .start_icon("WorkspacesOutlined")
                        .disabled(
                            self.workspaces
                                .as_ref()
                                .is_some_and(|workspaces| !workspaces.read(cx).state().ready),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_workspaces(window, cx);
                        }))
                        .label_element(
                            div().flex().min_w_0().max_w(px(240. - 10. - 26.)).child(
                                label(self.workspace_name.clone())
                                    .size(theme.typography.body2.size)
                                    .line_height(theme.typography.body2.line_height)
                                    .color(p.text.primary),
                            ),
                        ),
                )
                .child(div().flex_1())
                .children(self.features.multi_exec_control.clone())
                .child(
                    IconButton::new("command-buttons", "BoltOutlined")
                        .tooltip("Saved command buttons")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_command_buttons(window, cx);
                        })),
                );
            if ssh_ready {
                let shown = active_tab
                    .as_ref()
                    .is_some_and(|tab| self.file_browser_shown(&tab.id));
                let tab_id = active_tab.as_ref().map(|tab| tab.id.clone());
                bar = bar.child(
                    IconButton::new("file-browser", "FolderOutlined")
                        .tooltip(if shown {
                            "Hide file browser"
                        } else {
                            "Show file browser (SFTP)"
                        })
                        .color(if shown {
                            ButtonColor::Primary
                        } else {
                            ButtonColor::Default
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(id) = &tab_id {
                                this.toggle_file_browser(id, cx);
                            }
                        })),
                );
            }
            let forwarding_open = self.features.forwarding_open;
            let summary = self.forwards;
            let mut forwarding = IconButton::new("forwarding", "SwapHorizOutlined")
                .tooltip(summary.tooltip())
                .color(if forwarding_open {
                    ButtonColor::Primary
                } else {
                    ButtonColor::Default
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_forwarding_open(!forwarding_open, cx);
                }));
            // MUI hides a badge whose content is 0; it turns amber while a forward is
            // interrupted.
            if summary.total > 0 {
                let (fill, ink) = if summary.warns() {
                    (p.warning.main, p.warning.contrast_text)
                } else {
                    (p.success.main, p.success.contrast_text)
                };
                forwarding = forwarding.badge(badge(summary.total.min(99), fill, ink, 5., SMALL));
            }
            bar = bar.child(forwarding);
            if terminal_ready {
                bar = bar.child(
                    IconButton::new("terminal-actions", "Terminal")
                        .tooltip("Terminal actions")
                        .track_bounds(&self.anchors.terminal)
                        .on_click(cx.listener(
                            |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                                let position = menu::below(&this.anchors.terminal)
                                    .unwrap_or_else(|| event.mouse_position().unwrap_or_default());
                                this.menu = Some(ShellMenu::TerminalActions { position });
                                cx.notify();
                            },
                        )),
                );
            }
            bar = bar
                .child(
                    IconButton::new("history", "HistoryOutlined")
                        .tooltip("Session history")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_history("", None, window, cx);
                        })),
                )
                .child(
                    IconButton::new("appearance", theme_mode_icon(mode))
                        .tooltip(format!("Appearance: {}", theme_mode_label(mode)))
                        .track_bounds(&self.anchors.appearance)
                        .on_click(cx.listener(
                            |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                                let position = menu::below(&this.anchors.appearance)
                                    .unwrap_or_else(|| event.mouse_position().unwrap_or_default());
                                this.menu = Some(ShellMenu::Appearance { position });
                                cx.notify();
                            },
                        )),
                )
                .child(
                    IconButton::new("shortcuts", "KeyboardOutlined")
                        .tooltip("Keyboard shortcuts")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_shortcuts(window, cx);
                        })),
                )
                .child(
                    IconButton::new("settings", "SettingsOutlined")
                        .tooltip("Settings")
                        .on_click(cx.listener(|this, _, _, cx| this.open_settings(None, cx))),
                );
        }
        bar = bar.child(
            IconButton::new(
                "focus-mode",
                if self.focus_mode {
                    "FullscreenExitOutlined"
                } else {
                    "CenterFocusStrongOutlined"
                },
            )
            .color(if self.focus_mode {
                ButtonColor::Primary
            } else {
                ButtonColor::Default
            })
            .tooltip(with_chord(
                if self.focus_mode {
                    "Exit focus mode"
                } else {
                    "Enter focus mode"
                },
                "app.focus-mode",
            ))
            .on_click(cx.listener(|this, _, _, cx| {
                this.focus_mode = !this.focus_mode;
                this.menu = None;
                cx.notify();
            })),
        );
        if let Some(controls) = window_controls(window, cx) {
            // The toolbar keeps its 16px gutter before the controls overlay
            // (12px of flex gap plus 4).
            bar = bar
                .pr(px(0.))
                .child(div().ml(px(4.)).flex_none().child(controls));
        }

        // The bar's own fill follows the window frame's rounded top corners.
        let radius = window_corner_radius(window);
        div()
            .flex_none()
            .bg(p.sidebar)
            .rounded_t(px(radius))
            .border_b_1()
            .border_color(p.divider)
            .child(bar)
            .into_any_element()
    }

    pub fn render_shell_menu(
        &mut self,
        menu: ShellMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let shell = cx.entity().downgrade();
        let dismiss = move |_: &mut Window, cx: &mut muxus_ui::gpui_kit::App| {
            let _ = shell.update(cx, |this, cx| this.close_menu(cx));
        };
        let (position, content): (_, Div) = match menu {
            ShellMenu::Appearance { position } => {
                let current = self.prefs.theme_mode;
                let mut paper = menu::paper(&theme);
                for mode in THEME_MODE_OPTIONS {
                    paper = paper.child(
                        MenuItem::new(
                            format!("appearance-{}", theme_mode_label(mode)),
                            theme_mode_label(mode),
                        )
                        .icon(theme_mode_icon(mode))
                        .selected(mode == current)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.menu = None;
                                this.set_theme_mode(mode, window, cx);
                            },
                        )),
                    );
                }
                (position, paper)
            }
            ShellMenu::TerminalActions { position } => (position, self.terminal_actions_menu(cx)),
            ShellMenu::Terminal {
                tab_id,
                position,
                selection,
            } => (position, self.terminal_menu(&tab_id, &selection, cx)),
            ShellMenu::Tab { tab_id, position } => (position, self.tab_menu(&tab_id, window, cx)),
            ShellMenu::Pane { pane_id, position } => (position, self.pane_menu(&pane_id, cx)),
            ShellMenu::AllTabs { pane_id, position } => {
                (position, self.all_tabs_menu(&pane_id, cx))
            }
            ShellMenu::HostSessions { host, position } => {
                // Toward the panes: from the row's right edge, or its left edge when the
                // sidebar docks right.
                let content = self.host_sessions_menu(&host, cx);
                let corner = if self.prefs.sidebar_position == crate::ui::prefs::Side::Right {
                    muxus_ui::gpui_kit::Anchor::TopRight
                } else {
                    muxus_ui::gpui_kit::Anchor::TopLeft
                };
                return menu::popup_at(position, corner, dismiss, content).into_any_element();
            }
            ShellMenu::StatusBar { position } => {
                // Opens upward from the bar: its bottom-left corner on the point.
                let content = self.status_bar_menu(cx);
                return menu::popup_at(
                    position,
                    muxus_ui::gpui_kit::Anchor::BottomLeft,
                    dismiss,
                    content,
                )
                .into_any_element();
            }
        };
        menu::popup(position, dismiss, content).into_any_element()
    }
}
