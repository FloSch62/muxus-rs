//! The local shells listed under "Local terminal" (`SessionSidebar.tsx`): the
//! saved local shell profiles and, on Windows, the installed WSL distributions
//! no profile covers yet (`useWslShellProfiles`), each drawn with its
//! distribution's own icon when it ships one (`LocalShellIcon`).

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use muxus_core::api::WslDistribution;
use muxus_local::profile::{opens_wsl_distribution, saved_wsl_shell_profile, wsl_shell_profile};
use muxus_store::prefs::state::LocalShellProfileConfig;
use muxus_ui::gpui_kit::{
    AnyElement, Context, ElementId, Image, ImageFormat, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled, div, img, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::Theme;
use muxus_ui::widgets::{AnchorBounds, Placement, Tooltip, bounds_probe};

use muxus_app::sidebar::row_clicks::click_opens_row;

use crate::ui::shell::AppShell;
use crate::ui::sidebar::{SessionSidebar, SidebarEvent, label_text, row_shell};
use crate::ui::sidebar_menus::SidebarMenu;

/// A row under "Local terminal": a saved profile, or a distribution listed automatically.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalShellEntry {
    pub profile: LocalShellProfileConfig,
    /// An installed WSL distribution listed automatically, not a saved profile.
    pub wsl: bool,
}

/// What the sidebar knows of the local shells.
#[derive(Clone, Default)]
pub struct LocalShells {
    saved: Vec<LocalShellProfileConfig>,
    show_wsl: bool,
    distributions: Vec<WslDistribution>,
    /// Each distribution's icon, decoded once.
    icons: HashMap<String, Arc<Image>>,
}

impl LocalShells {
    /// Installed distributions no saved profile covers yet, when they are listed at all.
    pub fn wsl_profiles(&self) -> Vec<LocalShellProfileConfig> {
        if !self.show_wsl {
            return Vec::new();
        }
        self.distributions
            .iter()
            .filter(|distribution| {
                !self.saved.iter().any(|profile| {
                    opens_wsl_distribution(
                        Some(&profile.shell),
                        Some(&profile.args),
                        &distribution.name,
                    )
                })
            })
            .map(|distribution| to_store(wsl_shell_profile(distribution)))
            .collect()
    }

    /// The rows a search shows: saved profiles first, then the distributions, matched
    /// on name, shell, arguments and folder.
    pub fn visible(&self, needle: &str) -> Vec<LocalShellEntry> {
        let saved = self.saved.iter().cloned().map(|profile| LocalShellEntry {
            profile,
            wsl: false,
        });
        let wsl = self
            .wsl_profiles()
            .into_iter()
            .map(|profile| LocalShellEntry { profile, wsl: true });
        saved
            .chain(wsl)
            .filter(|entry| {
                if needle.is_empty() {
                    return true;
                }
                let profile = &entry.profile;
                std::iter::once(profile.name.as_str())
                    .chain(std::iter::once(profile.shell.as_str()))
                    .chain(profile.args.iter().map(String::as_str))
                    .chain(std::iter::once(profile.cwd.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains(needle)
            })
            .collect()
    }

    /// The icon of the distribution a launch starts, when it ships one; saved
    /// profiles match by name (`useWslDistributionIcon`).
    fn icon_for(&self, profile: &LocalShellProfileConfig) -> Option<Arc<Image>> {
        self.distributions
            .iter()
            .filter(|distribution| self.icons.contains_key(&distribution.name))
            .find(|distribution| {
                opens_wsl_distribution(
                    Some(&profile.shell),
                    Some(&profile.args),
                    &distribution.name,
                )
            })
            .and_then(|distribution| self.icons.get(&distribution.name).cloned())
    }
}

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

fn to_local(profile: &LocalShellProfileConfig) -> muxus_local::profile::LocalShellProfileConfig {
    muxus_local::profile::LocalShellProfileConfig {
        id: profile.id.clone(),
        name: profile.name.clone(),
        shell: profile.shell.clone(),
        args: profile.args.clone(),
        cwd: profile.cwd.clone(),
        startup_command: profile.startup_command.clone(),
    }
}

/// A `data:` URL image (the distributions' icons are `.ico` files).
fn data_url_image(url: &str) -> Option<Arc<Image>> {
    let (meta, data) = url.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let format = match mime {
        "image/x-icon" | "image/vnd.microsoft.icon" => ImageFormat::Ico,
        "image/png" => ImageFormat::Png,
        "image/bmp" => ImageFormat::Bmp,
        "image/svg+xml" => ImageFormat::Svg,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// `[shell || 'Automatic shell', ...args].join(' ')`.
fn command_line(profile: &LocalShellProfileConfig) -> String {
    let shell = if profile.shell.is_empty() {
        "Automatic shell"
    } else {
        profile.shell.as_str()
    };
    std::iter::once(shell)
        .chain(profile.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

impl SessionSidebar {
    /// The saved profiles and whether installed distributions are listed (the preferences).
    pub fn set_local_shell_prefs(
        &mut self,
        saved: Vec<LocalShellProfileConfig>,
        show_wsl: bool,
        cx: &mut Context<Self>,
    ) {
        if self.local_shells.saved != saved || self.local_shells.show_wsl != show_wsl {
            self.local_shells.saved = saved;
            self.local_shells.show_wsl = show_wsl;
            cx.notify();
        }
    }

    /// The installed WSL distributions (Windows only).
    pub fn set_wsl_distributions(
        &mut self,
        distributions: Vec<WslDistribution>,
        cx: &mut Context<Self>,
    ) {
        self.local_shells.icons = distributions
            .iter()
            .filter_map(|distribution| {
                let image = data_url_image(distribution.icon.as_deref()?)?;
                Some((distribution.name.clone(), image))
            })
            .collect();
        self.local_shells.distributions = distributions;
        cx.notify();
    }

    /// The distributions the quick launcher offers too.
    pub fn wsl_shell_profiles(&self) -> Vec<LocalShellProfileConfig> {
        self.local_shells.wsl_profiles()
    }

    /// The local shell rows under "Local terminal" that the search leaves.
    pub(crate) fn visible_local_shells(&self) -> Vec<LocalShellEntry> {
        self.local_shells.visible(&self.normalized_filter())
    }

    /// One local shell row: indented under "Local terminal", its command line on hover.
    pub(crate) fn local_shell_row(
        &self,
        entry: LocalShellEntry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let profile = entry.profile.clone();
        let name = muxus_core::js::js_trim(&profile.name);
        let title = if name.is_empty() {
            "Unnamed shell".to_string()
        } else {
            name.to_string()
        };
        let leading = match self.local_shells.icon_for(&profile) {
            Some(image) => img(image).size(px(15.)).flex_none().into_any_element(),
            None => icon("Terminal", 15., p.text.secondary).into_any_element(),
        };
        let anchor = AnchorBounds::default();
        // Tooltips open toward the panes, wherever the sidebar docks.
        let placement = if self.on_right {
            Placement::Left
        } else {
            Placement::Right
        };
        let open = profile.clone();
        let menu = entry.clone();
        row_shell(
            ElementId::Name(format!("local-shell-{}", profile.id).into()),
            0,
            None,
            theme,
        )
        .pl(px(24.))
        .gap(px(6.))
        .cursor_pointer()
        .child(leading)
        .child(
            div()
                .min_w_0()
                .child(label_text(title.into(), p.sidebar_ink, 450.)),
        )
        .child(bounds_probe(&anchor))
        .tooltip(Tooltip::anchored(command_line(&profile), anchor, placement))
        .on_click(
            cx.listener(move |this, event: &muxus_ui::gpui_kit::ClickEvent, _, cx| {
                cx.stop_propagation();
                if click_opens_row(event.click_count() as u32, this.open_gesture) {
                    cx.emit(SidebarEvent::OpenLocalShell(open.clone()));
                }
            }),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.menu = Some(SidebarMenu::LocalShell {
                    position: event.position,
                    profile: Some(menu.profile.clone()),
                    wsl: menu.wsl,
                });
                cx.notify();
            }),
        )
        .into_any_element()
    }
}

impl AppShell {
    /// Hand the sidebar the local shell preferences.
    pub fn sync_sidebar_shells(&mut self, cx: &mut Context<Self>) {
        let saved = self.prefs.local_shell_profiles.clone();
        let show_wsl = self.prefs.show_wsl_distributions;
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_local_shell_prefs(saved, show_wsl, cx)
        });
    }

    /// Read the installed WSL distributions once (only Windows has any).
    pub fn load_wsl_distributions(&mut self, cx: &mut Context<Self>) {
        if muxus_core::platform::Platform::current() != muxus_core::platform::Platform::Windows {
            return;
        }
        let Some(backend) = self.data.backend() else {
            return;
        };
        let sidebar = self.sidebar.downgrade();
        cx.spawn(async move |_, cx| {
            if let Ok(response) = backend.wsl_distributions().await {
                let _ = sidebar.update(cx, |sidebar, cx| {
                    sidebar.set_wsl_distributions(response.distributions, cx)
                });
            }
        })
        .detach();
    }

    /// "Save as profile": copy a listed distribution into the saved profiles, where
    /// it can be customized; the automatic entry then stops being listed.
    pub fn save_wsl_shell_profile(
        &mut self,
        profile: &LocalShellProfileConfig,
        cx: &mut Context<Self>,
    ) {
        let saved = to_store(saved_wsl_shell_profile(&to_local(profile)));
        self.prefs.local_shell_profiles.push(saved);
        self.sync_sidebar_shells(cx);
        self.prefs_changed(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, name: &str, shell: &str, args: &[&str]) -> LocalShellProfileConfig {
        LocalShellProfileConfig {
            id: id.into(),
            name: name.into(),
            shell: shell.into(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            cwd: String::new(),
            startup_command: String::new(),
        }
    }

    fn distribution(name: &str) -> WslDistribution {
        WslDistribution {
            name: name.into(),
            is_default: false,
            icon: None,
        }
    }

    #[test]
    fn distributions_follow_the_saved_profiles_unless_one_covers_them() {
        let shells = LocalShells {
            saved: vec![profile(
                "ubuntu",
                "Ubuntu dev",
                "wsl.exe",
                &["-d", "Ubuntu"],
            )],
            show_wsl: true,
            distributions: vec![distribution("Ubuntu"), distribution("Debian")],
            icons: HashMap::new(),
        };
        let rows: Vec<(String, bool)> = shells
            .visible("")
            .into_iter()
            .map(|entry| (entry.profile.name, entry.wsl))
            .collect();
        assert_eq!(
            rows,
            [
                ("Ubuntu dev".to_string(), false),
                ("Debian".to_string(), true)
            ]
        );
        // The search reads the name, the shell, the arguments and the folder.
        assert_eq!(shells.visible("debian").len(), 1);
        assert_eq!(shells.visible("wsl.exe").len(), 2);
        assert_eq!(shells.visible("pwsh").len(), 0);
        // Turned off, only the saved profiles stay.
        let hidden = LocalShells {
            show_wsl: false,
            ..shells
        };
        assert_eq!(hidden.visible("").len(), 1);
    }

    #[test]
    fn reads_icon_data_urls() {
        // A 1x1 PNG.
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let image = data_url_image(url).expect("decodes");
        assert_eq!(image.format, ImageFormat::Png);
        assert!(data_url_image("data:text/plain;base64,aGk=").is_none());
        assert!(data_url_image("https://example.test/icon.ico").is_none());
    }

    #[test]
    fn command_lines_name_the_automatic_shell() {
        assert_eq!(
            command_line(&profile("a", "A", "", &["-l"])),
            "Automatic shell -l"
        );
        assert_eq!(
            command_line(&profile("b", "B", "wsl.exe", &["-d", "Ubuntu"])),
            "wsl.exe -d Ubuntu"
        );
    }
}
