//! `BehaviorSection` of `SettingsDialog.tsx`: tabs, how sidebar entries
//! open, where new SSH hosts are saved, the connect summary, keepalives and
//! how sessions come back after a restart or a dropped connection.

use muxus_store::prefs::state::{NewSshHostStorage, SidebarOpenGesture};
use muxus_ui::gpui_kit::{AnyElement, Context, IntoElement, Window};
use muxus_ui::theme::ActiveTheme;

use super::super::js::number_to_string;
use super::super::texts;
use super::kit::{group, page, secondary_text};
use super::window::SettingsWindow;

impl SettingsWindow {
    pub fn behavior(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let tabs = group("Tabs")
            .row(self.switch_row(
                &theme,
                "settings-confirm-close",
                "Confirm before closing a live session",
                Some("Closing a connected tab ends its shell."),
                prefs.confirm_close_connected,
                |p, value| p.confirm_close_connected = value,
            ))
            .render(&theme);
        let gesture = self.select_row(
            &theme,
            "Open hosts with",
            Some("With double-click, a single click selects a host, as in a file manager. Local terminals open the same way, and Enter opens either way."),
            "sidebar-open-gesture",
            220.,
            vec![
                ("click".into(), "Single click".into()),
                ("double-click".into(), "Double-click".into()),
            ],
            match prefs.sidebar_open_gesture {
                SidebarOpenGesture::Click => "click",
                SidebarOpenGesture::DoubleClick => "double-click",
            },
            cx,
            |this, value, _, cx| {
                let gesture = if value == "double-click" {
                    SidebarOpenGesture::DoubleClick
                } else {
                    SidebarOpenGesture::Click
                };
                this.set_prefs(cx, |p| p.sidebar_open_gesture = gesture);
            },
        );
        let storage = self.select_row(
            &theme,
            "Save new SSH hosts in",
            Some("The host editor starts with this choice, and each new host can still pick the other. OpenSSH config also works with ssh in any terminal."),
            "new-ssh-host-storage",
            220.,
            vec![
                ("muxus".into(), "Muxus app data only".into()),
                ("openssh".into(), "OpenSSH config".into()),
            ],
            match prefs.new_ssh_host_storage {
                NewSshHostStorage::Muxus => "muxus",
                NewSshHostStorage::Openssh => "openssh",
            },
            cx,
            |this, value, _, cx| {
                let storage = if value == "muxus" {
                    NewSshHostStorage::Muxus
                } else {
                    NewSshHostStorage::Openssh
                };
                this.set_prefs(cx, |p| p.new_ssh_host_storage = storage);
            },
        );
        let keepalive = self.select_row(
            &theme,
            "SSH keepalive interval",
            Some("Keeps idle SSH connections alive through firewalls and NAT. A host's ServerAliveInterval takes precedence; changes apply on reconnect."),
            "ssh-keepalive",
            250.,
            texts::keepalive_options(prefs.ssh_keepalive_interval_seconds)
                .into_iter()
                .map(|(value, text)| (number_to_string(value), text))
                .collect(),
            &number_to_string(prefs.ssh_keepalive_interval_seconds),
            cx,
            |this, value, _, cx| {
                if let Ok(seconds) = value.parse::<f64>() {
                    this.set_prefs(cx, |p| p.ssh_keepalive_interval_seconds = seconds);
                }
            },
        );
        page(
            &theme,
            "Behavior",
            Some(secondary_text(
                &theme,
                "How tabs close, how sidebar entries open, where new SSH hosts are saved, what a new SSH session reports, and how sessions come back after a restart or a dropped connection.",
            )),
            vec![
                tabs,
                group("Hosts sidebar").row(gesture).render(&theme),
                group("New hosts").row(storage).render(&theme),
                group("Connecting")
                    .row(self.switch_row(
                        &theme,
                        "settings-ssh-session-summary",
                        "Show a summary when an SSH session connects",
                        Some("Prints the route, server, login method and encryption above the remote shell, and whether compression, the SFTP browser, X11 and agent forwarding are active."),
                        prefs.ssh_session_summary,
                        |p, value| p.ssh_session_summary = value,
                    ))
                    .render(&theme),
                group("Restore & reconnect")
                    .row(keepalive)
                    .row(self.switch_row(
                        &theme,
                        "settings-auto-reconnect",
                        "Automatically reconnect remote sessions",
                        Some("Restoring a workspace dials its SSH, Telnet, serial and remote desktop tabs, and a dropped connection redials a few times before waiting for a key press. Off: remote tabs wait until asked."),
                        prefs.auto_reconnect_remote,
                        |p, value| p.auto_reconnect_remote = value,
                    ))
                    .row(self.switch_row(
                        &theme,
                        "settings-restore-scrollback",
                        "Restore terminal history",
                        Some("Recent output is saved locally every few seconds and shown again above the new session after a restore or reconnect."),
                        prefs.restore_scrollback,
                        |p, value| p.restore_scrollback = value,
                    ))
                    .render(&theme),
                // `LinkHandlerSettings`: the desktop shell's link registration.
                crate::features::desktop::links_view::links_group(&self.backend, window, cx),
            ],
        )
        .into_any_element()
    }
}
