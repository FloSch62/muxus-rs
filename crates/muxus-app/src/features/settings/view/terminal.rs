//! `TerminalSection` of `SettingsDialog.tsx`: cursor, clipboard and mouse,
//! paste pacing, buffer and rendering.

use muxus_store::prefs::state::{CursorStyle, RightClickAction, TerminalFileLinkActivation};
use muxus_term::paste::{PasteDelayKind, parse_paste_delay};
use muxus_ui::gpui_kit::{AnyElement, Context, IntoElement, Window};
use muxus_ui::theme::ActiveTheme;

use super::super::js::number_to_string;
use super::fields::field;
use super::kit::{ToggleItem, ToggleStyle, group, page, row, secondary_text, toggle_group};
use super::window::SettingsWindow;

const IS_MAC: bool = cfg!(target_os = "macos");

/// `terminalFileLinkActivationOptions`: gestures this platform can produce.
pub fn link_activation_options() -> Vec<(TerminalFileLinkActivation, &'static str)> {
    if IS_MAC {
        vec![
            (TerminalFileLinkActivation::Alt, "Option + left click"),
            (TerminalFileLinkActivation::Meta, "Cmd + left click"),
            (TerminalFileLinkActivation::Direct, "Direct left click"),
        ]
    } else {
        vec![
            (TerminalFileLinkActivation::Alt, "Alt + left click"),
            (TerminalFileLinkActivation::Ctrl, "Ctrl + left click"),
            (TerminalFileLinkActivation::Direct, "Direct left click"),
        ]
    }
}

/// `terminalFileLinkActivationForPlatform`: map a backup from another
/// platform to a gesture that is safe here.
pub fn link_activation_for_platform(
    activation: TerminalFileLinkActivation,
    is_mac: bool,
) -> TerminalFileLinkActivation {
    match activation {
        TerminalFileLinkActivation::Meta if !is_mac => TerminalFileLinkActivation::Alt,
        TerminalFileLinkActivation::Ctrl if is_mac => TerminalFileLinkActivation::Meta,
        other => other,
    }
}

fn activation_id(activation: TerminalFileLinkActivation) -> &'static str {
    match activation {
        TerminalFileLinkActivation::Direct => "direct",
        TerminalFileLinkActivation::Alt => "alt",
        TerminalFileLinkActivation::Ctrl => "ctrl",
        TerminalFileLinkActivation::Meta => "meta",
    }
}

fn right_click_id(action: RightClickAction) -> &'static str {
    match action {
        RightClickAction::CopyPaste => "copy-paste",
        RightClickAction::Paste => "paste",
        RightClickAction::Menu => "menu",
    }
}

impl SettingsWindow {
    pub fn terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();

        let cursor = toggle_group(
            "cursor-style",
            &theme,
            vec![
                ToggleItem::text("\u{2589} Block"),
                ToggleItem::text("\u{2581} Underline"),
                ToggleItem::text("\u{258f} Bar"),
            ],
            Some(match prefs.cursor_style {
                CursorStyle::Block => 0,
                CursorStyle::Underline => 1,
                CursorStyle::Bar => 2,
            }),
            ToggleStyle::MONO,
            self.pref_toggle(|p, index| {
                p.cursor_style =
                    [CursorStyle::Block, CursorStyle::Underline, CursorStyle::Bar][index];
            }),
        );
        let cursor_group = group("Cursor")
            .row(row(&theme, "Cursor style").control(cursor).render())
            .row(self.switch_row(
                &theme,
                "settings-cursor-blink",
                "Blinking cursor",
                None,
                prefs.cursor_blink,
                |p, value| p.cursor_blink = value,
            ))
            .render(&theme);

        let right_click = self.select_row(
            &theme,
            "Right-click",
            Some("What a right-click in a terminal does."),
            "right-click",
            280.,
            vec![
                (
                    "copy-paste".into(),
                    "Copy selection, otherwise paste".into(),
                ),
                ("paste".into(), "Always paste".into()),
                ("menu".into(), "Show context menu".into()),
            ],
            right_click_id(prefs.right_click_action),
            cx,
            |this, value, _, cx| {
                let action = match value.as_str() {
                    "paste" => RightClickAction::Paste,
                    "menu" => RightClickAction::Menu,
                    _ => RightClickAction::CopyPaste,
                };
                this.set_prefs(cx, |p| p.right_click_action = action);
            },
        );
        let options = link_activation_options();
        let links = self.select_row(
            &theme,
            "Open terminal links",
            Some("The click that opens file paths and web addresses. A modifier leaves plain clicks free for selecting text."),
            "terminal-links",
            280.,
            options
                .iter()
                .map(|(value, text)| (activation_id(*value).to_string(), text.to_string()))
                .collect(),
            activation_id(link_activation_for_platform(
                prefs.terminal_file_link_activation,
                IS_MAC,
            )),
            cx,
            move |this, value, _, cx| {
                if let Some((activation, _)) = link_activation_options()
                    .into_iter()
                    .find(|(activation, _)| activation_id(*activation) == value)
                {
                    this.set_prefs(cx, |p| p.terminal_file_link_activation = activation);
                }
            },
        );
        let clipboard = group("Clipboard & mouse")
            .row(right_click)
            .row(links)
            .row(self.switch_row(
                &theme,
                "settings-copy-on-select",
                "Copy on select",
                Some("Copy selected terminal text to the clipboard automatically."),
                prefs.copy_on_select,
                |p, value| p.copy_on_select = value,
            ))
            .row(self.switch_row(
                &theme,
                "settings-osc52",
                "Allow terminal clipboard writes (OSC 52)",
                Some("Lets terminal programs such as tmux and Zellij replace the system clipboard. Clipboard reads remain blocked."),
                prefs.allow_osc52_clipboard_write,
                |p, value| p.allow_osc52_clipboard_write = value,
            ))
            .row(self.switch_row(
                &theme,
                "settings-paste-warn",
                "Confirm multiline pastes",
                Some("Preview before pasted text can run several shell commands."),
                prefs.paste_warn_multiline,
                |p, value| p.paste_warn_multiline = value,
            ))
            .render(&theme);

        let line_delay = number_to_string(prefs.paste_line_delay_ms);
        let line_input = self.input(
            "paste-line-delay",
            || line_delay.clone(),
            None,
            false,
            window,
            cx,
            |this, value, _, cx| {
                let delay = parse_paste_delay(&value, PasteDelayKind::Line).unwrap_or(0);
                this.set_prefs(cx, |p| p.paste_line_delay_ms = f64::from(delay));
            },
        );
        self.sync_input("paste-line-delay", &line_delay, window, cx);
        let char_delay = number_to_string(prefs.paste_char_delay_ms);
        let char_input = self.input(
            "paste-char-delay",
            || char_delay.clone(),
            None,
            false,
            window,
            cx,
            |this, value, _, cx| {
                let delay = parse_paste_delay(&value, PasteDelayKind::Char).unwrap_or(0);
                this.set_prefs(cx, |p| p.paste_char_delay_ms = f64::from(delay));
            },
        );
        self.sync_input("paste-char-delay", &char_delay, window, cx);
        let pacing = group("Paste pacing")
            .row(
                row(&theme, "Delay after each line")
                    .description_text(
                        &theme,
                        "Type pastes in a line at a time, for consoles and network devices that drop input sent all at once. 0 sends pastes at once. Hosts can set their own under Terminal appearance.",
                    )
                    .control(
                        field("settings-paste-line-delay", &line_input)
                            .width(140.)
                            .end_text("ms")
                            .render(&theme, window, cx),
                    )
                    .render(),
            )
            .row(
                row(&theme, "Delay after each character")
                    .description_text(
                        &theme,
                        "Also wait after every character, for devices that cannot keep up within a line.",
                    )
                    .control(
                        field("settings-paste-char-delay", &char_input)
                            .width(140.)
                            .end_text("ms")
                            .render(&theme, window, cx),
                    )
                    .render(),
            )
            .render(&theme);

        let scrollback = number_to_string(prefs.scrollback);
        let scrollback_input = self.input(
            "scrollback",
            || scrollback.clone(),
            None,
            false,
            window,
            cx,
            |this, value, _, cx| {
                let lines = super::super::draft::js_number(&value);
                if lines.fract() == 0. && (0. ..=1_000_000.).contains(&lines) {
                    this.set_prefs(cx, |p| p.scrollback = lines);
                }
            },
        );
        self.sync_input("scrollback", &scrollback, window, cx);
        let buffer = group("Buffer & rendering")
            .row(
                row(&theme, "Scrollback lines")
                    .description_text(&theme, "How much output each terminal keeps above the screen.")
                    .control(
                        field("settings-scrollback", &scrollback_input)
                            .width(140.)
                            .render(&theme, window, cx),
                    )
                    .render(),
            )
            .row(self.switch_row(
                &theme,
                "settings-webgl",
                "GPU renderer (WebGL)",
                Some("Paints terminals on the GPU instead of the DOM: smoother under heavy output, slightly more CPU while idle. Where WebGL is unavailable, terminals keep the standard renderer."),
                prefs.webgl_renderer,
                |p, value| p.webgl_renderer = value,
            ))
            .render(&theme);

        page(
            &theme,
            "Terminal",
            Some(secondary_text(
                &theme,
                "How terminals behave. Changes apply to open terminals too.",
            )),
            vec![cursor_group, clipboard, pacing, buffer],
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_gestures_map_across_platforms() {
        assert_eq!(
            link_activation_for_platform(TerminalFileLinkActivation::Meta, false),
            TerminalFileLinkActivation::Alt
        );
        assert_eq!(
            link_activation_for_platform(TerminalFileLinkActivation::Ctrl, true),
            TerminalFileLinkActivation::Meta
        );
        assert_eq!(
            link_activation_for_platform(TerminalFileLinkActivation::Direct, true),
            TerminalFileLinkActivation::Direct
        );
    }
}
