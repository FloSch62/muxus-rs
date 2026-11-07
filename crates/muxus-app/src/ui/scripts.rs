//! Where Scripts meets the window (`setScriptsDialog`, `openScriptsFor` and
//! `WindowDialPrompts`): the window keeps one script job, so closing the dialog does not stop
//! a run, and the sign-in questions of its hosts queue with every other question of the
//! window, shown one at a time.

use muxus_app::features::forwarding::start::DialPrompt;
use muxus_app::features::scripts::{
    OpenScripts, ScriptJobs, ScriptJobsEvent, ScriptsDialogEvent, open_scripts_dialog,
    open_scripts_for,
};
use muxus_app::hosts::ManagedHost;
use muxus_ui::gpui_kit::{AppContext, Context, Entity, Window};
use muxus_ui::theme::Status;

use crate::ui::prompts::{PromptReply, SessionPrompt};
use crate::ui::shell::AppShell;

impl AppShell {
    /// The window's script job, made on first use.
    fn script_jobs(&mut self, cx: &mut Context<Self>) -> Option<Entity<ScriptJobs>> {
        if let Some((jobs, _)) = &self.features.scripts {
            return Some(jobs.clone());
        }
        let backend = self.data.backend()?;
        let jobs = cx.new(|_| ScriptJobs::new(backend));
        let subscription = cx.subscribe(&jobs, |this, _, event: &ScriptJobsEvent, cx| {
            match event {
                ScriptJobsEvent::Prompt { key, prompt, reply } => {
                    let prompt = match prompt.clone() {
                        DialPrompt::Auth(info) => SessionPrompt::Auth(info),
                        DialPrompt::HostKey(challenge) => SessionPrompt::HostKey(challenge),
                    };
                    this.queue_prompt(
                        key.clone(),
                        prompt,
                        Some(PromptReply::Tunnel(reply.clone())),
                        cx,
                    );
                }
                // The dial gave up (or the job was cancelled): its question goes unasked.
                ScriptJobsEvent::Withdraw { key } => this.close_prompts_of(key, cx),
            }
        });
        self.features.scripts = Some((jobs.clone(), subscription));
        Some(jobs)
    }

    /// Open Scripts, with these hosts picked (`setScriptsDialog({ hostKeys })`).
    pub fn open_scripts(
        &mut self,
        host_keys: Option<Vec<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(backend), Some(jobs)) = (self.data.backend(), self.script_jobs(cx)) else {
            return;
        };
        let hosts: Vec<ManagedHost> = self
            .catalog
            .ssh
            .hosts
            .iter()
            .cloned()
            .map(ManagedHost::ssh)
            .chain(
                self.catalog
                    .profiles
                    .iter()
                    .cloned()
                    .map(ManagedHost::profile),
            )
            .collect();
        let keepalive = Some(self.prefs.ssh_keepalive_interval_seconds);
        let dialog = open_scripts_dialog(backend, jobs, &hosts, host_keys, keepalive, window, cx);
        self.mount_overlay(
            "scripts",
            dialog,
            window,
            cx,
            |this, _, event: &ScriptsDialogEvent, window, cx| match event {
                ScriptsDialogEvent::Close => this.close_overlay("scripts", window, cx),
            },
        );
    }

    /// `openScriptsFor`: Scripts for a host, a selection or a folder; hosts that cannot run
    /// scripts are left out, with a notice.
    pub fn open_scripts_for(
        &mut self,
        hosts: &[ManagedHost],
        what: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match open_scripts_for(hosts, what) {
            OpenScripts::Nothing(message) => self.show_toast(Status::Info, message, cx),
            OpenScripts::Open { keys, notice } => {
                if let Some(notice) = notice {
                    self.show_toast(Status::Info, notice, cx);
                }
                self.open_scripts(Some(keys), window, cx);
            }
        }
    }
}
