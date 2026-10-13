//! `components/LocalShellProfilesSection.tsx`: the default shell, saved
//! launch profiles, and on Windows the installed WSL distributions.

use muxus_core::api::WslDistribution;
use muxus_local::profile::{
    new_local_shell_profile_id, opens_wsl_distribution, parse_local_shell_argument_text,
    saved_wsl_shell_profile, wsl_shell_profile,
};
use muxus_store::prefs::state::LocalShellProfileConfig;
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::{Button, ButtonColor, IconButton};

use super::super::texts;
use super::fields::field;
use super::kit::{BODY2, CAPTION, group, page, row, secondary_text, snap};
use super::window::SettingsWindow;

/// `fontFamily: 'monospace'` as Chrome resolves it on Linux.
const MONOSPACE: &str = "DejaVu Sans Mono";

/// A profile as the prefs store it, from the local crate's shape.
fn to_store(profile: muxus_local::profile::LocalShellProfileConfig) -> LocalShellProfileConfig {
    LocalShellProfileConfig {
        id: profile.id,
        name: profile.name,
        shell: profile.shell,
        args: profile.args,
        cwd: profile.cwd,
        startup_command: profile.startup_command,
    }
}

/// `value.slice(0, max)` in UTF-16 units.
fn clip(value: &str, max: usize) -> String {
    muxus_core::js::utf16_prefix(value, max).to_string()
}

impl SettingsWindow {
    fn update_shell_profile(
        &self,
        id: &str,
        cx: &mut muxus_ui::gpui_kit::App,
        change: impl FnOnce(&mut LocalShellProfileConfig),
    ) {
        let id = id.to_string();
        self.set_prefs(cx, move |prefs| {
            if let Some(profile) = prefs.local_shell_profiles.iter_mut().find(|p| p.id == id) {
                change(profile);
            }
        });
    }

    pub fn local_shells(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let profiles = prefs.local_shell_profiles.clone();
        let info = self.data.app_info.clone();
        let selected_default = if profiles
            .iter()
            .any(|profile| profile.id == prefs.default_local_shell_profile_id)
        {
            prefs.default_local_shell_profile_id.clone()
        } else {
            String::new()
        };
        let mut options = vec![(String::new(), "Automatic shell".to_string())];
        options.extend(profiles.iter().map(|profile| {
            let name = muxus_core::js::js_trim(&profile.name);
            (
                profile.id.clone(),
                if name.is_empty() {
                    "Unnamed shell".to_string()
                } else {
                    name.to_string()
                },
            )
        }));
        let default_select = self.row_select(
            &theme,
            "default-local-shell",
            260.,
            options,
            &selected_default,
            false,
            cx,
            |this, value, _, cx| this.set_prefs(cx, |p| p.default_local_shell_profile_id = value),
        );
        let local_shell = prefs.local_shell.clone();
        let automatic = self.input(
            "automatic-shell",
            || local_shell.clone(),
            Some("auto"),
            false,
            window,
            cx,
            |this, value, _, cx| this.set_prefs(cx, |p| p.local_shell = clip(&value, 4096)),
        );
        self.sync_input("automatic-shell", &prefs.local_shell, window, cx);
        let default_group = group("Default")
            .row(
                row(&theme, "Default profile")
                    .description_text(
                        &theme,
                        "Used by the sidebar, the empty-pane shortcut and the generic \u{201c}New local terminal\u{201d} action.",
                    )
                    .control(default_select)
                    .render(),
            )
            .row(
                row(&theme, "Automatic shell")
                    .description_text(&theme, texts::automatic_shell_description(info.as_ref()))
                    .control(
                        field("settings-automatic-shell", &automatic)
                            .width(260.)
                            .font(MONOSPACE, super::kit::BODY1)
                            .render(&theme, window, cx),
                    )
                    .render(),
            )
            .render(&theme);

        let mut saved = group(texts::counted_title("Saved profiles", profiles.len()))
            .description(secondary_text(
                &theme,
                "Separate profiles for PowerShell, Command Prompt, WSL distributions or project-specific shells.",
            ))
            .action(
                Button::new("local-shell-add", "Add profile")
                    .start_icon("Add")
                    .on_click(cx.listener(|this, _, _, cx| this.add_shell_profile(cx))),
            )
            .flush();
        if profiles.is_empty() {
            saved = saved.row(
                div().px(px(16.)).py(px(20.)).child(
                    paragraph(
                        "No saved profiles yet. The automatic shell keeps working as before.",
                    )
                    .size(BODY2)
                    .line_height(snap(BODY2 * 1.43))
                    .color(theme.palette.text.secondary)
                    .center(),
                ),
            );
        } else {
            for (index, profile) in profiles.iter().enumerate() {
                let entry = self.shell_profile_entry(&theme, profile, info.as_ref(), window, cx);
                let highlighted = self.state.highlighted.as_deref() == Some(profile.id.as_str());
                saved = saved.row(
                    div()
                        .relative()
                        .when(index > 0, |d| {
                            d.border_t_1().border_color(theme.palette.divider)
                        })
                        .child(entry)
                        .when(highlighted, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .border_2()
                                    .border_color(theme.palette.primary.main),
                            )
                        }),
                );
            }
        }
        let mut groups = vec![default_group, saved.render(&theme)];
        if info.as_ref().is_some_and(|info| info.platform == "win32") {
            groups.push(self.wsl_group(&theme, &prefs, cx));
        }
        page(
            &theme,
            "Local shells",
            Some(secondary_text(
                &theme,
                "The shells Muxus starts on this computer. Every saved profile is also available in the quick launcher.",
            )),
            groups,
        )
        .into_any_element()
    }

    fn add_shell_profile(&mut self, cx: &mut Context<Self>) {
        let prefs = self.prefs(cx).clone();
        let count = prefs.local_shell_profiles.len();
        let id = new_local_shell_profile_id();
        let profile = LocalShellProfileConfig {
            id: id.clone(),
            name: format!("Shell {}", count + 1),
            shell: self
                .data
                .app_info
                .as_ref()
                .map(|info| info.default_shell.clone())
                .unwrap_or_default(),
            args: Vec::new(),
            cwd: String::new(),
            startup_command: String::new(),
        };
        self.set_prefs(cx, move |p| {
            p.local_shell_profiles.push(profile);
            if count == 0 {
                p.default_local_shell_profile_id = id;
            }
        });
    }

    fn remove_shell_profile(&mut self, profile: LocalShellProfileConfig, cx: &mut Context<Self>) {
        self.confirm(
            format!("Delete \u{201c}{}\u{201d}?", profile.name),
            "Existing tabs and saved workspaces keep their launch settings, but this profile will disappear from new-terminal choices.",
            "Delete profile",
            true,
            move |this, _, cx| {
                let id = profile.id.clone();
                this.drop_inputs(&format!("shell-{id}-"));
                this.set_prefs(cx, move |p| {
                    p.local_shell_profiles.retain(|candidate| candidate.id != id);
                    if p.default_local_shell_profile_id == id {
                        p.default_local_shell_profile_id.clear();
                    }
                });
            },
            cx,
        );
    }

    fn shell_profile_entry(
        &mut self,
        theme: &Theme,
        profile: &LocalShellProfileConfig,
        info: Option<&muxus_core::api::AppInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let paper = theme.palette.background_paper;
        let windows = info.is_some_and(|info| info.platform == "win32");
        let prefix = format!("shell-{}", profile.id);
        let key = |name: &str| format!("{prefix}-{name}");

        let id = profile.id.clone();
        let name_value = profile.name.clone();
        let name = self.input(
            &key("name"),
            || name_value.clone(),
            None,
            false,
            window,
            cx,
            move |this, value, _, cx| {
                this.update_shell_profile(&id, cx, |p| p.name = clip(&value, 200));
            },
        );
        self.sync_input(&key("name"), &profile.name, window, cx);
        let id = profile.id.clone();
        let shell_value = profile.shell.clone();
        let shell = self.input(
            &key("shell"),
            || shell_value.clone(),
            Some(if windows { "wsl.exe" } else { "/bin/zsh" }),
            false,
            window,
            cx,
            move |this, value, _, cx| {
                this.update_shell_profile(&id, cx, |p| p.shell = clip(&value, 4096))
            },
        );
        self.sync_input(&key("shell"), &profile.shell, window, cx);
        let id = profile.id.clone();
        let args_value = profile.args.join("\n");
        let args = self.textarea(
            &key("args"),
            || args_value.clone(),
            Some(if windows { "-d\nUbuntu" } else { "--login" }),
            (2, 12),
            window,
            cx,
            move |this, value, _, cx| {
                this.update_shell_profile(&id, cx, |p| {
                    p.args = parse_local_shell_argument_text(&value)
                });
            },
        );
        let id = profile.id.clone();
        let cwd_value = profile.cwd.clone();
        let home = info.map(|info| info.home_dir.clone());
        let cwd = self.input(
            &key("cwd"),
            || cwd_value.clone(),
            home.as_deref(),
            false,
            window,
            cx,
            move |this, value, _, cx| {
                this.update_shell_profile(&id, cx, |p| p.cwd = clip(&value, 4096))
            },
        );
        self.sync_input(&key("cwd"), &profile.cwd, window, cx);
        let id = profile.id.clone();
        let startup_value = profile.startup_command.clone();
        let startup = self.textarea(
            &key("startup"),
            || startup_value.clone(),
            Some("cd project"),
            (2, 12),
            window,
            cx,
            move |this, value, _, cx| {
                this.update_shell_profile(&id, cx, |p| p.startup_command = clip(&value, 32_768));
            },
        );
        let name_missing = muxus_core::js::js_trim(&profile.name).is_empty();
        let remove = profile.clone();
        div()
            .p(px(16.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        div().flex_1().min_w_0().child(
                            field(SharedString::from(key("name-field")), &name)
                                .label("Profile name", paper)
                                .error(name_missing)
                                .helper(if name_missing { "Enter a name." } else { "Shown in launch menus" })
                                .render(theme, window, cx),
                        ),
                    )
                    .child(
                        div().mt(px(2.)).child(
                            IconButton::new(SharedString::from(key("delete")), "DeleteOutlined")
                                .medium()
                                .icon_size(SMALL)
                                .color(ButtonColor::Error)
                                .tooltip("Delete profile")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.remove_shell_profile(remove.clone(), cx)
                                })),
                        ),
                    ),
            )
            .child(
                field(SharedString::from(key("shell-field")), &shell)
                    .label("Executable", paper)
                    .helper("Executable name or absolute path; blank uses the system default")
                    .font(MONOSPACE, super::kit::BODY1)
                    .render(theme, window, cx),
            )
            .child(
                div()
                    .flex()
                    .gap(px(16.))
                    .child(
                        div().flex_1().min_w_0().child(
                            field(SharedString::from(key("args-field")), &args)
                                .label("Arguments", paper)
                                .helper("One argument per line; spaces stay inside that argument")
                                .min_rows(2)
                                .render(theme, window, cx),
                        ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            field(SharedString::from(key("cwd-field")), &cwd)
                                .label("Starting directory", paper)
                                .helper("Blank starts in your home directory")
                                .render(theme, window, cx),
                        ),
                    ),
            )
            .child(
                field(SharedString::from(key("startup-field")), &startup)
                    .label("Startup commands", paper)
                    .helper("Entered automatically after the interactive shell starts; one command per line")
                    .min_rows(2)
                    .render(theme, window, cx),
            )
            .into_any_element()
    }

    /// Installed WSL distributions: listed wherever a local terminal can be
    /// launched, and one click away from a saved profile.
    fn wsl_group(
        &mut self,
        theme: &Theme,
        prefs: &muxus_store::prefs::PrefsState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        if self.state.wsl.is_none() {
            self.state.wsl = Some(super::window::Load::Pending);
            let backend = self.backend.clone();
            self.fetch(
                cx,
                async move { backend.wsl_distributions().await },
                |this, result, _| {
                    this.state.wsl = Some(match result {
                        Ok(response) => super::window::Load::Ready(response.distributions),
                        Err(error) => super::window::Load::Failed(error.to_string()),
                    });
                },
            );
        }
        let mut section = group("WSL distributions").row(self.switch_row(
            theme,
            "settings-show-wsl",
            "List installed distributions",
            Some("Each one appears below Local terminal in the sidebar and in the quick launcher, opening in its Linux home directory. Save one as a profile to change how it starts or to make it the default."),
            prefs.show_wsl_distributions,
            |p, value| p.show_wsl_distributions = value,
        ));
        let distributions: Vec<WslDistribution> = match &self.state.wsl {
            Some(super::window::Load::Ready(list)) if !list.is_empty() => list.clone(),
            other => {
                let text = match other {
                    Some(super::window::Load::Failed(_)) => {
                        "The installed distributions could not be read."
                    }
                    Some(super::window::Load::Ready(_)) => "No WSL distributions are installed.",
                    _ => "Looking for WSL distributions\u{2026}",
                };
                return section
                    .row(
                        div().px(px(16.)).py(px(12.)).child(
                            label(text)
                                .size(BODY2)
                                .line_height(snap(BODY2 * 1.43))
                                .color(p.text.secondary),
                        ),
                    )
                    .render(theme);
            }
        };
        for distribution in distributions {
            let saved = prefs.local_shell_profiles.iter().find(|profile| {
                opens_wsl_distribution(
                    Some(&profile.shell),
                    Some(&profile.args),
                    &distribution.name,
                )
            });
            let launch = wsl_shell_profile(&distribution);
            let command = std::iter::once(launch.shell.clone())
                .chain(launch.args.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ");
            let detail = format!(
                "{}{command}",
                if distribution.is_default {
                    "Default distribution \u{b7} "
                } else {
                    ""
                }
            );
            let trailing: AnyElement = match saved {
                Some(profile) => {
                    let name = muxus_core::js::js_trim(&profile.name);
                    label(format!(
                        "Saved as \u{201c}{}\u{201d}",
                        if name.is_empty() {
                            "Unnamed shell"
                        } else {
                            name
                        }
                    ))
                    .size(CAPTION)
                    .line_height(snap(CAPTION * 1.66))
                    .color(p.text.secondary)
                    .into_any_element()
                }
                None => {
                    let launch = launch.clone();
                    Button::new(
                        SharedString::from(format!("wsl-save-{}", distribution.name)),
                        "Save as profile",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let profile = to_store(saved_wsl_shell_profile(&launch));
                        this.set_prefs(cx, move |p| p.local_shell_profiles.push(profile));
                    }))
                    .into_any_element()
                }
            };
            let highlighted = self.state.highlighted.as_deref() == Some(launch.id.as_str());
            section = section.row(
                div()
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(10.))
                    .child(icon("Terminal", 20., p.text.secondary))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                label(distribution.name.clone())
                                    .size(BODY2)
                                    .weight(550.)
                                    .line_height(snap(BODY2 * 1.43))
                                    .color(p.text.primary),
                            )
                            .child(
                                label(detail)
                                    .size(CAPTION)
                                    .line_height(snap(CAPTION * 1.66))
                                    .color(p.text.secondary),
                            ),
                    )
                    .child(trailing)
                    .when(highlighted, |d| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .border_2()
                                .border_color(p.primary.main),
                        )
                    }),
            );
        }
        section.render(theme)
    }
}
