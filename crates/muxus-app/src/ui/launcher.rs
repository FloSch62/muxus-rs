//! The quick launcher (`QuickLauncherDialog.tsx`), the empty pane's host picker
//! (`HostPickerPopover.tsx`) and starting saved tunnels from either.

use std::time::Instant;

use muxus_app::features::forwarding::start::{DialAnswer, DialPrompt, start_tunnel};
use muxus_app::features::launcher::catalog::LauncherAction;
use muxus_app::features::launcher::sessions::{
    open_default_local_terminal, open_local_shell_profile,
};
use muxus_app::features::launcher::{
    HostPickerEvent, LauncherContext, LauncherEvent, open_host_picker, open_quick_launcher,
};
use muxus_app::model::SessionProfile;
use muxus_app::state::session_actions::{connect_managed_host, connect_target};
use muxus_core::api::TunnelRecord;
use muxus_ui::gpui_kit::{Context, Pixels, Point, Window};
use muxus_ui::theme::Status;

use crate::ui::prompts::{PromptReply, SessionPrompt};
use crate::ui::shell::AppShell;

impl AppShell {
    /// What the launcher needs to know about this window.
    fn launcher_context(&self, cx: &Context<Self>) -> LauncherContext {
        LauncherContext {
            tabs: self.tabs.tabs.clone(),
            active_id: self.tabs.active_id.clone(),
            shown_command_group: self.shown_command_group(cx),
            wsl_shell_profiles: Vec::new(),
            workspaces: self.workspaces.clone(),
        }
    }

    /// `setQuickLauncherOpen(true)` (Ctrl+K).
    pub fn open_launcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let context = self.launcher_context(cx);
        let launcher = open_quick_launcher(backend, context, window, cx);
        self.mount_overlay(
            "launcher",
            launcher,
            window,
            cx,
            |this, _, event: &LauncherEvent, window, cx| this.on_launcher_event(event, window, cx),
        );
    }

    fn on_launcher_event(
        &mut self,
        event: &LauncherEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = Instant::now();
        match event.clone() {
            LauncherEvent::Close => {
                self.close_overlay("launcher", window, cx);
                return;
            }
            LauncherEvent::ActivateTab { tab_id, reconnect } => {
                self.tabs.activate(&tab_id);
                if reconnect {
                    self.tabs.reconnect(&[tab_id.as_str()], None);
                }
            }
            LauncherEvent::ActivateEditor { tab_id, path } => {
                self.tabs.activate(&tab_id);
                self.tabs.activate_editor(&tab_id, &path);
            }
            LauncherEvent::ConnectHost(host) => {
                connect_managed_host(&mut self.tabs, &mut self.launch_guard, &host, None, now);
            }
            LauncherEvent::ConnectTarget(target) => {
                connect_target(
                    &mut self.tabs,
                    &mut self.launch_guard,
                    &target,
                    &target,
                    None,
                    now,
                );
            }
            LauncherEvent::OpenLocalShell(profile) => {
                open_local_shell_profile(
                    &mut self.tabs,
                    &mut self.launch_guard,
                    &profile,
                    None,
                    now,
                );
            }
            LauncherEvent::OpenHistory { query, selected_id } => {
                self.open_history(&query, Some(selected_id), window, cx);
            }
            LauncherEvent::Action(action) => self.run_launcher_action(action, window, cx),
            LauncherEvent::RunKeyCommand(id) => {
                self.close_overlay("launcher", window, cx);
                self.run_command(&id, window, cx);
            }
            LauncherEvent::StartTunnel(tunnel) => self.start_tunnel(tunnel, cx),
        }
        self.tabs_changed(cx);
    }

    fn run_launcher_action(
        &mut self,
        action: LauncherAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_overlay("launcher", window, cx);
        match action {
            LauncherAction::NewLocal => self.open_local(None, cx),
            LauncherAction::NewHost => self.add_host(None, window, cx),
            LauncherAction::Settings => self.open_settings(None, cx),
            LauncherAction::Workspaces => self.open_workspaces(window, cx),
            LauncherAction::History => self.open_history("", None, window, cx),
            LauncherAction::Commands => self.open_command_buttons(window, cx),
            LauncherAction::Tunnels => self.set_forwarding_open(true, cx),
            LauncherAction::Sftp => {
                if let Some(tab) = self.tabs.active_tab().cloned()
                    && matches!(tab.profile, Some(SessionProfile::Ssh(_)))
                    && tab.conn_id.is_some()
                {
                    self.toggle_file_browser(&tab.id, cx);
                }
            }
            LauncherAction::FindTerminal => self.tabs.request_search(),
        }
    }

    /// `startTunnel`: its sign-in questions go through this window's prompt dialogs.
    pub fn start_tunnel(&mut self, tunnel: TunnelRecord, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let keepalive = Some(self.prefs.ssh_keepalive_interval_seconds);
        let this = cx.entity().downgrade();
        let async_cx = cx.to_async();
        cx.spawn(async move |weak, cx| {
            let name = tunnel.name.clone().unwrap_or_else(|| tunnel.target.clone());
            let key = format!("tunnel:{}", tunnel.id);
            let result = start_tunnel(
                &backend,
                &tunnel,
                keepalive,
                |_| {},
                |prompt| {
                    let (reply, answer) = async_channel::bounded::<DialAnswer>(1);
                    let key = key.clone();
                    let prompt = match prompt {
                        DialPrompt::Auth(info) => SessionPrompt::Auth(info),
                        DialPrompt::HostKey(challenge) => SessionPrompt::HostKey(challenge),
                    };
                    let mut async_cx = async_cx.clone();
                    let this = this.clone();
                    async move {
                        let _ = this.update(&mut async_cx, |this, cx| {
                            this.queue_prompt(key, prompt, Some(PromptReply::Tunnel(reply)), cx)
                        });
                        answer.recv().await.unwrap_or(DialAnswer::Auth(None))
                    }
                },
            )
            .await;
            let _ = weak.update(cx, |this, cx| {
                this.refresh_forwards(cx);
                match result {
                    Ok(_) => {
                        this.show_toast(Status::Success, format!("Started tunnel “{name}”."), cx)
                    }
                    Err(message) => this.show_toast(Status::Error, message, cx),
                }
            });
        })
        .detach();
    }

    /// The empty pane's "Saved host" card: the host picker below it.
    pub fn open_host_picker(
        &mut self,
        position: Point<Pixels>,
        replace_tab_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let picker = open_host_picker(backend, position, replace_tab_id, window, cx);
        self.mount_layer(
            "host-picker",
            picker,
            true,
            window,
            cx,
            |this, _, event: &HostPickerEvent, window, cx| match event.clone() {
                HostPickerEvent::Close => this.close_overlay("host-picker", window, cx),
                HostPickerEvent::Connect {
                    host,
                    replace_tab_id,
                } => {
                    this.close_overlay("host-picker", window, cx);
                    connect_managed_host(
                        &mut this.tabs,
                        &mut this.launch_guard,
                        &host,
                        replace_tab_id.as_deref(),
                        Instant::now(),
                    );
                    this.tabs_changed(cx);
                }
                HostPickerEvent::AddHost => {
                    this.close_overlay("host-picker", window, cx);
                    this.add_host(None, window, cx);
                }
            },
        );
    }

    /// `openLocalTerminal`: the default local shell (in place of a blank tab).
    pub fn open_local(&mut self, replace: Option<&str>, cx: &mut Context<Self>) {
        open_default_local_terminal(
            &mut self.tabs,
            &mut self.launch_guard,
            &self.prefs,
            replace,
            Instant::now(),
        );
        self.tabs_changed(cx);
    }
}
