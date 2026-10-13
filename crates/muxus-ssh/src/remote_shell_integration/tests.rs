//! Port of tests/unit/server/remote-shell-integration.test.ts and the remote part of
//! shell-integration.test.ts.

use std::sync::Mutex;

use super::*;

#[derive(Default)]
struct Stub {
    probe_output: Option<String>,
    complete: bool,
    lose_transport_on_probe: bool,
    lose_transport_on_shell: bool,
    shell_error: Option<String>,
    log: Mutex<Vec<String>>,
    commands: Mutex<Vec<String>>,
    writes: Mutex<Vec<(String, String)>>,
    lost: Mutex<Option<Option<String>>>,
}

impl Stub {
    fn log(&self, entry: &str) {
        self.log.lock().unwrap().push(entry.to_string());
    }
}

struct StubSftp<'a>(&'a Stub);

impl RemoteShellSftp for StubSftp<'_> {
    async fn stat(&self, path: &str) -> Result<RemoteFileKind, String> {
        self.0.log("stat");
        if path.ends_with("/.complete") {
            return if self.0.complete {
                Ok(RemoteFileKind::File)
            } else {
                Err("not found".into())
            };
        }
        Ok(RemoteFileKind::Directory)
    }
    async fn mkdir(&self, _path: &str, _mode: u32) -> Result<(), String> {
        self.0.log("mkdir");
        Ok(())
    }
    async fn write_file(&self, path: &str, content: &str, _mode: u32) -> Result<(), String> {
        self.0
            .writes
            .lock()
            .unwrap()
            .push((path.to_string(), content.to_string()));
        Ok(())
    }
}

impl<'s> RemoteShellHost for &'s Stub {
    type Channel = &'static str;
    type Sftp = StubSftp<'s>;

    async fn probe(&self, command: &str) -> Result<String, String> {
        self.commands.lock().unwrap().push(command.to_string());
        self.log("probe-start");
        if self.lose_transport_on_probe {
            *self.lost.lock().unwrap() = Some(Some("remote protocol disconnect".into()));
        }
        self.log("probe-finish");
        Ok(self.probe_output.clone().unwrap_or_default())
    }
    async fn sftp(&self) -> Result<StubSftp<'s>, String> {
        self.log("sftp-start");
        Ok(StubSftp(self))
    }
    async fn open_exec(&self, command: &str) -> Result<&'static str, String> {
        self.commands.lock().unwrap().push(command.to_string());
        Ok("terminal")
    }
    async fn open_shell(&self) -> Result<&'static str, String> {
        self.log("shell");
        if self.lose_transport_on_shell {
            *self.lost.lock().unwrap() = Some(None);
        }
        match &self.shell_error {
            Some(error) => Err(error.clone()),
            None => Ok("plain"),
        }
    }
    fn transport_lost(&self) -> Option<Option<String>> {
        self.lost.lock().unwrap().clone()
    }
}

fn is_hash_dir(segment: &str) -> bool {
    segment.len() == 12
        && segment
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[test]
fn the_content_hash_matches_the_typescript_build() {
    assert_eq!(
        integration_version(),
        scripts::TYPESCRIPT_INTEGRATION_VERSION
    );
    assert!(is_hash_dir(&integration_version()));
}

#[tokio::test]
async fn probes_installs_and_opens_an_integrated_remote_shell() {
    let stub = Stub {
        probe_output: Some(
            "__MUXUS_SHELL__=/usr/bin/zsh\n__MUXUS_ZDOTDIR__=\n__MUXUS_HOME__=/home/u\n".into(),
        ),
        ..Stub::default()
    };
    let result = open_remote_shell(&&stub, None).await.unwrap();
    assert_eq!(result, "terminal");
    assert_eq!(
        stub.log.lock().unwrap()[..3],
        ["probe-start", "probe-finish", "sftp-start"]
    );
    let commands = stub.commands.lock().unwrap().clone();
    assert!(commands[0].contains("\"${SHELL-}\" \"${ZDOTDIR-}\" \"${HOME-}\""));
    assert!(!commands[0].contains("\\${SHELL-}"));
    let version = integration_version();
    assert!(commands[1].contains(&format!(
        "export ZDOTDIR='/home/u/.cache/muxus/shell-integration/{version}/zsh'"
    )));
    let writes: Vec<String> = stub
        .writes
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    assert_eq!(writes.len(), 4);
    assert!(writes[0].ends_with(&format!("/shell-integration/{version}/zsh/.zshenv")));
    assert!(writes[1].ends_with(&format!("/shell-integration/{version}/zsh/.zprofile")));
    assert!(writes[2].ends_with(&format!("/shell-integration/{version}/zsh/.zshrc")));
    assert!(writes[3].ends_with(&format!("/shell-integration/{version}/.complete")));
}

#[tokio::test]
async fn reuses_a_completed_content_addressed_install_without_remote_writes() {
    let stub = Stub {
        probe_output: Some(
            "__MUXUS_SHELL__=/bin/bash\n__MUXUS_ZDOTDIR__=\n__MUXUS_HOME__=/home/u\n".into(),
        ),
        complete: true,
        ..Stub::default()
    };
    assert_eq!(open_remote_shell(&&stub, None).await.unwrap(), "terminal");
    let log = stub.log.lock().unwrap().clone();
    assert_eq!(log.iter().filter(|e| *e == "stat").count(), 1);
    assert!(!log.contains(&"mkdir".to_string()));
    assert!(stub.writes.lock().unwrap().is_empty());
    let commands = stub.commands.lock().unwrap().clone();
    assert!(commands[1].contains(&format!(
        "--rcfile '/home/u/.cache/muxus/shell-integration/{}/bash-init.bash'",
        integration_version()
    )));
}

#[tokio::test]
async fn does_not_request_sftp_when_the_remote_is_not_a_supported_unix_shell() {
    let stub = Stub {
        probe_output: Some("console appliance\n".into()),
        ..Stub::default()
    };
    assert_eq!(open_remote_shell(&&stub, None).await.unwrap(), "plain");
    let log = stub.log.lock().unwrap().clone();
    assert!(!log.contains(&"sftp-start".to_string()));
    assert_eq!(log.iter().filter(|e| *e == "shell").count(), 1);
}

#[tokio::test]
async fn surfaces_a_transport_lost_during_the_optional_shell_probe() {
    let stub = Stub {
        lose_transport_on_probe: true,
        ..Stub::default()
    };
    let err = open_remote_shell(&&stub, None).await.unwrap_err();
    assert!(matches!(err, OpenShellError::TransportLost(_)));
}

#[tokio::test]
async fn keeps_watching_for_transport_loss_while_the_plain_fallback_opens() {
    let stub = Stub {
        lose_transport_on_shell: true,
        shell_error: Some("Not connected".into()),
        ..Stub::default()
    };
    let err = open_remote_shell(&&stub, None).await.unwrap_err();
    assert!(matches!(err, OpenShellError::TransportLost(_)));
}

#[test]
fn detects_supported_shells_among_startup_output() {
    assert_eq!(
        parse_shell_probe(
            "banner\n__MUXUS_SHELL__=/usr/bin/zsh\n__MUXUS_ZDOTDIR__=/home/u/.config/zsh\n__MUXUS_HOME__=/home/u\n"
        ),
        Some(SupportedShell {
            path: "/usr/bin/zsh".into(),
            kind: ShellKind::Zsh,
            home: "/home/u".into(),
            zdotdir: Some("/home/u/.config/zsh".into()),
        })
    );
    assert_eq!(
        parse_shell_probe(
            "__MUXUS_SHELL__=/bin/bash\n__MUXUS_ZDOTDIR__=\n__MUXUS_HOME__=/home/u\n"
        ),
        Some(SupportedShell {
            path: "/bin/bash".into(),
            kind: ShellKind::Bash,
            home: "/home/u".into(),
            zdotdir: None
        })
    );
}

#[test]
fn rejects_unsupported_and_missing_shells() {
    assert_eq!(
        parse_shell_probe(
            "__MUXUS_SHELL__=/usr/bin/fish\n__MUXUS_ZDOTDIR__=\n__MUXUS_HOME__=/home/u\n"
        ),
        None
    );
    assert_eq!(
        parse_shell_probe("__MUXUS_SHELL__=/bin/bash\n__MUXUS_ZDOTDIR__=\n"),
        None
    );
    assert_eq!(parse_shell_probe(""), None);
}

fn zsh(zdotdir: Option<&str>) -> SupportedShell {
    SupportedShell {
        path: "/usr/bin/zsh".into(),
        kind: ShellKind::Zsh,
        home: "/home/u".into(),
        zdotdir: zdotdir.map(Into::into),
    }
}

fn bash() -> SupportedShell {
    SupportedShell {
        path: "/bin/bash".into(),
        kind: ShellKind::Bash,
        home: "/home/u".into(),
        zdotdir: None,
    }
}

#[test]
fn starts_zsh_through_the_cached_shim_and_preserves_a_custom_zdotdir() {
    assert_eq!(
        remote_shell_command(
            &zsh(Some("/home/o'hara/.config/zsh")),
            "/home/u/.cache/muxus",
            None
        ),
        "export MUXUS_USER_ZDOTDIR='/home/o'\\''hara/.config/zsh'; export ZDOTDIR='/home/u/.cache/muxus/zsh'; exec '/usr/bin/zsh' -l"
    );
}

#[test]
fn starts_interactive_bash_through_its_cached_init_file() {
    assert_eq!(
        remote_shell_command(&bash(), "/home/u/.cache/muxus", None),
        "exec '/bin/bash' --noprofile --rcfile '/home/u/.cache/muxus/bash-init.bash' -i"
    );
}

#[test]
fn hands_a_reconnect_directory_to_the_startup_files_as_quoted_data() {
    let cwd = "/srv/team's $(project)";
    assert_eq!(
        remote_shell_command(&bash(), "/home/u/.cache/muxus", Some(cwd)),
        "export MUXUS_RESTORE_CWD='/srv/team'\\''s $(project)'; exec '/bin/bash' --noprofile --rcfile '/home/u/.cache/muxus/bash-init.bash' -i"
    );
    assert_eq!(
        remote_shell_command(&zsh(None), "/home/u/.cache/muxus", Some(cwd)),
        "export MUXUS_RESTORE_CWD='/srv/team'\\''s $(project)'; unset MUXUS_USER_ZDOTDIR; export ZDOTDIR='/home/u/.cache/muxus/zsh'; exec '/usr/bin/zsh' -l"
    );
}

#[test]
fn only_restores_absolute_unix_directories() {
    let plain = remote_shell_command(&bash(), "/home/u/.cache/muxus", None);
    assert_eq!(
        remote_shell_command(&bash(), "/home/u/.cache/muxus", Some("relative/path")),
        plain
    );
    assert_eq!(
        remote_shell_command(&bash(), "/home/u/.cache/muxus", Some("C:\\work")),
        plain
    );
    assert_eq!(
        remote_shell_command(&bash(), "/home/u/.cache/muxus", Some("/srv/bad\0path")),
        plain
    );
}

#[tokio::test]
async fn changes_into_the_reconnect_directory_at_the_end_of_shell_startup() {
    let stub = Stub {
        probe_output: Some("__MUXUS_SHELL__=/usr/bin/zsh\n__MUXUS_HOME__=/home/u\n".into()),
        ..Stub::default()
    };
    open_remote_shell(&&stub, Some("/srv/project"))
        .await
        .unwrap();
    let writes = stub.writes.lock().unwrap().clone();
    let zshrc = &writes
        .iter()
        .find(|(p, _)| p.ends_with("/zsh/.zshrc"))
        .unwrap()
        .1;
    assert!(
        stub.commands
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("export MUXUS_RESTORE_CWD='/srv/project'; ")
    );
    // After the user's .zshrc and the integration hooks, so neither undoes it.
    assert!(
        zshrc.find("builtin cd -- \"$MUXUS_RESTORE_CWD\"").unwrap()
            > zshrc.find("add-zsh-hook precmd").unwrap()
    );
    assert!(
        zshrc
            .trim_end()
            .ends_with("builtin unset MUXUS_RESTORE_CWD")
    );
}

// shell-integration.test.ts: the scripts the remote install shares with local terminals.

#[test]
fn emits_osc_133_command_reports_from_the_zsh_shim() {
    assert!(scripts::ZSHRC.contains("\\e]133;D;%s\\a"));
    assert!(scripts::ZSHRC.contains("\\e]133;C\\a"));
    assert!(scripts::ZSHRC.contains(scripts::SHELL_CWD_REPORT));
}

#[test]
fn sources_the_user_zsh_startup_files_before_hooking() {
    let rc = scripts::ZSHRC;
    assert!(
        rc.find("builtin source \"${ZDOTDIR:-$HOME}/.zshrc\"")
            .unwrap()
            < rc.find("add-zsh-hook precmd").unwrap()
    );
    assert!(ZSHENV.contains("builtin source \"${ZDOTDIR:-$HOME}/.zshenv\""));
}

#[test]
fn reports_via_ps0_and_prompt_command_in_the_bash_shim() {
    assert!(BASH_INIT.contains("PS0=\"\\[\\e]133;C\\a\\]${PS0-}\""));
    assert!(
        BASH_INIT
            .contains("PROMPT_COMMAND=\"__muxus_prompt_mark${PROMPT_COMMAND:+;$PROMPT_COMMAND}\"")
    );
    assert!(BASH_INIT.contains(scripts::SHELL_CWD_REPORT));
    assert!(BASH_INIT.starts_with("# Muxus SSH shell integration. Reproduce login startup first."));
}
