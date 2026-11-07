//! The Scripts dialog against a live SSH server whose exec runs the local `sh`, in a headless
//! window: a picked host runs the script, its output shows, the script is kept, and closing
//! with unsaved changes asks first.

#![cfg(unix)]

mod support;

use std::time::Duration;

use gpui::{AppContext, TestAppContext};
use muxus_app::features::forwarding::start::{DialAnswer, DialPrompt};
use muxus_app::features::scripts::{ScriptJobs, ScriptJobsEvent, ScriptsDialog};
use muxus_app::hosts::ManagedHost;
use muxus_core::protocol::AuthPromptResponse;
use muxus_session::scripts::HostRunState;
use support::{Fixture, allow_backend_threads, init_ui, wait_until};

#[gpui::test]
fn runs_a_script_on_the_picked_host_and_keeps_it(cx: &mut TestAppContext) {
    let fx = Fixture::start_with(
        |_| {},
        |mut spec| {
            spec.exec_shell = true;
            spec
        },
    );
    allow_backend_threads(cx);
    cx.update(|cx| {
        init_ui(&fx.backend, cx);
    });
    let hosts: Vec<ManagedHost> = fx
        .runtime
        .block_on(fx.backend.ssh_config())
        .expect("the ssh config reads")
        .hosts
        .into_iter()
        .map(ManagedHost::ssh)
        .collect();
    assert_eq!(hosts.len(), 1);

    let backend = fx.backend.clone();
    let jobs = cx.update(|cx| cx.new(|_| ScriptJobs::new(backend.clone())));
    // Answer the dials' questions as the window's prompt dialogs would (the host is
    // usually reached over the fixture's open connection and asks nothing).
    cx.update(|cx| {
        cx.subscribe(&jobs, |_, event: &ScriptJobsEvent, _| {
            if let ScriptJobsEvent::Prompt { prompt, reply, .. } = event {
                let answer = match prompt {
                    DialPrompt::HostKey(_) => DialAnswer::HostKey(true),
                    DialPrompt::Auth(_) => DialAnswer::Auth(Some(AuthPromptResponse {
                        answers: vec!["pw".into()],
                        remember_password: None,
                        skipped: None,
                    })),
                };
                let _ = reply.try_send(answer);
            }
        })
        .detach();
    });
    let dialog_jobs = jobs.clone();
    let window = cx.add_window(move |window, cx| {
        let targets = muxus_app::features::scripts::model::script_targets(&hosts);
        ScriptsDialog::new(
            backend,
            dialog_jobs,
            targets,
            Some(vec!["ssh:box".into()]),
            Some(30.),
            window,
            cx,
        )
    });
    let dialog = window.update(cx, |_, _, cx| cx.entity()).unwrap();
    // Draw a frame of the dialog as it is now.
    let draw = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    };
    draw(cx);
    assert_eq!(
        dialog.read_with(cx, |dialog, _| dialog.chosen().to_vec()),
        ["ssh:box"]
    );

    window
        .update(cx, |dialog, window, cx| {
            dialog.edit_script("Greeting", "echo \"hello from $(echo box)\"\n", window, cx);
            dialog.save(cx);
        })
        .unwrap();
    wait_until(cx, Duration::from_secs(10), "the script to be kept", |cx| {
        dialog.read_with(cx, |dialog, _| {
            dialog
                .saved_scripts()
                .iter()
                .any(|script| script.name == "Greeting")
        })
    });

    draw(cx);
    window.update(cx, |dialog, _, cx| dialog.run(cx)).unwrap();
    assert!(dialog.read_with(cx, |dialog, _| dialog.showing_results()));
    draw(cx);
    wait_until(cx, Duration::from_secs(20), "the job to finish", |cx| {
        jobs.read_with(cx, |jobs, _| {
            jobs.job.as_ref().is_some_and(|job| !job.running())
        })
    });
    let job = jobs.read_with(cx, |jobs, _| jobs.job.clone()).unwrap();
    assert_eq!(job.hosts.len(), 1);
    let host = &job.hosts[0];
    assert_eq!(host.state, HostRunState::Completed, "{host:?}");
    assert_eq!(host.output, "hello from box\n");
    assert_eq!(host.exit_code, Some(0));
    draw(cx);

    // An unsaved edit asks before the dialog closes.
    window
        .update(cx, |dialog, window, cx| {
            dialog.edit_script("Greeting", "uptime\n", window, cx);
            dialog.close(window, cx);
        })
        .unwrap();
    assert!(dialog.read_with(cx, |dialog, _| dialog.asking()));
    draw(cx);
}
