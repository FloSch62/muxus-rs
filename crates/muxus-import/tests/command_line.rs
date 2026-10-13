//! Port of `tests/unit/electron/command-line.test.ts`.

use muxus_core::api::{AppWindowLaunch, CommandLineLaunch};
use muxus_core::protocol::{LocalProfile, SessionProfile};
use muxus_import::launch::{
    ConnectOptions, can_handle_command_line_launch, link_launch, parse_command_line_launch,
    parse_command_line_launch_data, parse_connect_target,
};
use serde_json::json;

fn host(name: &str) -> Option<CommandLineLaunch> {
    Some(CommandLineLaunch::Host {
        name: name.to_owned(),
    })
}

fn connect(name: &str) -> Option<CommandLineLaunch> {
    Some(CommandLineLaunch::Connect {
        name: name.to_owned(),
    })
}

fn url(name: &str) -> Option<CommandLineLaunch> {
    Some(CommandLineLaunch::Url {
        name: name.to_owned(),
    })
}

fn target(value: &str) -> Option<String> {
    parse_connect_target(value, &ConnectOptions::default())
}

fn with_port(value: &str, port: &str) -> Option<String> {
    parse_connect_target(
        value,
        &ConnectOptions {
            port: Some(port.to_owned()),
            ..ConnectOptions::default()
        },
    )
}

#[test]
fn accepts_split_and_equals_forms_among_unrelated_electron_arguments() {
    assert_eq!(
        parse_command_line_launch(&["muxus", "--host", "edge-1"]),
        host("edge-1")
    );
    assert_eq!(
        parse_command_line_launch(&["electron", ".", "--inspect=9229", "--folder=Production/EU"]),
        Some(CommandLineLaunch::Folder {
            name: "Production/EU".to_owned()
        })
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--workspace", "Night shift"]),
        Some(CommandLineLaunch::Workspace {
            name: "Night shift".to_owned()
        })
    );
}

#[test]
fn ignores_invocations_without_a_desktop_launch_target() {
    assert_eq!(parse_command_line_launch(&["muxus"]), None);
    assert_eq!(parse_command_line_launch(&["muxus", "--no-sandbox"]), None);
}

#[test]
fn rejects_missing_empty_oversized_and_competing_targets() {
    assert_eq!(parse_command_line_launch(&["muxus", "--host"]), None);
    assert_eq!(parse_command_line_launch(&["muxus", "--host="]), None);
    assert_eq!(
        parse_command_line_launch(&["muxus", "--host", "--workspace", "Lab"]),
        None
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--host", "edge", "--folder", "Lab"]),
        None
    );
    let oversized = format!("--host={}", "x".repeat(501));
    assert_eq!(
        parse_command_line_launch(&["muxus", oversized.as_str()]),
        None
    );
}

#[test]
fn validates_structured_second_instance_launch_data() {
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "workspace", "name": " Night shift " })),
        Some(CommandLineLaunch::Workspace {
            name: "Night shift".to_owned()
        })
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "host", "name": "edge-1" })),
        host("edge-1")
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "unknown", "name": "edge-1" })),
        None
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "folder", "name": "" })),
        None
    );
}

#[test]
fn revalidates_forwarded_ad_hoc_connection_targets() {
    assert_eq!(
        parse_command_line_launch_data(
            &json!({ "kind": "connect", "name": "admin@10.10.10.1:2222" })
        ),
        connect("admin@10.10.10.1:2222")
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "connect", "name": "[fe80::1]:2222" })),
        connect("[fe80::1]:2222")
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "connect", "name": "edge;reboot" })),
        None
    );
}

#[test]
fn accepts_a_user_host_port_target_in_split_and_equals_forms() {
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "admin@10.10.10.1"]),
        connect("admin@10.10.10.1")
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect=admin@10.10.10.1:2222"]),
        connect("admin@10.10.10.1:2222")
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "edge-1.example.test"]),
        connect("edge-1.example.test")
    );
}

#[test]
fn folds_separate_user_and_port_flags_into_the_target() {
    assert_eq!(
        parse_command_line_launch(&[
            "muxus",
            "--connect",
            "10.10.10.1",
            "--user",
            "admin",
            "--port",
            "2222",
        ]),
        connect("admin@10.10.10.1:2222")
    );
    assert_eq!(
        parse_command_line_launch(&[
            "muxus",
            "--port=2222",
            "--user=admin",
            "--connect=10.10.10.1"
        ]),
        connect("admin@10.10.10.1:2222")
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "edge", "--port", "0022"]),
        connect("edge:22")
    );
}

#[test]
fn brackets_ipv6_hosts_only_when_a_port_follows_them() {
    assert_eq!(target("fe80::1").as_deref(), Some("fe80::1"));
    assert_eq!(
        with_port("admin@fe80::1", "2222").as_deref(),
        Some("admin@[fe80::1]:2222")
    );
    assert_eq!(
        target("[2001:db8::5]:830").as_deref(),
        Some("[2001:db8::5]:830")
    );
    assert_eq!(target("[2001:db8::5]").as_deref(), Some("2001:db8::5"));
    assert_eq!(
        with_port("fe80::1%eth0", "22").as_deref(),
        Some("[fe80::1%eth0]:22")
    );
}

#[test]
fn rejects_a_user_or_port_given_twice() {
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "admin@edge", "--user", "root"]),
        None
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "edge:22", "--port", "2222"]),
        None
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "edge", "--port", "22", "--port", "23"]),
        None
    );
}

#[test]
fn rejects_user_and_port_without_connect() {
    assert_eq!(
        parse_command_line_launch(&["muxus", "--host", "edge", "--port", "22"]),
        None
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--user", "admin"]),
        None
    );
}

#[test]
fn rejects_malformed_ports_and_shell_sensitive_names() {
    for value in [
        "edge:0",
        "edge:65536",
        "edge:",
        "edge:ssh",
        "@edge",
        "admin@",
        "-oProxyCommand=sh",
        "admin@-edge",
        "edge;reboot",
        "edge$(id)",
        "ad min@edge",
        "`id`@edge",
        "[edge;id]:22",
    ] {
        assert_eq!(target(value), None, "{value}");
    }
    assert_eq!(
        parse_connect_target(
            "edge",
            &ConnectOptions {
                user: Some("-oProxyCommand=sh".to_owned()),
                ..ConnectOptions::default()
            }
        ),
        None
    );
    assert_eq!(with_port("edge", "22;id"), None);
    assert_eq!(
        parse_command_line_launch(&["muxus", "--connect", "edge", "--user", "a|b"]),
        None
    );
}

#[test]
fn takes_an_ssh_or_telnet_link_from_the_system_url_handler() {
    assert_eq!(
        parse_command_line_launch(&["/opt/Muxus/muxus", "ssh://admin@10.0.0.1:2222"]),
        url("ssh://admin@10.0.0.1:2222")
    );
    // Windows: "C:\…\Muxus.exe" "%1"; a source checkout: electron <app> <link>.
    assert_eq!(
        parse_command_line_launch(&["C:\\Muxus\\Muxus.exe", "telnet://switch:23"]),
        url("telnet://switch:23")
    );
    assert_eq!(
        parse_command_line_launch(&[
            "electron",
            "/src/muxus/electron",
            "--no-sandbox",
            "ssh://edge"
        ]),
        url("ssh://edge")
    );
}

#[test]
fn forwards_malformed_and_unsupported_links_so_the_window_can_report_them() {
    assert_eq!(
        parse_command_line_launch(&["muxus", "ssh://-oProxyCommand=id"]),
        url("ssh://-oProxyCommand=id")
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "sftp://host"]),
        url("sftp://host")
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "ssh:host"]),
        url("ssh:host")
    );
}

#[test]
fn caps_an_oversized_link_one_character_past_the_limit() {
    let link = format!("ssh://{}", "a".repeat(5000));
    let launch = parse_command_line_launch(&["muxus", link.as_str()]);
    let Some(CommandLineLaunch::Url { name }) = launch else {
        panic!("not a link launch");
    };
    assert_eq!(name.len(), 2049);
}

#[test]
fn does_not_mistake_the_executable_switches_or_flag_values_for_links() {
    assert_eq!(parse_command_line_launch(&["ssh://not-a-link"]), None);
    assert_eq!(
        parse_command_line_launch(&["muxus", "--host", "ssh://edge"]),
        host("ssh://edge")
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--inspect=ssh://x"]),
        None
    );
}

#[test]
fn accepts_one_launch_target_at_a_time() {
    assert_eq!(
        parse_command_line_launch(&["muxus", "ssh://a", "ssh://b"]),
        None
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "--host", "edge", "ssh://a"]),
        None
    );
    assert_eq!(
        parse_command_line_launch(&["muxus", "ssh://a", "--user", "root"]),
        None
    );
}

#[test]
fn revalidates_forwarded_and_macos_open_url_links() {
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "url", "name": "ssh://edge" })),
        url("ssh://edge")
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "url", "name": "--host=edge" })),
        None
    );
    assert_eq!(
        parse_command_line_launch_data(&json!({ "kind": "url", "name": 42 })),
        None
    );
    assert_eq!(link_launch("telnet://switch"), url("telnet://switch"));
    assert_eq!(link_launch("/Users/me/file.txt"), None);
}

#[test]
fn excludes_sftp_only_and_settings_windows_from_launch_request_delivery() {
    assert!(can_handle_command_line_launch(None));
    assert!(can_handle_command_line_launch(Some(
        &AppWindowLaunch::Workspace {
            workspace_id: Some("operations".to_owned()),
            title: "Operations".to_owned(),
        }
    )));
    assert!(can_handle_command_line_launch(Some(
        &AppWindowLaunch::Session {
            profile: SessionProfile::Local(LocalProfile::default()),
            title: "Local".to_owned(),
            color: None,
        }
    )));
    assert!(!can_handle_command_line_launch(Some(
        &AppWindowLaunch::Sftp {
            conn_id: "ssh-1".to_owned(),
            title: "Files".to_owned(),
            path: None,
        }
    )));
    assert!(!can_handle_command_line_launch(Some(
        &AppWindowLaunch::Settings {
            section: Some("terminal".to_owned()),
            item: None,
        }
    )));
}
