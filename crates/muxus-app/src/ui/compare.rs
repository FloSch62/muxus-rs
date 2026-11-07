//! The compare dialog over a main window (`App.tsx` mounts `CompareDialog`
//! while `compareSource` is set): each new first side starts the dialog over,
//! and the dialog reads the window's terminals and connections for its second
//! side.

use std::rc::Rc;

use muxus_app::features::compare;
use muxus_app::features::compare::sources::CompareText;
use muxus_app::features::compare::view::{
    CompareEnv, CompareEvent, CompareRemote, CompareTerminal, open_compare_dialog,
};
use muxus_session::Backend;
use muxus_ui::gpui_kit::{App, Context, WeakEntity, Window};

use crate::ui::features::with_active_shell;
use crate::ui::shell::AppShell;

/// Install the compare feature: "Compare with…" anywhere opens the dialog
/// over the main window the user works in.
pub fn install(backend: &Backend, cx: &mut App) {
    compare::install(
        backend.clone(),
        |side, cx| {
            with_active_shell(cx, move |shell, window, cx| {
                shell.open_compare_dialog(side, window, cx)
            })
        },
        cx,
    );
}

/// The window's terminals and connections, as the dialog's menu lists them.
struct ShellCompareEnv(WeakEntity<AppShell>);

impl CompareEnv for ShellCompareEnv {
    fn terminals(&self, cx: &App) -> Vec<CompareTerminal> {
        let Some(shell) = self.0.upgrade() else {
            return Vec::new();
        };
        let shell = shell.read(cx);
        shell
            .tabs
            .tabs
            .iter()
            .filter_map(|tab| {
                let view = shell.terminal_of(&tab.id)?;
                Some(CompareTerminal {
                    id: tab.id.clone(),
                    title: tab.title.clone(),
                    has_selection: view.read(cx).has_selection(),
                })
            })
            .collect()
    }

    fn terminal_text(&self, id: &str, selection: bool, cx: &App) -> Option<String> {
        let shell = self.0.upgrade()?;
        let view = shell.read(cx).terminal_of(id)?;
        let view = view.read(cx);
        Some(if selection {
            view.selection_text()
        } else {
            view.emulator().buffer_text()
        })
    }

    fn remote_hosts(&self, cx: &App) -> Vec<CompareRemote> {
        let Some(shell) = self.0.upgrade() else {
            return Vec::new();
        };
        let mut remotes: Vec<CompareRemote> = Vec::new();
        for tab in &shell.read(cx).tabs.tabs {
            let Some(conn_id) = tab.conn_id.clone() else {
                continue;
            };
            if tab.sftp_available == Some(false) {
                continue;
            }
            match remotes.iter_mut().find(|remote| remote.conn_id == conn_id) {
                Some(known) => known.host = tab.title.clone(),
                None => remotes.push(CompareRemote {
                    conn_id,
                    host: tab.title.clone(),
                }),
            }
        }
        remotes
    }
}

impl AppShell {
    /// `setCompareSource`: a fresh dialog with `side` on the left.
    pub fn open_compare_dialog(
        &mut self,
        side: CompareText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let env = Rc::new(ShellCompareEnv(cx.entity().downgrade()));
        let dialog = open_compare_dialog(backend, env, side, window, cx);
        self.mount_overlay(
            "compare",
            dialog,
            window,
            cx,
            |this, _, event: &CompareEvent, window, cx| match event {
                CompareEvent::Close => this.close_overlay("compare", window, cx),
            },
        );
    }

    /// "Compare with…" in a terminal's context menu: its selection, or the
    /// whole buffer when nothing is selected.
    pub fn compare_terminal(&mut self, tab_id: &str, selection: &str, cx: &mut Context<Self>) {
        let Some(view) = self.terminal_of(tab_id) else {
            return;
        };
        let title = self
            .tabs
            .tab(tab_id)
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        let (text, selected) = if selection.is_empty() {
            (view.read(cx).emulator().buffer_text(), false)
        } else {
            (selection.to_string(), true)
        };
        compare::open_compare_text(
            compare::sources::terminal_compare_text(&title, text, selected),
            cx,
        );
    }
}
