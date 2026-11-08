//! The "SSH client" block of the SSH host editor's Advanced section (`AdvancedSection.tsx`):
//! connect with the computer's `ssh` program instead of the built-in client.

use muxus_ui::gpui_kit::{AnyElement, Context, IntoElement, ParentElement};
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel};
use muxus_ui::widgets::typography::{body1, body2, para};

use super::HostEditor;
use super::ssh::stack;
use crate::features::host_editor::widgets::section::heading;

/// What the switch promises (the bulk editor says the same).
pub const SYSTEM_OPENSSH_HELP: &str = "Connects with this computer's ssh program, for authentication such as Kerberos (GSSAPI). Windows gets the terminal only; elsewhere the file browser, statistics and container listing work too. Tunnels and other features of the built-in client are not available.";

impl HostEditor {
    pub(super) fn ssh_client_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let entity = cx.entity().downgrade();
        stack(0.5)
            .child(heading(&theme, "SSH client", 600.))
            .child(
                FormControlLabel::new(
                    "system-openssh",
                    FormControl::Switch(self.ssh.system_open_ssh),
                    "",
                )
                .label_element(para("Use system OpenSSH", body1(), p.text.primary))
                .flush()
                .on_click(move |_, _, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.ssh.system_open_ssh = !this.ssh.system_open_ssh;
                        this.changed(cx);
                    });
                }),
            )
            .child(para(SYSTEM_OPENSSH_HELP, body2(), p.text.secondary))
            .into_any_element()
    }

    /// Windows OpenSSH cannot share a login, so nothing here runs beside its terminal.
    pub(super) fn system_open_ssh_on_windows(&self) -> bool {
        self.ssh.system_open_ssh
            && muxus_core::platform::Platform::current() == muxus_core::platform::Platform::Windows
    }
}
