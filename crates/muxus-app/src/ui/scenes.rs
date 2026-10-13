//! `MUXUS_SCENE`: the windows the documentation screenshots show
//! (`hack/capture.mjs`), rebuilt on the demo catalog for side-by-side
//! comparison, plus a few scenes for performance checks.
//!
//! - `empty`: the demo sidebar, no tabs.
//! - `emptytab`: web-01 connected and a blank tab beside it.
//! - `overview`: web-01, after `status` and `ll conf/`.
//! - `panes`: web-01 split right (`tailaccess`) and down (`ll`).
//! - `local`: one local shell.
//! - `localsplit`: two local shells side by side.
//! - `many`: the demo plus 800 generated hosts.
//! - `tabs:N`: N local shells in one pane.
//!
//! Commands are typed once the terminal shows a prompt, the way the capture
//! script waits for `$ ` before typing.

use std::time::Duration;

use muxus_app::hosts::ManagedHost;
use muxus_app::model::Catalog;
use muxus_app::state::workspace_layout::PaneDirection;
use muxus_ui::gpui_kit::{App, AsyncApp, Context, WeakEntity};

use crate::ui::shell::AppShell;

/// One step of a scene, run after the previous one.
#[derive(Clone, Debug)]
enum Step {
    /// Wait for the active terminal's prompt, type the command and Enter.
    Run(&'static str, u64),
    /// Type the command once the prompt shows, then wait for the next prompt.
    RunToPrompt(&'static str),
    Split(PaneDirection),
    Wait(u64),
    /// A line in the performance log.
    Mark(&'static str),
    /// The active terminal's last lines into the performance log.
    Dump(usize),
    /// Open this many more local shells.
    OpenLocal(usize),
    Quit,
}

/// Set a scene up and start its steps.
pub fn open_scene(shell: &mut AppShell, scene: &str, cx: &mut Context<AppShell>) {
    let steps: Vec<Step> = match scene {
        "overview" => {
            open_demo_host(shell, "web-01", cx);
            shell.workspace_name = "Workspace 1".into();
            shell.forwards.total = 1;
            shell.forwards.active = 1;
            vec![Step::Run("status", 900), Step::Run("ll conf/", 900)]
        }
        "emptytab" => {
            open_demo_host(shell, "web-01", cx);
            shell.workspace_name = "Workspace 2".into();
            shell.forwards.total = 1;
            shell.forwards.active = 1;
            shell.tabs.open_empty();
            Vec::new()
        }
        "panes" => {
            open_demo_host(shell, "web-01", cx);
            shell.workspace_name = "Workspace 1".into();
            shell.forwards.total = 1;
            shell.forwards.active = 1;
            vec![
                Step::Run("status", 600),
                Step::Split(PaneDirection::Right),
                Step::Wait(1500),
                Step::Run("tailaccess", 800),
                Step::Split(PaneDirection::Down),
                Step::Wait(1500),
                Step::Run("ll", 800),
            ]
        }
        "local" => {
            shell.open_local(None, cx);
            Vec::new()
        }
        // hack/perf.sh: output floods in a plain bash, then memory with ten tabs.
        "perf" => {
            let profile = muxus_app::model::SessionProfile::Local(muxus_app::model::LocalProfile {
                shell: Some("/bin/bash".into()),
                args: Some(vec!["--noprofile".into(), "--norc".into(), "-i".into()]),
                ..Default::default()
            });
            shell.tabs.open(profile, "perf");
            vec![
                Step::Wait(1500),
                Step::Mark("idle-start"),
                Step::Wait(3000),
                Step::Mark("idle-end"),
                Step::Mark("seq-start"),
                Step::RunToPrompt("time seq 1 2000000"),
                Step::Mark("seq-end"),
                Step::Dump(5),
                Step::Wait(1000),
                Step::Mark("cat-start"),
                Step::RunToPrompt("time cat ~/big.txt"),
                Step::Mark("cat-end"),
                Step::Dump(5),
                Step::Mark("one-tab"),
                Step::OpenLocal(9),
                Step::Wait(6000),
                Step::Mark("ten-tabs"),
                Step::Quit,
            ]
        }
        "localsplit" => {
            shell.open_local(None, cx);
            vec![Step::Wait(600), Step::Split(PaneDirection::Right)]
        }
        other => {
            if let Some(count) = other
                .strip_prefix("tabs:")
                .and_then(|n| n.parse::<usize>().ok())
            {
                for _ in 0..count {
                    // Past the repeat-launch guard: every launch is its own tab.
                    shell.launch_guard = Default::default();
                    shell.open_local(None, cx);
                }
            }
            Vec::new()
        }
    };
    shell.tabs_changed(cx);
    if !steps.is_empty() {
        cx.spawn(async move |this, cx| run_steps(this, steps, cx).await)
            .detach();
    }
}

fn open_demo_host(shell: &mut AppShell, alias: &str, cx: &mut Context<AppShell>) {
    let host = shell
        .catalog
        .ssh
        .hosts
        .iter()
        .find(|host| host.alias == alias)
        .cloned()
        .map(ManagedHost::ssh);
    if let Some(host) = host {
        shell.open_host(&host, true, None, cx);
    }
}

/// Whether the active terminal shows a prompt (`/[$#%] $/` on its last line).
fn prompt_ready(shell: &AppShell, cx: &App) -> bool {
    let Some(view) = shell.active_terminal() else {
        return false;
    };
    let lines = view.read(cx).emulator().recent_lines(1);
    let last = lines.last().map(|line| line.trim_end()).unwrap_or("");
    last.ends_with('$') || last.ends_with('#') || last.ends_with('%')
}

fn type_into_active(shell: &mut AppShell, command: &str, cx: &mut Context<AppShell>) {
    if let Some(view) = shell.active_terminal() {
        view.update(cx, |view, _| {
            view.send_input(format!("{command}\r").into_bytes());
        });
    }
}

async fn run_steps(this: WeakEntity<AppShell>, steps: Vec<Step>, cx: &mut AsyncApp) {
    let executor = cx.background_executor().clone();
    let pause = |ms: u64| executor.timer(Duration::from_millis(ms));
    for step in steps {
        match step {
            Step::Run(command, settle) => {
                for _ in 0..200 {
                    let ready = this
                        .update(cx, |shell, cx| prompt_ready(shell, cx))
                        .unwrap_or(true);
                    if ready {
                        break;
                    }
                    pause(50).await;
                }
                pause(300).await;
                let _ = this.update(cx, |shell, cx| type_into_active(shell, command, cx));
                pause(settle).await;
            }
            Step::RunToPrompt(command) => {
                let ready = |this: &WeakEntity<AppShell>, cx: &mut AsyncApp| {
                    this.update(cx, |shell, cx| prompt_ready(shell, cx))
                        .unwrap_or(true)
                };
                while !ready(&this, cx) {
                    pause(20).await;
                }
                let _ = this.update(cx, |shell, cx| type_into_active(shell, command, cx));
                // The prompt line becomes the command line, then output.
                for _ in 0..100 {
                    if !ready(&this, cx) {
                        break;
                    }
                    pause(10).await;
                }
                while !ready(&this, cx) {
                    pause(20).await;
                }
            }
            Step::Mark(label) => {
                if let Some(perf) = crate::ui::perf::perf() {
                    perf.mark(label);
                }
            }
            Step::Dump(count) => {
                let _ = this.update(cx, |shell, cx| {
                    if let (Some(perf), Some(view)) =
                        (crate::ui::perf::perf(), shell.active_terminal())
                    {
                        perf.note(&view.read(cx).emulator().recent_lines(count).join("\n"));
                    }
                });
            }
            Step::OpenLocal(count) => {
                let _ = this.update(cx, |shell, cx| {
                    for _ in 0..count {
                        shell.launch_guard = Default::default();
                        shell.open_local(None, cx);
                    }
                });
            }
            Step::Quit => {
                let _ = this.update(cx, |_, cx| cx.quit());
            }
            Step::Split(direction) => {
                let _ = this.update(cx, |shell, cx| {
                    let inherit = shell.prefs.split_inherits_session;
                    shell.tabs.split_active_pane(direction, inherit);
                    shell.tabs_changed(cx);
                });
            }
            Step::Wait(ms) => pause(ms).await,
        }
    }
}

/// `MUXUS_SCENE=many`: the demo plus `count` generated hosts in 40 folders,
/// for checking that the windowed tree stays fast with a large catalog
/// (the 700-host case `hack/startup-scale.mjs` measures in the client).
pub fn add_many_hosts(catalog: &mut Catalog, count: usize) {
    let Some(template) = catalog.ssh.hosts.first().cloned() else {
        return;
    };
    for index in 0..count {
        let mut host = template.clone();
        host.alias = format!("node-{index:04}");
        host.aliases = vec![host.alias.clone()];
        host.resolved.hostname = format!("node-{index:04}.fleet.internal");
        let mut metadata = host.metadata.clone().unwrap_or_default();
        metadata.profile_id = format!("generated-{index}");
        metadata.group = Some(format!("Fleet/Rack {:02}", index % 40));
        metadata.display_name = None;
        metadata.color = None;
        host.metadata = Some(metadata);
        catalog.ssh.hosts.push(host);
    }
}
