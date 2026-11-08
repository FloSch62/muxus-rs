//! `tests/unit/server/environments.test.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use muxus_core::environments::{
    ComposeAction, ContainerAction, ContainerPort, ContainerRuntime, ContainerRuntimeEnvironment,
    ContainerlabLabels, ContainerlabNode, EnvironmentAction, EnvironmentProblem,
    EnvironmentProvider, HostEnvironment, KubernetesEnvironment, KubernetesOwner, LibvirtAction,
    LibvirtDomain, LibvirtEnvironment, LibvirtUri, ProxmoxAction, ProxmoxGuest, ProxmoxGuestType,
    VncEndpoint,
};
use serde_json::json;

use super::discovery::{action_argv, discover_environments};
use super::parse::{
    NodeAddress, containerlab_labs, parse_docker_ports, parse_docker_ps, parse_inspect_addresses,
    parse_kube_pods, parse_podman_ps, parse_pve_resources, parse_qemu_agent_addresses,
    parse_virsh_addresses, parse_virsh_display, parse_virsh_list,
};
use super::probe::{
    Probe, ProbeResult, ProbeResults, ProbeRunner, parse_probe_output, probe_failure, probe_script,
};
use super::{kubus_url, probe};

fn block_on<T>(future: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

#[cfg(unix)]
#[test]
fn runs_each_probe_on_its_own_and_keeps_stderr_out_of_stdout() {
    let run = probe::local_probe_runner(None);
    let results = block_on(run(vec![
        Probe::new("ok", &["printf", "%s\n", "hello world"]),
        Probe::new("missing", &["muxus-no-such-tool-anywhere"]),
        Probe::new(
            "fails",
            &[
                "sh",
                "-c",
                "echo out; echo \"permission denied\" >&2; exit 3",
            ],
        ),
        Probe::new("quoted", &["printf", "%s", "it's $(not run)"]),
    ]))
    .expect("ran");
    assert_eq!(results["ok"], ProbeResult::ran("hello world\n", 0, ""));
    assert_eq!(results["missing"], ProbeResult::Missing);
    assert_eq!(
        results["fails"],
        ProbeResult::ran("out\n", 3, "permission denied")
    );
    assert!(
        matches!(&results["quoted"], ProbeResult::Ran { stdout, .. } if stdout == "it's $(not run)")
    );
    assert_eq!(
        probe_failure(results.get("fails")).as_deref(),
        Some("permission denied")
    );
}

/// A system OpenSSH session runs the batch as one script over its shared login.
#[cfg(unix)]
#[test]
fn runs_a_batch_as_one_script_through_the_script_runner() {
    let scripts = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = scripts.clone();
    let run = probe::script_probe_runner(move |script: String| {
        seen.lock().unwrap().push(script.clone());
        async move {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .output()
                .map_err(|error| error.to_string())?;
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
    });
    let results = block_on(run(vec![
        Probe::new("ok", &["printf", "%s", "hello"]),
        Probe::new("missing", &["muxus-no-such-tool-anywhere"]),
    ]))
    .expect("ran");
    assert_eq!(results["ok"], ProbeResult::ran("hello", 0, ""));
    assert_eq!(results["missing"], ProbeResult::Missing);
    assert_eq!(scripts.lock().unwrap().len(), 1, "one script for the batch");
    // Nothing to ask, nothing run; a script that fails reports why.
    assert!(block_on(run(Vec::new())).expect("empty").is_empty());
    assert_eq!(scripts.lock().unwrap().len(), 1);
    let refused = probe::script_probe_runner(|_script: String| async {
        Err::<String, String>("ssh could not run the command.".into())
    });
    assert_eq!(
        block_on(refused(vec![Probe::new("ok", &["true"])])).unwrap_err(),
        "ssh could not run the command."
    );
}

#[test]
fn ignores_a_login_banner_and_reports_a_cut_off_probe_as_timed_out() {
    let output = [
        "Welcome! Authorized use only.",
        "",
        "@@muxus-env:a",
        "{\"x\":1}",
        "@@muxus-env-exit:0",
        "",
        "@@muxus-env:b",
        "partial output",
    ]
    .join("\n");
    let results = parse_probe_output(&output);
    assert_eq!(results["a"], ProbeResult::ran("{\"x\":1}", 0, ""));
    assert!(matches!(
        results["b"],
        ProbeResult::Ran { exit_code: 124, .. }
    ));
    assert_eq!(
        probe_failure(results.get("b")).as_deref(),
        Some("did not answer in time")
    );
}

#[test]
fn reports_the_sentence_a_tool_ends_with_not_its_log_lines() {
    assert_eq!(
        probe_failure(Some(&ProbeResult::ran(
            "",
            1,
            &[
                "E1101 11:32:54.787058  613673 memcache.go:265] \"Unhandled Error\" err=\"couldn't get current server API group list\"",
                "The connection to the server 127.0.0.1:34121 was refused - did you specify the right host or port?",
            ]
            .join("\n"),
        )))
        .as_deref(),
        Some(
            "The connection to the server 127.0.0.1:34121 was refused - did you specify the right host or port?"
        )
    );
    assert_eq!(
        probe_failure(Some(&ProbeResult::ran(
            "",
            1,
            "setlocale: No such file or directory\nerror: failed to connect to the hypervisor\nerror: authentication unavailable: no polkit agent available",
        )))
        .as_deref(),
        Some("authentication unavailable: no polkit agent available")
    );
}

#[test]
fn quotes_every_argument() {
    assert!(
        probe_script(&[Probe::new("p", &["docker", "ps", "--format", "{{json .}}"])])
            .contains("muxus_probe p docker ps --format '{{json .}}'")
    );
}

fn port(container_port: u16, protocol: &str, host: Option<(u16, &str)>) -> ContainerPort {
    ContainerPort {
        host_ip: host.map(|(_, ip)| ip.to_string()),
        host_port: host.map(|(port, _)| port),
        container_port,
        protocol: protocol.into(),
    }
}

#[test]
fn reads_published_ports_keeping_one_entry_per_port() {
    assert_eq!(
        parse_docker_ports("0.0.0.0:18080->80/tcp, :::18080->80/tcp, 443/tcp, [::]:53->53/udp"),
        vec![
            port(80, "tcp", Some((18080, "0.0.0.0"))),
            port(443, "tcp", None),
            port(53, "udp", Some((53, "::"))),
        ]
    );
    assert_eq!(
        parse_docker_ports("127.0.0.1:8000-8001->9000-9001/tcp"),
        vec![
            port(9000, "tcp", Some((8000, "127.0.0.1"))),
            port(9001, "tcp", Some((8001, "127.0.0.1"))),
        ]
    );
}

fn docker_line(fields: serde_json::Value) -> String {
    let mut base = json!({
        "id": "abcdef0123456789",
        "image": "nginx:1.27",
        "state": "running",
        "status": "Up 3 days",
        "ports": "",
        "project": "",
        "clab": "",
        "clabNode": "",
        "clabKind": "",
        "clabTopo": "",
        "clabOwner": "",
        "clabInternal": "",
    });
    for (key, value) in fields.as_object().expect("fields") {
        base[key] = value.clone();
    }
    base.to_string()
}

#[test]
fn reads_docker_ps_lines_with_compose_and_containerlab_labels() {
    let containers = parse_docker_ps(
        &[
            "WARNING: something".to_string(),
            docker_line(json!({ "name": "web", "project": "shop", "ports": "0.0.0.0:8080->80/tcp" })),
            docker_line(json!({ "name": "clab-evpn-leaf1", "clab": "evpn", "clabNode": "leaf1", "clabKind": "nokia_srlinux", "clabTopo": "/labs/evpn.clab.yml" })),
            docker_line(json!({ "name": "clab-evpn-sshx", "clab": "evpn", "clabNode": "sshx", "clabInternal": "true" })),
        ]
        .join("\n"),
    );
    let names: Vec<&str> = containers.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["clab-evpn-leaf1", "clab-evpn-sshx", "web"]);
    assert_eq!(containers[2].id, "abcdef012345");
    assert_eq!(containers[2].compose_project.as_deref(), Some("shop"));
    assert_eq!(containers[2].ports[0].host_port, Some(8080));
    assert_eq!(
        containers[0].containerlab,
        Some(ContainerlabLabels {
            lab: "evpn".into(),
            node: "leaf1".into(),
            kind: "nokia_srlinux".into(),
            topology_file: Some("/labs/evpn.clab.yml".into()),
            owner: None,
        })
    );
    // Containerlab's own helpers are not nodes.
    assert_eq!(containers[1].containerlab, None);
}

#[test]
fn reads_podman_ps_json() {
    let containers = parse_podman_ps(
        &json!([{
            "Id": "0123456789abcdef",
            "Names": ["db"],
            "Image": "docker.io/library/postgres:16",
            "State": "running",
            "Status": "Up 2 hours",
            "Ports": [{ "host_ip": "", "container_port": 5432, "host_port": 15432, "range": 1, "protocol": "tcp" }],
            "Labels": { "io.podman.compose.project": "shop" },
        }])
        .to_string(),
    );
    assert_eq!(containers.len(), 1);
    let db = &containers[0];
    assert_eq!(db.id, "0123456789ab");
    assert_eq!(db.name, "db");
    assert_eq!(db.image, "docker.io/library/postgres:16");
    assert_eq!(db.state, "running");
    assert_eq!(db.status, "Up 2 hours");
    assert_eq!(db.ports, vec![port(5432, "tcp", Some((15432, "0.0.0.0")))]);
    assert_eq!(db.compose_project.as_deref(), Some("shop"));
    assert_eq!(db.containerlab, None);
}

#[test]
fn builds_labs_with_management_addresses_from_inspect() {
    let containers = parse_docker_ps(
        &json!({
            "id": "a1",
            "name": "clab-evpn-spine1",
            "image": "ghcr.io/nokia/srlinux",
            "state": "running",
            "status": "Up",
            "ports": "",
            "clab": "evpn",
            "clabNode": "spine1",
            "clabKind": "nokia_srlinux",
            "clabTopo": "/labs/evpn.clab.yml",
        })
        .to_string(),
    );
    let addresses = parse_inspect_addresses(
        &json!({
            "name": "/clab-evpn-spine1",
            "mode": "clab",
            "nets": { "clab": { "IPAddress": "172.20.20.2", "GlobalIPv6Address": "3fff:172:20:20::2" } },
        })
        .to_string(),
    );
    assert_eq!(
        addresses.get("clab-evpn-spine1"),
        Some(&NodeAddress {
            ipv4: Some("172.20.20.2".into()),
            ipv6: Some("3fff:172:20:20::2".into()),
        })
    );
    let labs = containerlab_labs(ContainerRuntime::Docker, &containers, &addresses);
    assert_eq!(labs.len(), 1);
    assert_eq!(labs[0].name, "evpn");
    assert_eq!(labs[0].runtime, ContainerRuntime::Docker);
    assert_eq!(
        labs[0].topology_file.as_deref(),
        Some("/labs/evpn.clab.yml")
    );
    assert_eq!(
        labs[0].nodes,
        vec![ContainerlabNode {
            name: "spine1".into(),
            container: "clab-evpn-spine1".into(),
            kind: "nokia_srlinux".into(),
            image: "ghcr.io/nokia/srlinux".into(),
            state: "running".into(),
            ipv4: Some("172.20.20.2".into()),
            ipv6: Some("3fff:172:20:20::2".into()),
        }]
    );
}

#[test]
fn summarizes_pods_with_the_reason_one_is_not_running() {
    let pods = parse_kube_pods(
        &json!({
            "items": [
                {
                    "metadata": {
                        "name": "worker-5d4b8-zq1rt",
                        "namespace": "default",
                        "ownerReferences": [{ "kind": "ReplicaSet", "name": "worker-5d4b8" }],
                    },
                    "spec": { "containers": [{ "name": "worker" }], "nodeName": "kind-control-plane" },
                    "status": {
                        "phase": "Running",
                        "containerStatuses": [
                            { "ready": false, "restartCount": 7, "state": { "waiting": { "reason": "CrashLoopBackOff" } } },
                        ],
                    },
                },
                {
                    "metadata": { "name": "job-1", "namespace": "default" },
                    "spec": { "containers": [{ "name": "job" }] },
                    "status": {
                        "phase": "Succeeded",
                        "containerStatuses": [{ "ready": false, "restartCount": 0, "state": { "terminated": { "reason": "Completed" } } }],
                    },
                },
            ],
        })
        .to_string(),
    );
    assert_eq!(pods[0].name, "job-1");
    assert_eq!(pods[0].phase, "Succeeded");
    assert_eq!(pods[0].ready, "0/1");
    assert_eq!(pods[0].restarts, 0);
    assert_eq!(pods[0].containers, ["job"]);
    assert_eq!(pods[0].reason, None);
    assert_eq!(pods[0].owner, None);
    assert_eq!(pods[1].name, "worker-5d4b8-zq1rt");
    assert_eq!(pods[1].ready, "0/1");
    assert_eq!(pods[1].restarts, 7);
    assert_eq!(pods[1].reason.as_deref(), Some("CrashLoopBackOff"));
    assert_eq!(
        pods[1].owner,
        Some(KubernetesOwner {
            kind: "ReplicaSet".into(),
            name: "worker-5d4b8".into(),
        })
    );
    assert_eq!(pods[1].node.as_deref(), Some("kind-control-plane"));
}

#[test]
fn reads_cluster_resources() {
    assert_eq!(
        parse_pve_resources(
            &json!([
                { "vmid": 201, "type": "lxc", "name": "pihole", "status": "running", "node": "pve", "cpu": 0.03, "maxcpu": 1 },
                { "vmid": 101, "type": "qemu", "name": "runner", "status": "stopped", "template": 1 },
                { "type": "storage", "id": "local" },
            ])
            .to_string()
        ),
        vec![
            ProxmoxGuest {
                vmid: 101,
                name: "runner".into(),
                guest_type: ProxmoxGuestType::Qemu,
                status: "stopped".into(),
                node: None,
                cpu: None,
                maxcpu: None,
                mem: None,
                maxmem: None,
                uptime: None,
                template: Some(true),
            },
            ProxmoxGuest {
                vmid: 201,
                name: "pihole".into(),
                guest_type: ProxmoxGuestType::Lxc,
                status: "running".into(),
                node: Some("pve".into()),
                cpu: Some(0.03),
                maxcpu: Some(1.),
                mem: None,
                maxmem: None,
                uptime: None,
                template: None,
            },
        ]
    );
}

fn domain(name: &str, state: &str, id: Option<i64>, uri: Option<LibvirtUri>) -> LibvirtDomain {
    LibvirtDomain {
        name: name.into(),
        state: state.into(),
        id,
        uri,
    }
}

#[test]
fn reads_virsh_tables() {
    assert_eq!(
        parse_virsh_list(
            &[
                " Id   Name            State",
                "--------------------------------",
                " 1    runner          running",
                " -    win 11          shut off",
                "",
            ]
            .join("\n")
        ),
        vec![
            domain("runner", "running", Some(1), None),
            domain("win 11", "shut off", None, None),
        ]
    );
    assert_eq!(
        parse_virsh_addresses(
            &[
                " Name       MAC address          Protocol     Address",
                "-------------------------------------------------------",
                " vnet0      52:54:00:aa:bb:cc    ipv6         fe80::1/64",
                " vnet0      52:54:00:aa:bb:cc    ipv4         192.168.122.10/24",
            ]
            .join("\n")
        ),
        ["192.168.122.10"]
    );
    let endpoint = |host: &str, port: u16| VncEndpoint {
        host: host.into(),
        port,
    };
    assert_eq!(
        parse_virsh_display("vnc://127.0.0.1:0\n"),
        Some(endpoint("127.0.0.1", 5900))
    );
    assert_eq!(
        parse_virsh_display("vnc://:3"),
        Some(endpoint("127.0.0.1", 5903))
    );
    assert_eq!(parse_virsh_display("spice://127.0.0.1:5930"), None);
}

#[test]
fn reads_guest_agent_addresses_without_loopback_or_link_local_ones() {
    assert_eq!(
        parse_qemu_agent_addresses(
            &json!([
                { "name": "lo", "ip-addresses": [{ "ip-address": "127.0.0.1" }] },
                {
                    "name": "eth0",
                    "ip-addresses": [{ "ip-address": "fe80::5054:ff:fe00:1" }, { "ip-address": "10.0.0.21" }, { "ip-address": "2001:db8::21" }],
                },
            ])
            .to_string()
        ),
        ["10.0.0.21", "2001:db8::21"]
    );
}

/// Answers each probe from `outputs` (missing otherwise) and records what was asked.
fn fake_runner(
    outputs: HashMap<&'static str, ProbeResult>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
) -> ProbeRunner {
    let outputs = Arc::new(outputs);
    Arc::new(move |probes: Vec<Probe>| {
        let outputs = outputs.clone();
        let calls = calls.clone();
        Box::pin(async move {
            let mut results = ProbeResults::new();
            for probe in probes {
                calls.lock().expect("calls").push(probe.argv.clone());
                let result = outputs
                    .get(probe.id.as_str())
                    .cloned()
                    .unwrap_or(ProbeResult::Missing);
                results.insert(probe.id, result);
            }
            Ok(results)
        })
    })
}

fn ran(stdout: &str) -> ProbeResult {
    ProbeResult::ran(stdout, 0, "")
}

fn discover(outputs: HashMap<&'static str, ProbeResult>) -> super::discovery::DiscoveryResult {
    let runner = fake_runner(outputs, Arc::default());
    block_on(discover_environments(&runner)).expect("discovered")
}

#[test]
fn reports_only_the_tools_a_host_has_and_why_one_would_not_answer() {
    let result = discover(HashMap::from([
        ("docker-cli", ran("Docker version 28.0.1, build 068a01e\n")),
        (
            "docker-ps",
            ProbeResult::ran(
                "",
                1,
                "permission denied while trying to connect to the Docker daemon socket",
            ),
        ),
        (
            "kubectl-version",
            ran(r#"{"clientVersion":{"gitVersion":"v1.31.0"}}"#),
        ),
        ("kubectl-contexts", ran("kind-dev\nprod-eu\n")),
        ("kubectl-current", ran("kind-dev\n")),
        ("virsh-version", ran("10.0.0\n")),
        (
            "virsh-system",
            ran(" Id   Name     State\n------\n 1    runner   running\n"),
        ),
        ("virsh-session", ran(" Id   Name   State\n------\n")),
    ]));
    assert_eq!(
        result.problems,
        vec![EnvironmentProblem {
            provider: EnvironmentProvider::Docker,
            message: "permission denied while trying to connect to the Docker daemon socket".into(),
        }]
    );
    assert_eq!(
        result.environments,
        vec![
            HostEnvironment::Kubernetes(KubernetesEnvironment {
                version: Some("v1.31.0".into()),
                contexts: vec!["kind-dev".into(), "prod-eu".into()],
                current_context: Some("kind-dev".into()),
            }),
            HostEnvironment::Libvirt(LibvirtEnvironment {
                version: Some("10.0.0".into()),
                domains: vec![domain(
                    "runner",
                    "running",
                    Some(1),
                    Some(LibvirtUri::System)
                )],
            }),
        ]
    );
}

#[test]
fn lists_libvirt_once_with_the_session_vms_after_the_system_ones() {
    let list = |rows: &str| ran(&format!(" Id   Name   State\n------\n{rows}"));
    let libvirt = |system: &str, session: &str| -> Vec<HostEnvironment> {
        discover(HashMap::from([
            ("virsh-version", ran("10.0.0\n")),
            ("virsh-system", list(system)),
            ("virsh-session", list(session)),
        ]))
        .environments
        .into_iter()
        .filter(|environment| matches!(environment, HostEnvironment::Libvirt(_)))
        .collect()
    };
    assert_eq!(
        libvirt(" 1    runner   running\n", " -    desk     shut off\n"),
        vec![HostEnvironment::Libvirt(LibvirtEnvironment {
            version: Some("10.0.0".into()),
            domains: vec![
                domain("runner", "running", Some(1), Some(LibvirtUri::System)),
                domain("desk", "shut off", None, Some(LibvirtUri::Session)),
            ],
        })]
    );
    // A session URI answered by the system daemon lists the same VMs: shown once.
    assert_eq!(
        libvirt(" 1    runner   running\n", " 1    runner   running\n"),
        vec![HostEnvironment::Libvirt(LibvirtEnvironment {
            version: Some("10.0.0".into()),
            domains: vec![domain(
                "runner",
                "running",
                Some(1),
                Some(LibvirtUri::System)
            )],
        })]
    );
}

#[test]
fn lists_podman_docker_once_under_podman() {
    let result = discover(HashMap::from([
        (
            "docker-cli",
            ProbeResult::ran("", 0, "Emulate Docker CLI using podman."),
        ),
        (
            "docker-ps",
            ran(r#"{"id":"x","name":"twice","state":"running"}"#),
        ),
        ("podman-cli", ran("podman version 5.2.0\n")),
        ("podman-ps", ran("[]")),
    ]));
    assert_eq!(
        result.environments,
        vec![HostEnvironment::Containers(ContainerRuntimeEnvironment {
            runtime: ContainerRuntime::Podman,
            version: Some("5.2.0".into()),
            containers: Vec::new(),
        })]
    );
}

#[test]
fn asks_for_lab_addresses_only_when_there_are_lab_nodes() {
    let calls: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let runner = fake_runner(
        HashMap::from([(
            "docker-ps",
            ran(&json!({ "id": "a", "name": "clab-x-n1", "state": "running", "clab": "x", "clabNode": "n1", "clabKind": "linux" }).to_string()),
        )]),
        calls.clone(),
    );
    block_on(discover_environments(&runner)).expect("discovered");
    let last = calls
        .lock()
        .expect("calls")
        .last()
        .cloned()
        .expect("a call");
    assert_eq!(&last[..2], ["docker", "inspect"]);
    assert_eq!(last.last().map(String::as_str), Some("clab-x-n1"));
}

#[test]
fn maps_actions_to_the_tools_that_perform_them() {
    let argv = |action: EnvironmentAction| action_argv(&action).expect("valid");
    assert_eq!(
        argv(EnvironmentAction::Container {
            runtime: ContainerRuntime::Docker,
            container: "web".into(),
            action: ContainerAction::Restart,
        }),
        ["docker", "restart", "web"]
    );
    assert_eq!(
        argv(EnvironmentAction::Container {
            runtime: ContainerRuntime::Podman,
            container: "old-db".into(),
            action: ContainerAction::Remove,
        }),
        ["podman", "rm", "--force", "old-db"]
    );
    assert_eq!(
        argv(EnvironmentAction::Compose {
            runtime: ContainerRuntime::Podman,
            project: "shop".into(),
            action: ComposeAction::Stop,
        }),
        ["podman", "compose", "--project-name", "shop", "stop"]
    );
    assert_eq!(
        argv(EnvironmentAction::Proxmox {
            guest_type: ProxmoxGuestType::Lxc,
            vmid: 201,
            action: ProxmoxAction::Shutdown,
        }),
        ["pct", "shutdown", "201"]
    );
    assert_eq!(
        argv(EnvironmentAction::Libvirt {
            uri: None,
            domain: "runner".into(),
            action: LibvirtAction::Destroy,
        }),
        ["virsh", "destroy", "runner"]
    );
    assert!(
        action_argv(&EnvironmentAction::Compose {
            runtime: ContainerRuntime::Docker,
            project: "-p".into(),
            action: ComposeAction::Stop,
        })
        .is_err()
    );
}

#[test]
fn passes_only_in_app_kubus_routes() {
    assert_eq!(
        kubus_url("/r/core/v1/pods?sel=kind-dev%7Cdefault%7Capi-1").as_deref(),
        Some("kubus://r/core/v1/pods?sel=kind-dev%7Cdefault%7Capi-1")
    );
    assert_eq!(kubus_url("/").as_deref(), Some("kubus://"));
    assert_eq!(kubus_url("//evil.example"), None);
    assert_eq!(kubus_url("https://evil.example"), None);
    assert_eq!(kubus_url("/a b"), None);
}
