//! `components/X11Section.tsx`: the master switch (off by default on macOS,
//! which needs XQuartz first), the default for hosts without their own
//! ForwardX11, and clipboard sharing for the X server built into Windows.

use muxus_core::api::X11Source;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, Styled, Window, div, px,
};
use muxus_ui::theme::ActiveTheme;

use super::super::texts;
use super::kit::{alert_text, group, outlined_alert, page, row, secondary_text, skeleton};
use super::window::SettingsWindow;

const DESCRIPTION: &str =
    "Graphical programs started in SSH sessions open their windows on this computer.";

impl SettingsWindow {
    pub fn x11(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let Some(status) = self.data.x11.clone() else {
            return page(
                &theme,
                "X11 forwarding",
                Some(secondary_text(&theme, DESCRIPTION)),
                vec![
                    skeleton(&theme, 88.).into_any_element(),
                    skeleton(&theme, 72.).into_any_element(),
                ],
            )
            .into_any_element();
        };
        let enabled = prefs.x11_enabled.unwrap_or(status.defaults.enabled);
        let forward_by_default = prefs
            .x11_forward_by_default
            .unwrap_or(status.defaults.forward_by_default);
        let platform = self
            .data
            .app_info
            .as_ref()
            .map(|info| info.platform.clone());

        let mut master = row(&theme, "Enable X11 forwarding")
            .description_text(
                &theme,
                "Off: Muxus never requests X11, whatever a host's ForwardX11 says, and shows no X11 hints.",
            )
            .control(self.pref_switch("settings-x11-enabled", enabled, false, |p, value| {
                p.x11_enabled = Some(value)
            }));
        if enabled {
            let (tone, text) = texts::x11_server_status(&status, platform.as_deref());
            master = master.child(div().mt(px(10.)).child(outlined_alert(
                &theme,
                tone,
                alert_text(&theme, tone, text),
            )));
        }
        let mut defaults = group("Defaults").row(
            row(&theme, "Forward X11 by default")
                .disabled(&theme, "Forward X11 by default", !enabled)
                .description_text(&theme, texts::x11_default_description(status.source))
                .control(self.pref_switch(
                    "settings-x11-default",
                    forward_by_default,
                    !enabled,
                    |p, value| p.x11_forward_by_default = Some(value),
                ))
                .render(),
        );
        if status.source == X11Source::Bundled {
            defaults = defaults.row(
                row(&theme, "Share the clipboard with X11 apps")
                    .disabled(&theme, "Share the clipboard with X11 apps", !enabled)
                    .description_text(
                        &theme,
                        "Copy and paste between forwarded windows and Windows. Every server you connect to with X11 forwarding can then read and replace your clipboard, so turn this on only if you trust all of them. Applies to a connection once it has no forwarded windows open.",
                    )
                    .control(self.pref_switch(
                        "settings-x11-clipboard",
                        prefs.x11_clipboard_sharing,
                        !enabled,
                        |p, value| p.x11_clipboard_sharing = value,
                    ))
                    .render(),
            );
        }
        page(
            &theme,
            "X11 forwarding",
            Some(secondary_text(&theme, DESCRIPTION)),
            vec![
                group("").row(master.render()).render(&theme),
                defaults.render(&theme),
            ],
        )
        .into_any_element()
    }
}
