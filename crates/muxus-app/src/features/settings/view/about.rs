//! `components/AboutSection.tsx` and the update row of
//! `components/DesktopUpdateControls.tsx`: what this build is, where to read
//! more, whether a newer build exists, and who makes it. Links open in the
//! system browser.

use muxus_core::api::{
    DesktopUpdateSource, DesktopUpdateState, DesktopUpdateStatus, UpdateCheckResult,
};
use muxus_ui::color::alpha;
use muxus_ui::gpui_kit::{
    AnyElement, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, linear_color_stop, linear_gradient, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::Button;

use super::super::hooks::{DesktopUpdateHooks, settings_hooks};
use super::super::texts::{self, DOCS_URL};
use super::kit::{BODY2, CAPTION, group, row, snap, status_text};
use super::window::SettingsWindow;

const SOURCE: &str = "https://github.com/FloSch62/muxus";
const ISSUES: &str = "https://github.com/FloSch62/muxus/issues/new";
const LICENSE: &str = "https://github.com/FloSch62/muxus/blob/main/LICENSE";
const AUTHOR: &str = "https://flosch.me/";
const AUTHOR_GITHUB: &str = "https://github.com/FloSch62";
const AUTHOR_LINKEDIN: &str = "https://www.linkedin.com/in/florian-schwarz-812a34145/";
const COFFEE: &str = "https://www.buymeacoffee.com/FloSch62";

/// The plain update check (no in-app updater).
#[derive(Default)]
pub struct UpdateState {
    pub checking: bool,
    pub result: Option<UpdateCheckResult>,
}

/// What the plain update row says (`UpdateControls`' status).
pub fn update_status(state: &UpdateState) -> (Option<Status>, String) {
    if state.checking {
        return (
            None,
            "Checking GitHub for the latest release\u{2026}".into(),
        );
    }
    match &state.result {
        Some(UpdateCheckResult::Available {
            current_version,
            latest_version,
            ..
        }) => (
            Some(Status::Info),
            format!("Muxus {latest_version} is available. You are running {current_version}."),
        ),
        Some(UpdateCheckResult::NotAvailable {
            latest_version: Some(latest),
            ..
        }) => (
            Some(Status::Success),
            format!("Muxus is up to date. Latest release: {latest}."),
        ),
        Some(UpdateCheckResult::NotAvailable { reason, .. }) => (
            Some(if matches!(reason.as_deref(), Some("store" | "disabled")) {
                Status::Info
            } else {
                Status::Warning
            }),
            texts::update_reason_label(reason.as_deref()),
        ),
        None => (
            None,
            "Looks for a newer release on GitHub when you ask.".into(),
        ),
    }
}

/// What the in-app updater row says (`releaseStatus` / `storeStatus`).
pub fn desktop_update_status(state: &DesktopUpdateState) -> (Option<Status>, String) {
    let store = state.source == Some(DesktopUpdateSource::Store);
    let version = state.version.clone().unwrap_or_default();
    let percent = state.percent.map(texts_percent);
    use DesktopUpdateStatus::*;
    if store {
        return match state.status {
            Checking => (None, "Asking Microsoft Store for updates\u{2026}".into()),
            Available => (
                Some(Status::Info),
                "A new version of Muxus is available in Microsoft Store.".into(),
            ),
            Installing => (
                None,
                match percent {
                    None => "Waiting for Microsoft Store\u{2026}".into(),
                    Some(percent) => {
                        format!("Updating Muxus through Microsoft Store\u{2026} {percent}%")
                    }
                },
            ),
            Updated => (
                Some(Status::Success),
                "Microsoft Store completed the update. Reopen Muxus to use the installed version."
                    .into(),
            ),
            UpToDate => (
                Some(Status::Success),
                "Microsoft Store reports no available updates.".into(),
            ),
            Error => (
                Some(Status::Warning),
                state.error.clone().unwrap_or_default(),
            ),
            _ => (
                None,
                "Microsoft Store manages updates for this installation.".into(),
            ),
        };
    }
    match state.status {
        Checking => (None, "Checking GitHub for the latest release\u{2026}".into()),
        Available => (
            Some(Status::Info),
            format!(
                "Muxus {version} is available. You are running {}.",
                state.current_version
            ),
        ),
        Downloading => (
            None,
            format!(
                "Downloading Muxus {version}\u{2026} {}%",
                percent.unwrap_or_else(|| "0".into())
            ),
        ),
        Ready => (
            Some(Status::Info),
            format!("Muxus {version} is downloaded. Restart Muxus to install it."),
        ),
        Installing => (None, "Restarting Muxus to install the update\u{2026}".into()),
        UpToDate => (Some(Status::Success), "Muxus is up to date.".into()),
        Error => (Some(Status::Warning), state.error.clone().unwrap_or_default()),
        _ => (
            None,
            "Downloads and installation start only when you choose them. Quitting normally never installs an update."
                .into(),
        ),
    }
}

fn texts_percent(value: f64) -> String {
    super::super::js::number_to_string(value)
}

impl SettingsWindow {
    pub fn about(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let info = self.data.app_info.clone();
        let version = info.as_ref().map(|info| info.version.clone());
        let hero = self.about_hero(&theme, version.as_deref(), window, cx);
        let updates = self.update_controls(&theme, cx);
        let facts = texts::installation_facts(info.as_ref());
        let copy_text = facts
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut keys = div().flex().flex_col().gap(px(6.)).min_w(px(110.));
        let mut values = div().flex().flex_col().gap(px(6.)).flex_1().min_w_0();
        for (key, value) in &facts {
            keys = keys.child(
                label(*key)
                    .size(BODY2)
                    .line_height(snap(BODY2 * 1.43))
                    .color(p.text.secondary),
            );
            values = values.child(
                label(value.clone())
                    .size(BODY2)
                    .line_height(snap(BODY2 * 1.43))
                    .tabular_nums()
                    .color(p.text.primary),
            );
        }
        let installation = group("This installation")
            .action(
                Button::new("about-copy-diagnostics", "Copy diagnostics")
                    .start_icon("ContentCopyOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        this.show_toast(Status::Success, "Diagnostics copied.", cx);
                    })),
            )
            .row(
                div()
                    .flex()
                    .gap(px(24.))
                    .px(px(16.))
                    .py(px(10.))
                    .child(keys)
                    .child(values),
            )
            .render(&theme);
        let link = |id: &'static str, text: &'static str, url: &'static str| {
            div()
                .id(id)
                .cursor_pointer()
                .text_size(px(BODY2))
                .line_height(px(snap(BODY2 * 1.43)))
                .text_color(p.primary.main)
                .underline()
                .text_decoration_color(alpha(p.primary.main, 0.4))
                .on_click(move |_, _, cx| cx.open_url(url))
                .child(text)
        };
        let made_by = group("Made by")
            .row(
                row(&theme, "FloSch")
                    .description_text(
                        &theme,
                        "Muxus is built in the open, in my spare time. Bug reports, ideas and pull requests are always welcome.",
                    )
                    .child(
                        div()
                            .mt(px(8.))
                            .flex()
                            .flex_wrap()
                            .gap(px(12.))
                            .child(link("about-link-author", "flosch.me", AUTHOR))
                            .child(link("about-link-github", "GitHub", AUTHOR_GITHUB))
                            .child(link("about-link-linkedin", "LinkedIn", AUTHOR_LINKEDIN)),
                    )
                    .render(),
            )
            .row(
                row(&theme, "Support Muxus")
                    .description_text(
                        &theme,
                        "Muxus is free and stays free. If it saves you time, a coffee keeps the releases coming.",
                    )
                    .control(
                        Button::new("about-coffee", "Buy me a coffee")
                            .outlined()
                            .start_icon("CoffeeOutlined")
                            .on_click(|_, _, cx| cx.open_url(COFFEE)),
                    )
                    .control(
                        Button::new("about-star", "Star on GitHub")
                            .start_icon("StarBorderOutlined")
                            .on_click(|_, _, cx| cx.open_url(SOURCE)),
                    )
                    .render(),
            )
            .render(&theme);
        let footer = div()
            .flex()
            .justify_center()
            .mt(px(-4.))
            .text_size(px(CAPTION))
            .line_height(px(snap(CAPTION * 1.66)))
            .text_color(p.text.secondary)
            .child("Released under the\u{a0}")
            .child(
                div()
                    .id("about-license")
                    .cursor_pointer()
                    .underline()
                    .on_click(|_, _, cx| cx.open_url(LICENSE))
                    .child("MIT license"),
            )
            .child(".");
        div()
            .flex()
            .flex_col()
            .gap(px(24.))
            .child(hero)
            .child(group("Updates").rows(updates).render(&theme))
            .child(installation)
            .child(made_by)
            .child(footer)
            .into_any_element()
    }

    fn about_hero(
        &mut self,
        theme: &Theme,
        version: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let wash = theme.components.primary_wash;
        let outlined =
            |id: &'static str, text: &'static str, icon_name: &'static str, url: String| {
                Button::new(id, text)
                    .outlined()
                    .start_icon(icon_name)
                    .on_click(move |_, _, cx| cx.open_url(&url))
            };
        let release_notes = texts::release_notes_url(version);
        let mut title_row = div().flex().flex_wrap().items_end().gap(px(10.)).child(
            label("Muxus")
                .size(26.)
                .weight(700.)
                .line_height(snap(26. * 1.2))
                .tracking(-0.26)
                .color(p.text.primary)
                .no_truncate(),
        );
        if let Some(version) = version {
            title_row = title_row.child(
                div().pb(px(3.)).child(
                    label(format!("Version {version}"))
                        .size(BODY2)
                        .line_height(snap(BODY2 * 1.43))
                        .tabular_nums()
                        .color(p.text.secondary),
                ),
            );
        }
        div()
            .flex()
            .items_center()
            .gap(px(20.))
            .p(px(20.))
            .border_1()
            .border_color(p.divider)
            .rounded(px(16.))
            .bg(linear_gradient(
                135.,
                linear_color_stop(wash, 0.),
                linear_color_stop(p.background_paper, 0.7),
            ))
            .child(muxus_ui::icon::logo(68., window, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(title_row)
                    .child(
                        div().mt(px(4.)).max_w(px(520.)).child(
                            paragraph(
                                "A free, open-source SSH, Telnet and serial client with RDP and VNC remote desktops. Split panes, saved workspaces, SFTP, a remote editor, saved tunnels and images in the terminal.",
                            )
                            .size(BODY2)
                            .line_height(snap(BODY2 * 1.43))
                            .color(p.text.secondary),
                        ),
                    )
                    .child(
                        div()
                            .mt(px(14.))
                            .flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(outlined(
                                "about-docs",
                                "Documentation",
                                "MenuBookOutlined",
                                DOCS_URL.to_string(),
                            ))
                            .child(outlined(
                                "about-release-notes",
                                "Release notes",
                                "NewReleasesOutlined",
                                release_notes,
                            ))
                            .child(outlined("about-github", "GitHub", "GitHub", SOURCE.to_string()))
                            .child(outlined(
                                "about-issue",
                                "Report an issue",
                                "BugReportOutlined",
                                ISSUES.to_string(),
                            )),
                    ),
            )
            .into_any_element()
    }

    fn update_controls(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let prefs = self.prefs(cx).clone();
        let desktop = settings_hooks(cx).desktop_update;
        let desktop_state = desktop
            .as_ref()
            .and_then(|hooks| (hooks.state)(cx))
            .filter(|state| state.status != DesktopUpdateStatus::Disabled);
        let notify = self.switch_row(
            theme,
            "settings-notify-new-version",
            "Notify me when a new version is available",
            Some(if desktop_state.is_some() {
                "Checks at startup and every four hours. Off: no background checks or notifications; checking here still works."
            } else {
                "Checks once at startup. Off: no notification, and checking here still works."
            }),
            prefs.notify_on_new_version,
            |p, value| p.notify_on_new_version = value,
        );
        let check = match (desktop, desktop_state) {
            (Some(hooks), Some(state)) => self.desktop_update_row(theme, &hooks, &state),
            _ => self.plain_update_row(theme, cx),
        };
        vec![notify, check]
    }

    fn plain_update_row(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let (tone, text) = update_status(&self.state.update);
        let description = match tone {
            Some(tone) => status_text(theme, tone, text),
            None => paragraph(text)
                .size(CAPTION)
                .line_height(snap(CAPTION * 1.5))
                .color(theme.palette.text.secondary)
                .into_any_element(),
        };
        let available = match &self.state.update.result {
            Some(UpdateCheckResult::Available { release_url, .. }) => Some(release_url.clone()),
            _ => None,
        };
        let mut setting = row(theme, "Update check").description(description);
        if let Some(url) = available.clone() {
            setting = setting.control(
                Button::new("about-download", "Download")
                    .contained()
                    .start_icon("DownloadOutlined")
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            );
        }
        let checking = self.state.update.checking;
        let mut check = Button::new("about-check-updates", "Check for updates")
            .start_icon("CachedOutlined")
            .disabled(checking)
            .on_click(cx.listener(|this, _, _, cx| this.check_for_updates(cx)));
        check = if available.is_some() {
            check.outlined()
        } else {
            check.contained()
        };
        setting.control(check).render()
    }

    fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        self.state.update.checking = true;
        self.state.update.result = None;
        let backend = self.backend.clone();
        let current = self
            .data
            .app_info
            .as_ref()
            .map(|info| info.version.clone())
            .unwrap_or_default();
        self.fetch(
            cx,
            async move { backend.update_check(true).await },
            move |this, result, _| {
                this.state.update.checking = false;
                this.state.update.result =
                    Some(result.unwrap_or(UpdateCheckResult::NotAvailable {
                        current_version: current.clone(),
                        latest_version: None,
                        reason: Some("network".into()),
                    }));
            },
        );
    }

    /// `DesktopUpdateRow`: the update row for installations that update themselves.
    fn desktop_update_row(
        &mut self,
        theme: &Theme,
        hooks: &DesktopUpdateHooks,
        state: &DesktopUpdateState,
    ) -> AnyElement {
        use DesktopUpdateStatus::*;
        let store = state.source == Some(DesktopUpdateSource::Store);
        let busy = matches!(state.status, Checking | Downloading | Installing);
        let can_download = !store
            && (state.status == Available || (state.status == Error && state.version.is_some()));
        let show_progress = state.status == Downloading || (store && state.status == Installing);
        let (tone, text) = desktop_update_status(state);
        let mut description = div().flex().flex_col().child(match tone {
            Some(tone) => status_text(theme, tone, text),
            None => paragraph(text)
                .size(CAPTION)
                .line_height(snap(CAPTION * 1.5))
                .color(theme.palette.text.secondary)
                .into_any_element(),
        });
        if show_progress {
            description = description.child(div().mt(px(8.)).child(
                muxus_ui::widgets::display::linear_progress(
                    theme,
                    (state.percent.unwrap_or(0.) / 100.) as f32,
                ),
            ));
        }
        let mut setting = row(theme, "Update check").description(description);
        let hook = |hook: &super::super::hooks::Hook| {
            let hook = hook.clone();
            move |_: &muxus_ui::gpui_kit::ClickEvent,
                  window: &mut Window,
                  cx: &mut muxus_ui::gpui_kit::App| { hook(window, cx) }
        };
        if can_download {
            setting = setting.control(
                Button::new("update-download", "Download update")
                    .contained()
                    .start_icon("DownloadOutlined")
                    .on_click(hook(&hooks.download)),
            );
        }
        if !store && state.status == Ready {
            setting = setting.control(
                Button::new("update-restart", "Restart to update")
                    .contained()
                    .start_icon("RestartAltOutlined")
                    .on_click(hook(&hooks.install)),
            );
        }
        if store && state.status == Available {
            setting = setting.control(
                Button::new("update-store-now", "Update now")
                    .contained()
                    .start_icon("DownloadOutlined")
                    .on_click(hook(&hooks.install)),
            );
        }
        if store {
            setting = setting.control(
                Button::new("update-open-store", "Open Microsoft Store")
                    .outlined()
                    .start_icon("StorefrontOutlined")
                    .disabled(busy)
                    .on_click(hook(&hooks.open_store)),
            );
        }
        let primary_done =
            can_download || state.status == Ready || (store && state.status == Available);
        let mut check = Button::new("update-check", "Check for updates")
            .start_icon("CachedOutlined")
            .disabled(busy || state.status == Ready || state.status == Updated)
            .on_click(hook(&hooks.check));
        check = if primary_done {
            check.outlined()
        } else {
            check.contained()
        };
        setting.control(check).render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_update_texts() {
        let mut state = UpdateState::default();
        assert_eq!(
            update_status(&state).1,
            "Looks for a newer release on GitHub when you ask."
        );
        state.checking = true;
        assert!(update_status(&state).1.starts_with("Checking GitHub"));
        state.checking = false;
        state.result = Some(UpdateCheckResult::NotAvailable {
            current_version: "0.11.1".into(),
            latest_version: Some("0.11.1".into()),
            reason: None,
        });
        assert_eq!(
            update_status(&state),
            (
                Some(Status::Success),
                "Muxus is up to date. Latest release: 0.11.1.".into()
            )
        );
        state.result = Some(UpdateCheckResult::NotAvailable {
            current_version: "0.11.1".into(),
            latest_version: None,
            reason: Some("store".into()),
        });
        assert_eq!(update_status(&state).0, Some(Status::Info));
    }

    #[test]
    fn desktop_update_texts() {
        let state = DesktopUpdateState {
            status: DesktopUpdateStatus::Downloading,
            current_version: "0.11.0".into(),
            source: None,
            version: Some("0.11.1".into()),
            percent: Some(42.),
            reason: None,
            error: None,
        };
        assert_eq!(
            desktop_update_status(&state).1,
            "Downloading Muxus 0.11.1\u{2026} 42%"
        );
        let store = DesktopUpdateState {
            status: DesktopUpdateStatus::Installing,
            source: Some(DesktopUpdateSource::Store),
            percent: None,
            ..state
        };
        assert_eq!(
            desktop_update_status(&store).1,
            "Waiting for Microsoft Store\u{2026}"
        );
    }
}
