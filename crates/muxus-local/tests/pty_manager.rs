//! Port of `tests/unit/server/pty-manager.test.ts`, plus live sessions on a
//! real pseudo-terminal in a scratch home directory.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use muxus_core::stream::{TerminalSize, TransportChannels, TransportEvent, TransportInput};
use muxus_local::pty::{
    LocalEnvironment, local_pty_args, local_shell_prompt_ready, local_startup_input,
    plan_local_pty, spawn_local_pty,
};
use muxus_local::shell_integration::write_integration_files;
use muxus_local::{LocalProfile, Platform};

#[test]
fn keeps_shell_integration_arguments_and_appends_configured_arguments() {
    let profile = LocalProfile {
        shell: Some("wsl.exe".into()),
        args: Some(vec!["-d".into(), "Ubuntu".into()]),
        ..LocalProfile::default()
    };
    assert_eq!(
        local_pty_args(&profile, &["--integration".to_string()]),
        vec!["--integration", "-d", "Ubuntu"]
    );
}

#[test]
fn turns_multiline_startup_commands_into_pty_enter_presses() {
    assert_eq!(
        local_startup_input(Some("  cd project\nnpm run dev  ")).as_deref(),
        Some("cd project\rnpm run dev\r")
    );
    assert_eq!(
        local_startup_input(Some("a\r\nb")).as_deref(),
        Some("a\rb\r")
    );
    assert_eq!(local_startup_input(Some("  ")), None);
    assert_eq!(local_startup_input(None), None);
}

#[test]
fn waits_for_an_integrated_or_visible_interactive_prompt() {
    assert!(!local_shell_prompt_ready("Loading profile...\r\n"));
    assert!(!local_shell_prompt_ready("Password: "));
    assert!(local_shell_prompt_ready("\x1b]133;A\x07"));
    assert!(local_shell_prompt_ready("\x1b]633;A;Prompt\x1b\\"));
    assert!(local_shell_prompt_ready("\x1b[32muser@host\x1b[0m $ "));
    assert!(local_shell_prompt_ready("PS C:\\work> "));
}

/// A scratch home with no ZDOTDIR: this machine may export one pointing at the
/// real home, and a shell started from it would read the real rc files.
fn scratch_environment(home: &Path) -> LocalEnvironment {
    let mut vars: HashMap<String, String> = HashMap::new();
    for key in ["PATH", "LANG", "USER", "LOGNAME"] {
        if let Ok(value) = std::env::var(key) {
            vars.insert(key.into(), value);
        }
    }
    vars.insert("HOME".into(), home.to_string_lossy().into_owned());
    vars.insert(
        "XDG_CACHE_HOME".into(),
        home.join(".cache").to_string_lossy().into_owned(),
    );
    vars.insert(
        "XDG_CONFIG_HOME".into(),
        home.join(".config").to_string_lossy().into_owned(),
    );
    vars.insert("TERM_PROGRAM".into(), "host-terminal".into());
    vars.insert("KITTY_WINDOW_ID".into(), "7".into());
    let root = home.join(".cache/muxus/shell-integration");
    write_integration_files(&root).expect("write shims");
    LocalEnvironment {
        vars,
        integration_root: Some(root),
        platform: Platform::current(),
    }
}

#[test]
fn plans_the_environment_of_a_local_shell() {
    let home = tempfile::tempdir().unwrap();
    let mut environment = scratch_environment(home.path());
    environment
        .vars
        .insert("ZDOTDIR".into(), "/home/u/.config/zsh".into());
    let plan = plan_local_pty(
        &LocalProfile {
            shell: Some("/bin/zsh".into()),
            ..LocalProfile::default()
        },
        &environment,
    );
    assert_eq!(plan.program, "/bin/zsh");
    assert_eq!(plan.cwd, home.path());
    assert_eq!(plan.env["TERM"], "xterm-256color");
    assert_eq!(plan.env["COLORTERM"], "truecolor");
    assert_eq!(plan.env["TERM_PROGRAM"], "muxus");
    assert!(!plan.env.contains_key("KITTY_WINDOW_ID"));
    assert_eq!(plan.env["MUXUS_USER_ZDOTDIR"], "/home/u/.config/zsh");
    assert!(plan.env["ZDOTDIR"].ends_with("shell-integration/zsh"));
}

struct Collected {
    output: String,
    exit: Option<TransportEvent>,
}

/// Read events until `until` matches the output or the session ends.
fn collect(
    channels: &TransportChannels,
    until: impl Fn(&str) -> bool,
    timeout: Duration,
) -> Collected {
    let deadline = Instant::now() + timeout;
    let mut output = String::new();
    let mut raw = Vec::new();
    while Instant::now() < deadline {
        match channels.events.try_recv() {
            Ok(TransportEvent::Data(data)) => {
                raw.extend_from_slice(&data);
                output = String::from_utf8_lossy(&raw).into_owned();
                if until(&output) {
                    return Collected { output, exit: None };
                }
            }
            Ok(event) => {
                return Collected {
                    output,
                    exit: Some(event),
                };
            }
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(async_channel::TryRecvError::Closed) => break,
        }
    }
    Collected { output, exit: None }
}

fn size() -> TerminalSize {
    TerminalSize {
        cols: 100,
        rows: 30,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(unix)]
#[test]
fn runs_bash_with_shell_integration_and_the_startup_command() {
    if !Path::new("/bin/bash").exists() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let environment = scratch_environment(home.path());
    let profile = LocalProfile {
        shell: Some("/bin/bash".into()),
        startup_command: Some("echo started-$((40+2))\nexit 3".into()),
        ..LocalProfile::default()
    };
    let pty = spawn_local_pty(&profile, size(), &environment).expect("spawn bash");
    assert_eq!(pty.shell, "/bin/bash");
    let first = collect(
        &pty.channels,
        |out| out.contains("started-42"),
        Duration::from_secs(10),
    );
    assert!(
        first.output.contains("\x1b]133;A\x07"),
        "prompt mark missing: {:?}",
        first.output
    );
    assert!(
        first.output.contains("started-42"),
        "startup command did not run: {:?}",
        first.output
    );
    let rest = collect(&pty.channels, |_| false, Duration::from_secs(10));
    assert_eq!(
        rest.exit,
        Some(TransportEvent::Exit {
            code: Some(3),
            signal: None
        })
    );
}

#[cfg(unix)]
#[test]
fn reports_failed_commands_and_cwd_from_zsh() {
    if !Path::new("/bin/zsh").exists() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let environment = scratch_environment(home.path());
    let pty = spawn_local_pty(
        &LocalProfile {
            shell: Some("/bin/zsh".into()),
            ..LocalProfile::default()
        },
        size(),
        &environment,
    )
    .expect("spawn zsh");
    let ready = collect(
        &pty.channels,
        |out| out.contains("\x1b]133;A\x07"),
        Duration::from_secs(10),
    );
    assert!(
        ready.output.contains("\x1b]133;A\x07"),
        "no prompt mark: {:?}",
        ready.output
    );
    let cwd_report = format!("\x1b]133;P;Cwd={}\x07", home.path().display());
    assert!(
        ready.output.contains(&cwd_report),
        "no cwd report: {:?}",
        ready.output
    );

    pty.channels
        .input
        .send_blocking(TransportInput::Data(b"false\r".to_vec()))
        .unwrap();
    let failed = collect(
        &pty.channels,
        |out| out.contains("\x1b]133;D;1\x07"),
        Duration::from_secs(10),
    );
    assert!(
        failed.output.contains("\x1b]133;C\x07"),
        "no preexec mark: {:?}",
        failed.output
    );
    assert!(
        failed.output.contains("\x1b]133;D;1\x07"),
        "no failure mark: {:?}",
        failed.output
    );

    pty.channels
        .input
        .send_blocking(TransportInput::Resize(TerminalSize {
            cols: 120,
            rows: 40,
            ..size()
        }))
        .unwrap();
    pty.channels
        .input
        .send_blocking(TransportInput::Data(
            b"echo $COLUMNS-$TERM_PROGRAM\r".to_vec(),
        ))
        .unwrap();
    let resized = collect(
        &pty.channels,
        |out| out.contains("120-muxus"),
        Duration::from_secs(10),
    );
    assert!(
        resized.output.contains("120-muxus"),
        "resize not seen: {:?}",
        resized.output
    );

    // Closing from the terminal side hangs the shell up and ends the stream.
    pty.channels
        .input
        .send_blocking(TransportInput::Close)
        .unwrap();
    let closed = collect(&pty.channels, |_| false, Duration::from_secs(5));
    assert_eq!(closed.exit, None);
    assert!(pty.channels.events.is_closed());
}
