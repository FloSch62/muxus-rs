//! `KeyboardSection` of `SettingsDialog.tsx`: split behavior, numbered tabs
//! and the entry point to the full shortcut editor.

use muxus_store::prefs::state::TabNumberVisibility;
use muxus_ui::gpui_kit::{
    AnyElement, AppContext, Context, IntoElement, ParentElement, Styled, Window, div, px,
};
use muxus_ui::label::label;
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::Button;

use super::super::texts;
use super::kit::{BODY2, BODY2_LINE, chord_text, group, page, secondary_text, snap};
use super::shortcuts::{ShortcutsDialog, ShortcutsEvent};
use super::window::SettingsWindow;

impl SettingsWindow {
    pub fn keyboard(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let customized = prefs.keybindings.len();
        let numbers = self.select_row(
            &theme,
            "Show tab numbers",
            Some("Tabs are numbered across the whole window and update when tabs or panes move."),
            "tab-numbers",
            200.,
            vec![
                ("shortcut".into(), "While Alt is held".into()),
                ("always".into(), "Always".into()),
            ],
            match prefs.tab_number_visibility {
                TabNumberVisibility::Shortcut => "shortcut",
                TabNumberVisibility::Always => "always",
            },
            cx,
            |this, value, _, cx| {
                let visibility = if value == "always" {
                    TabNumberVisibility::Always
                } else {
                    TabNumberVisibility::Shortcut
                };
                this.set_prefs(cx, |p| p.tab_number_visibility = visibility);
            },
        );
        let summary = |text: &str, chord: Option<String>| {
            div()
                .flex()
                .items_center()
                .gap(px(16.))
                .px(px(16.))
                .py(px(8.))
                .child(
                    label(text.to_string())
                        .size(BODY2)
                        .line_height(snap(BODY2_LINE))
                        .color(theme.palette.text.primary)
                        .grow(),
                )
                .child(chord_text(
                    chord.unwrap_or_else(|| "Unbound".into()),
                    theme.palette.text.primary,
                ))
                .into_any_element()
        };
        let rows = vec![
            summary(
                "Split the focused pane in any direction",
                self.chord("pane.split.right", cx),
            ),
            summary(
                "Move focus between panes",
                self.chord("pane.focus.right", cx),
            ),
            summary(
                "Jump to a window-wide numbered tab",
                self.chord("tab.select.1", cx),
            ),
            summary(
                "Send the current tab to another pane",
                self.chord("tab.to-pane.right", cx),
            ),
            summary(
                "Zoom a pane / restore the layout",
                self.chord("pane.zoom", cx),
            ),
        ];
        let all = Button::new(
            "settings-all-shortcuts",
            texts::all_shortcuts_label(customized),
        )
        .start_icon("KeyboardOutlined")
        .on_click(cx.listener(|this, _, window, cx| this.open_shortcuts(window, cx)));
        page(
            &theme,
            "Keyboard",
            Some(secondary_text(
                &theme,
                "How splits and numbered tabs behave, and every shortcut in one place.",
            )),
            vec![
                group("Panes & tabs")
                    .row(self.switch_row(
                        &theme,
                        "settings-split-inherits",
                        "New splits continue the current session",
                        Some("Splitting opens a second session on the same host (SSH reuses the live connection). Off: the new pane asks what to start. Serial consoles always ask."),
                        prefs.split_inherits_session,
                        |p, value| p.split_inherits_session = value,
                    ))
                    .row(numbers)
                    .render(&theme),
                group("Layout keys").action(all).rows(rows).render(&theme),
            ],
        )
        .into_any_element()
    }

    /// Show the keyboard shortcuts editor over this window.
    pub fn open_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcuts.is_some() {
            return;
        }
        let prefs = self.prefs.clone();
        let dialog = cx.new(|cx| ShortcutsDialog::new(prefs, window, cx));
        let subscription =
            cx.subscribe_in(&dialog, window, |this, _, event, window, cx| match event {
                ShortcutsEvent::Close => {
                    this.shortcuts = None;
                    window.focus(&this.focus_handle_ref().clone(), cx);
                    cx.notify();
                }
            });
        self.keep_subscription(subscription);
        self.shortcuts = Some(dialog);
        cx.notify();
    }
}
