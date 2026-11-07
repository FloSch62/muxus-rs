//! Port of `tests/unit/shared/environments.test.ts`: the commands that enter
//! a container, pod or console, their quoting, tab titles and the profile
//! schemas that carry a target.

use muxus_core::environments::{
    ContainerRuntime, ContainerShell, EnvironmentTarget, LibvirtUri, environment_argv,
    environment_command_line, environment_title, is_valid_environment_target, shell_quote,
};
use muxus_core::protocol::{LocalProfile, SessionProfile, SshProfile};
use muxus_core::validate::parse_value;
use serde_json::{Value, json};

fn argv(target: EnvironmentTarget) -> Vec<String> {
    environment_argv(&target).unwrap()
}

fn container(
    runtime: ContainerRuntime,
    name: &str,
    shell: Option<ContainerShell>,
) -> EnvironmentTarget {
    EnvironmentTarget::Container {
        runtime,
        container: name.into(),
        shell,
    }
}

fn pod(namespace: &str, name: &str) -> EnvironmentTarget {
    EnvironmentTarget::Pod {
        context: None,
        namespace: namespace.into(),
        pod: name.into(),
        container: None,
    }
}

#[test]
fn enters_a_container_with_the_best_shell_it_has() {
    assert_eq!(
        argv(container(ContainerRuntime::Docker, "web-1", None)),
        [
            "docker",
            "exec",
            "-it",
            "-e",
            "TERM=xterm-256color",
            "web-1",
            "sh",
            "-c",
            "if command -v bash >/dev/null 2>&1; then exec bash; elif command -v ash >/dev/null 2>&1; then exec ash; else exec sh; fi",
        ]
    );
    let podman = argv(container(
        ContainerRuntime::Podman,
        "db",
        Some(ContainerShell::Bash),
    ));
    assert_eq!(&podman[podman.len() - 2..], ["db", "bash"]);
}

#[test]
fn follows_container_and_pod_logs() {
    assert_eq!(
        argv(EnvironmentTarget::ContainerLogs {
            runtime: ContainerRuntime::Docker,
            container: "web".into(),
        }),
        ["docker", "logs", "--follow", "--tail", "1000", "web"]
    );
    assert_eq!(
        argv(EnvironmentTarget::PodLogs {
            context: Some("kind-dev".into()),
            namespace: "default".into(),
            pod: "worker-5d4b8-zq1rt".into(),
            container: Some("worker".into()),
            previous: Some(true),
        }),
        [
            "kubectl",
            "--context=kind-dev",
            "--namespace=default",
            "logs",
            "worker-5d4b8-zq1rt",
            "--container=worker",
            "--tail=1000",
            "--previous",
        ]
    );
}

#[test]
fn passes_kube_contexts_as_one_option_so_a_free_form_name_cannot_split() {
    let argv = argv(EnvironmentTarget::Pod {
        context: Some("arn:aws:eks:eu-central-1:1234:cluster/prod one".into()),
        namespace: "kube-system".into(),
        pod: "coredns-abc".into(),
        container: None,
    });
    assert_eq!(
        &argv[..7],
        [
            "kubectl",
            "--context=arn:aws:eks:eu-central-1:1234:cluster/prod one",
            "--namespace=kube-system",
            "exec",
            "-it",
            "coredns-abc",
            "--",
        ]
    );
}

#[test]
fn enters_proxmox_guests_and_libvirt_consoles() {
    assert_eq!(
        argv(EnvironmentTarget::ProxmoxLxc { vmid: 201 }),
        ["pct", "enter", "201"]
    );
    assert_eq!(
        argv(EnvironmentTarget::ProxmoxSerial { vmid: 101 }),
        ["qm", "terminal", "101"]
    );
    assert_eq!(
        argv(EnvironmentTarget::LibvirtConsole {
            domain: "runner".into(),
            uri: Some(LibvirtUri::System),
        }),
        [
            "virsh",
            "-c",
            "qemu:///system",
            "console",
            "--force",
            "runner"
        ]
    );
}

#[test]
fn refuses_names_that_could_read_as_options_or_carry_shell_syntax() {
    let bad = [
        container(ContainerRuntime::Docker, "-v", None),
        container(ContainerRuntime::Docker, "web;rm -rf /", None),
        pod("default", "Pod"),
        EnvironmentTarget::Pod {
            context: Some("-x".into()),
            namespace: "default".into(),
            pod: "web".into(),
            container: None,
        },
        EnvironmentTarget::ProxmoxLxc { vmid: 5 },
        EnvironmentTarget::LibvirtConsole {
            domain: "--help".into(),
            uri: None,
        },
    ];
    for target in bad {
        assert!(!is_valid_environment_target(&target), "{target:?}");
        assert!(environment_argv(&target).is_err());
    }
    // A URI other than the two known ones cannot even be read.
    assert!(
        serde_json::from_value::<EnvironmentTarget>(json!({
            "kind": "libvirt-console", "domain": "vm", "uri": "qemu+ssh://evil/system"
        }))
        .is_err()
    );
}

#[test]
fn leaves_plain_words_alone_and_quotes_the_rest() {
    assert_eq!(shell_quote("web-1"), "web-1");
    assert_eq!(shell_quote("--context=kind-dev"), "--context=kind-dev");
    assert_eq!(shell_quote("two words"), "'two words'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote("$(id)"), "'$(id)'");
}

#[test]
fn builds_the_command_line_an_ssh_session_runs() {
    assert_eq!(
        environment_command_line(&EnvironmentTarget::LibvirtConsole {
            domain: "my vm".into(),
            uri: None,
        })
        .unwrap(),
        "virsh console --force 'my vm'"
    );
    assert_eq!(
        environment_command_line(&container(
            ContainerRuntime::Docker,
            "web",
            Some(ContainerShell::Sh)
        ))
        .unwrap(),
        "docker exec -it -e TERM=xterm-256color web sh"
    );
}

#[test]
fn names_tabs_after_what_they_enter() {
    assert_eq!(
        environment_title(&container(ContainerRuntime::Docker, "nginx", None)),
        "nginx"
    );
    assert_eq!(
        environment_title(&EnvironmentTarget::PodLogs {
            context: None,
            namespace: "default".into(),
            pod: "api".into(),
            container: None,
            previous: None,
        }),
        "logs · api"
    );
    assert_eq!(
        environment_title(&EnvironmentTarget::ProxmoxSerial { vmid: 101 }),
        "VM 101 · serial"
    );
}

#[test]
fn accepts_an_environment_on_local_and_ssh_sessions() {
    let local: LocalProfile = parse_value(json!({
        "kind": "local",
        "environment": { "kind": "container", "runtime": "docker", "container": "web" },
    }))
    .unwrap();
    assert_eq!(
        local.environment,
        Some(container(ContainerRuntime::Docker, "web", None))
    );
    let ssh: SshProfile = parse_value(json!({
        "kind": "ssh",
        "target": "build-01",
        "environment": { "kind": "pod", "namespace": "default", "pod": "api-1" },
    }))
    .unwrap();
    assert!(matches!(
        ssh.environment,
        Some(EnvironmentTarget::Pod { .. })
    ));
}

#[test]
fn rejects_targets_the_backend_could_not_run_safely() {
    let environments: [Value; 5] = [
        json!({ "kind": "container", "runtime": "docker", "container": "$(reboot)" }),
        json!({ "kind": "container", "runtime": "nerdctl", "container": "web" }),
        json!({ "kind": "pod", "namespace": "Default", "pod": "api" }),
        json!({ "kind": "proxmox-serial", "vmid": 1.5 }),
        json!({ "kind": "shell", "command": "id" }),
    ];
    for environment in environments {
        let profile = json!({ "kind": "ssh", "target": "build-01", "environment": environment });
        assert!(
            parse_value::<SessionProfile>(profile).is_err(),
            "{environment}"
        );
    }
}
