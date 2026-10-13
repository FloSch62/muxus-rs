//! `DebugSection` of `SettingsDialog.tsx`: verbose logging and the
//! application log (view and export).

use muxus_ui::gpui_kit::{AnyElement, Context, IntoElement, Window};
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::Button;

use super::kit::{group, page, row, secondary_text};
use super::window::SettingsWindow;

impl SettingsWindow {
    pub fn debug(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let view_logs = Button::new("settings-view-logs", "View logs")
            .outlined()
            .start_icon("ArticleOutlined")
            .on_click(cx.listener(|this, _, window, cx| {
                match super::super::hooks::settings_hooks(cx).open_log_viewer {
                    Some(open) => open(window, cx),
                    None => {
                        this.show_toast(Status::Info, "The log viewer is not available yet.", cx)
                    }
                }
            }));
        let export_logs = Button::new("settings-export-logs", "Export logs")
            .outlined()
            .start_icon("DownloadOutlined")
            .on_click(cx.listener(|this, _, window, cx| this.export_logs(window, cx)));
        page(
            &theme,
            "Debug",
            Some(secondary_text(
                &theme,
                "Muxus keeps its own log in memory on this machine. It never leaves it unless you export it, for example to attach to a bug report.",
            )),
            vec![group("")
                .row(self.switch_row(
                    &theme,
                    "settings-debug-mode",
                    "Capture verbose diagnostic logs",
                    Some("Records connection-level detail: every dial, authentication step, SSH agent wait and the raw error behind a failure. Warnings and errors are always captured, even while this is off."),
                    prefs.debug_mode,
                    |p, value| p.debug_mode = value,
                ))
                .row(
                    row(&theme, "Application log")
                        .description_text(
                            &theme,
                            "App startup and every connection attempt since launch. If the desktop app fails to start at all, its shell also writes logs/main.log in the app data directory.",
                        )
                        .control(view_logs)
                        .control(export_logs)
                        .render(),
                )
                .render(&theme)],
        )
        .into_any_element()
    }

    /// `exportLogs`: the in-memory log as a text file the user saves.
    fn export_logs(&mut self, _window: &mut Window, cx: &mut muxus_ui::gpui_kit::Context<Self>) {
        let backend = self.backend.clone();
        self.fetch(
            cx,
            async move { backend.app_logs().await },
            move |this, result, cx| match result {
                Ok(logs) => {
                    let now = muxus_core::js::now_iso();
                    let mut lines = vec![
                        format!("# Muxus diagnostic log \u{2014} exported {now}"),
                        format!(
                            "# {} entries, debug logging {}",
                            logs.entries.len(),
                            if logs.debug_enabled { "on" } else { "off" }
                        ),
                    ];
                    lines.extend(
                        logs.entries
                            .iter()
                            .map(super::super::files::format_log_entry),
                    );
                    let text = lines.join("\n");
                    let name = super::super::files::export_filename("debug log", "log");
                    this.save_text_file(name, text, cx);
                }
                Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
            },
        );
    }
}
