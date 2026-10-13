//! Ports of `tests/unit/client/local-shell-profile.test.ts` and
//! `tests/unit/electron/login-shell-environment.test.ts`.

use std::collections::HashMap;
use std::io;

use muxus_local::login_env::{import_login_shell_environment, parse_shell_env, strip_ansi};
use muxus_local::profile::{
    local_shell_launch_arguments, opens_wsl_distribution, parse_local_shell_argument_text,
    wsl_shell_profile, wsl_shell_profiles,
};
use muxus_local::{LocalProfile, LocalShellProfileConfig, Platform, WslDistribution};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn preserves_trailing_and_intermediate_blank_rows_while_editing() {
    assert_eq!(
        parse_local_shell_argument_text("-d\n"),
        strings(&["-d", ""])
    );
    assert_eq!(
        parse_local_shell_argument_text("-d\n\nUbuntu"),
        strings(&["-d", "", "Ubuntu"])
    );
    assert_eq!(
        parse_local_shell_argument_text("a\r\nb"),
        strings(&["a", "b"])
    );
    assert_eq!(
        parse_local_shell_argument_text(&"x\n".repeat(100)).len(),
        64
    );
}

#[test]
fn discards_blank_editor_rows_only_when_launching() {
    assert_eq!(
        local_shell_launch_arguments(&strings(&["-d", "", "Ubuntu", ""])),
        Some(strings(&["-d", "Ubuntu"]))
    );
    assert_eq!(local_shell_launch_arguments(&strings(&[""])), None);
}

fn saved(shell: &str, args: &[&str]) -> LocalShellProfileConfig {
    LocalShellProfileConfig {
        id: format!("saved-{shell}-{}", args.join("-")),
        name: "Saved".into(),
        shell: shell.into(),
        args: strings(args),
        cwd: String::new(),
        startup_command: String::new(),
    }
}

fn opens(profile: &LocalShellProfileConfig, name: &str) -> bool {
    opens_wsl_distribution(Some(&profile.shell), Some(&profile.args), name)
}

#[test]
fn opens_a_distribution_in_its_linux_home_directory() {
    let profile = wsl_shell_profile(&WslDistribution {
        name: "Ubuntu-24.04".into(),
        is_default: true,
        icon: None,
    });
    assert_eq!(
        profile,
        LocalShellProfileConfig {
            id: "wsl:Ubuntu-24.04".into(),
            name: "Ubuntu-24.04".into(),
            shell: "wsl.exe".into(),
            args: strings(&["-d", "Ubuntu-24.04", "--cd", "~"]),
            cwd: String::new(),
            startup_command: String::new(),
        }
    );
    assert_eq!(
        profile.launch_profile(),
        LocalProfile {
            shell: Some("wsl.exe".into()),
            args: Some(strings(&["-d", "Ubuntu-24.04", "--cd", "~"])),
            cwd: None,
            startup_command: None,
        }
    );
}

#[test]
fn recognises_a_saved_profile_that_already_starts_the_distribution() {
    assert!(opens(&saved("wsl.exe", &["-d", "Ubuntu"]), "ubuntu"));
    assert!(opens(
        &saved("WSL", &["--distribution", "Ubuntu"]),
        "Ubuntu"
    ));
    assert!(opens(
        &saved(r"C:\Windows\System32\wsl.exe", &["-d", "Ubuntu"]),
        "Ubuntu"
    ));
    assert!(!opens(&saved("wsl.exe", &["-d", "Debian"]), "Ubuntu"));
    assert!(!opens(&saved("wsl.exe", &[]), "Ubuntu"));
    assert!(!opens(&saved("pwsh.exe", &["-d", "Ubuntu"]), "Ubuntu"));
}

#[test]
fn recognises_the_distribution_an_open_tab_started() {
    assert!(opens_wsl_distribution(
        Some("wsl.exe"),
        Some(&strings(&["-d", "Ubuntu"])),
        "Ubuntu"
    ));
    assert!(!opens_wsl_distribution(Some("wsl.exe"), None, "Ubuntu"));
    assert!(!opens_wsl_distribution(
        None,
        Some(&strings(&["-d", "Ubuntu"])),
        "Ubuntu"
    ));
}

#[test]
fn offers_only_the_distributions_no_saved_profile_covers() {
    let offered = wsl_shell_profiles(
        &[
            WslDistribution {
                name: "Ubuntu".into(),
                is_default: true,
                icon: None,
            },
            WslDistribution {
                name: "Debian".into(),
                is_default: false,
                icon: None,
            },
        ],
        &[saved("wsl.exe", &["-d", "Ubuntu"])],
    );
    assert_eq!(
        offered
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Debian"]
    );
}

#[test]
fn saved_profiles_keep_the_preference_file_shape() {
    let json = serde_json::to_value(saved("wsl.exe", &["-d"])).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "id": "saved-wsl.exe--d", "name": "Saved", "shell": "wsl.exe",
            "args": ["-d"], "cwd": "", "startupCommand": ""
        })
    );
}

fn environment(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn imports_path_and_ssh_auth_sock_for_a_finder_launched_macos_app() {
    let mut env = environment(&[
        ("PATH", "/usr/bin"),
        (
            "SSH_AUTH_SOCK",
            "/private/tmp/com.apple.launchd/default-agent",
        ),
    ]);
    import_login_shell_environment(
        Platform::MacOs,
        &mut env,
        || {
            Ok(environment(&[
                ("PATH", "/opt/homebrew/bin:/usr/bin"),
                (
                    "SSH_AUTH_SOCK",
                    "/Users/alice/Library/Group Containers/1password/agent.sock",
                ),
            ]))
        },
        None,
    );
    assert_eq!(
        env,
        environment(&[
            ("PATH", "/opt/homebrew/bin:/usr/bin"),
            (
                "SSH_AUTH_SOCK",
                "/Users/alice/Library/Group Containers/1password/agent.sock"
            ),
        ])
    );
}

#[test]
fn does_not_replace_the_launch_environment_when_the_shell_has_no_agent_socket() {
    let mut env = environment(&[
        ("PATH", "/usr/bin"),
        (
            "SSH_AUTH_SOCK",
            "/private/tmp/com.apple.launchd/default-agent",
        ),
    ]);
    import_login_shell_environment(
        Platform::MacOs,
        &mut env,
        || Ok(environment(&[("PATH", "/opt/homebrew/bin:/usr/bin")])),
        None,
    );
    assert_eq!(
        env["SSH_AUTH_SOCK"],
        "/private/tmp/com.apple.launchd/default-agent"
    );
}

#[test]
fn keeps_ssh_auth_sock_unchanged_on_linux_and_skips_shell_startup_on_windows() {
    let mut linux = environment(&[
        ("PATH", "/usr/bin"),
        ("SSH_AUTH_SOCK", "/run/user/1000/agent.sock"),
    ]);
    import_login_shell_environment(
        Platform::Linux,
        &mut linux,
        || {
            Ok(environment(&[
                ("PATH", "/home/alice/bin:/usr/bin"),
                ("SSH_AUTH_SOCK", "/tmp/other.sock"),
            ]))
        },
        None,
    );
    assert_eq!(
        linux,
        environment(&[
            ("PATH", "/home/alice/bin:/usr/bin"),
            ("SSH_AUTH_SOCK", "/run/user/1000/agent.sock")
        ])
    );

    let mut called = false;
    let mut windows = environment(&[("PATH", r"C:\Windows\System32")]);
    import_login_shell_environment(
        Platform::Windows,
        &mut windows,
        || {
            called = true;
            Ok(environment(&[("PATH", "ignored")]))
        },
        None,
    );
    assert!(!called);
    assert_eq!(windows["PATH"], r"C:\Windows\System32");
}

#[test]
fn leaves_the_current_environment_intact_when_shell_startup_fails() {
    let mut env = environment(&[("PATH", "/usr/bin"), ("SSH_AUTH_SOCK", "/tmp/current.sock")]);
    import_login_shell_environment(
        Platform::MacOs,
        &mut env,
        || Err(io::Error::other("shell failed")),
        None,
    );
    assert_eq!(
        env,
        environment(&[("PATH", "/usr/bin"), ("SSH_AUTH_SOCK", "/tmp/current.sock")])
    );
}

#[test]
fn reports_a_failed_shell_startup_to_the_diagnostics_hook() {
    let mut reported = Vec::new();
    let mut on_error = |err: &io::Error| reported.push(err.to_string());
    import_login_shell_environment(
        Platform::MacOs,
        &mut environment(&[("PATH", "/usr/bin")]),
        || Err(io::Error::other("shell failed")),
        Some(&mut on_error),
    );
    assert_eq!(reported, vec!["shell failed"]);
}

#[test]
fn parses_env_output_between_the_delimiters() {
    let output = "motd\x1b[1m!\x1b[0m\n_SHELL_ENV_DELIMITER_PATH=/a:/b\nEMPTY=\nEQ=a=b\n\x1b]0;title\x07LANG=C\n_SHELL_ENV_DELIMITER_";
    assert_eq!(
        parse_shell_env(output),
        environment(&[
            ("PATH", "/a:/b"),
            ("EMPTY", ""),
            ("EQ", "a=b"),
            ("LANG", "C")
        ])
    );
    assert_eq!(strip_ansi("\x1b[31mred\x1b[0m \u{9b}1mx"), "red x");
}
