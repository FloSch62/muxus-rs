//! `tests/unit/client/environments.test.ts` and `environment-hosts.test.ts`.

use muxus_core::environments::{
    ContainerInfo, ContainerRuntime, ContainerRuntimeEnvironment, ContainerlabEnvironment,
    ContainerlabLab, ContainerlabLabels, ContainerlabNode, EnvironmentTarget, HostEnvironment,
    KubernetesEnvironment, KubernetesOwner, KubernetesPod, LibvirtDomain, LibvirtEnvironment,
    LibvirtUri, ProxmoxEnvironment, ProxmoxGuest, ProxmoxGuestType,
};
use muxus_store::prefs::state::NewSshHostStorage;
use serde_json::json;

use super::hosts::{
    HostDirectory, JumpHostOptions, enter_host_draft, jump_host_draft, local_enter_profile,
};
use super::launcher::environment_launch_items;
use super::model::*;
use super::sessions::*;
use crate::features::host_editor::logic::draft::{
    EnvironmentDiscoveryDraft, RemoteCommandMode, RouteMode, Storage,
};
use crate::hosts::managed::ManagedHost;
use crate::hosts::saved::saved_host_hop;
use crate::model::{
    LocalProfile, RequestTty, SavedHostProfile, SessionProfile, SshHostEntry, SshProfile,
};

fn saved_pve() -> SavedHostProfile {
    serde_json::from_value(json!({
        "id": "p-pve",
        "kind": "ssh",
        "name": "PVE",
        "profile": { "kind": "ssh", "target": "pve.example.test", "useConfig": false, "user": "root" },
        "metadata": { "profileId": "p-pve", "color": "#e8862f", "connectCount": 0 },
        "createdAt": "",
        "updatedAt": "",
    }))
    .expect("saved host")
}

fn config_host() -> SshHostEntry {
    let mut entry = crate::hosts::test_support::ssh_host("build-01", None);
    entry.file = "/home/u/.ssh/config".into();
    entry
}

fn tab(connected: bool, conn_id: &str, profile: SshProfile) -> LiveTab {
    LiveTab {
        profile: Some(SessionProfile::Ssh(profile)),
        connected,
        conn_id: Some(conn_id.into()),
        runs_commands: true,
    }
}

fn saved_tab_profile() -> SshProfile {
    SshProfile {
        profile_id: Some("p-pve".into()),
        ..SshProfile::new("x")
    }
}

#[test]
fn stands_for_ssh_hosts_only_keyed_like_the_sidebar() {
    let host = environment_host_for_managed(&ManagedHost::profile(saved_pve())).unwrap();
    assert_eq!(host.key, "profile:p-pve");
    assert_eq!(host.label, "PVE");
    assert_eq!(
        host.profile,
        Some(SshProfile {
            profile_id: Some("p-pve".into()),
            use_config: Some(false),
            user: Some("root".into()),
            ..SshProfile::new("pve.example.test")
        })
    );
    assert_eq!(host.color.as_deref(), Some("#e8862f"));
    assert_eq!(
        environment_host_for_managed(&ManagedHost::ssh(config_host()))
            .unwrap()
            .profile,
        Some(SshProfile::new("build-01"))
    );
    let mut serial = saved_pve();
    serial.profile = serde_json::from_value(json!({ "kind": "serial", "path": "/dev/ttyUSB0" }))
        .expect("serial profile");
    assert_eq!(
        environment_host_for_managed(&ManagedHost::profile(serial)),
        None
    );
}

#[test]
fn maps_a_tab_to_its_host_or_to_an_ad_hoc_target() {
    let hosts = [config_host()];
    assert_eq!(
        environment_host_for_profile(&crate::model::ssh_profile("build-01"), &hosts, &[])
            .unwrap()
            .key,
        "ssh:build-01"
    );
    let adhoc =
        environment_host_for_profile(&crate::model::ssh_profile("admin@10.0.0.9"), &[], &[])
            .unwrap();
    assert_eq!(adhoc.key, "target:admin@10.0.0.9");
    assert_eq!(adhoc.profile, Some(SshProfile::new("admin@10.0.0.9")));
    assert_eq!(
        environment_host_for_profile(&SessionProfile::Local(LocalProfile::default()), &[], &[]),
        None
    );
}

#[test]
fn finds_a_connected_tabs_transport_to_the_same_host() {
    let host = environment_host_for_managed(&ManagedHost::profile(saved_pve())).unwrap();
    let tabs = [
        tab(false, "c1", saved_tab_profile()),
        tab(true, "c2", SshProfile::new("pve.example.test")),
        tab(true, "c3", saved_tab_profile()),
    ];
    assert_eq!(live_connection_for(&tabs, &host).as_deref(), Some("c3"));
}

/// A system OpenSSH tab whose login is not shared reaches no commands, so neither discovery
/// nor the panel reads the host over it.
#[test]
fn skips_a_system_openssh_tab_that_cannot_run_commands() {
    assert!(tab_runs_commands(None, None));
    assert!(tab_runs_commands(Some(true), Some(true)));
    assert!(tab_runs_commands(Some(true), None));
    assert!(tab_runs_commands(Some(false), Some(false)));
    assert!(!tab_runs_commands(Some(true), Some(false)));
    let host = environment_host_for_managed(&ManagedHost::profile(saved_pve())).unwrap();
    let mut unshared = tab(true, "c1", saved_tab_profile());
    unshared.runs_commands = false;
    let mut tabs = vec![unshared];
    assert_eq!(live_connection_for(&tabs, &host), None);
    assert!(connected_hosts(&tabs, &[], &[saved_pve()]).is_empty());
    tabs.push(tab(true, "c2", saved_tab_profile()));
    assert_eq!(live_connection_for(&tabs, &host).as_deref(), Some("c2"));
    assert_eq!(connected_hosts(&tabs, &[], &[saved_pve()]).len(), 1);
}

#[test]
fn follows_a_hosts_own_discovery_choice_before_the_global_setting() {
    let mut saved = saved_pve();
    saved.metadata.discover_environments = Some(false);
    let host = environment_host_for_managed(&ManagedHost::profile(saved)).unwrap();
    assert_eq!(host.discover, Some(false));
    assert!(!discovery_enabled_for(&host, true));
    let follows = EnvironmentHost {
        discover: None,
        ..host.clone()
    };
    assert!(discovery_enabled_for(&follows, true));
    let on = EnvironmentHost {
        discover: Some(true),
        ..host
    };
    assert!(discovery_enabled_for(&on, false));
    assert!(!discovery_enabled_for(&follows, false));
}

#[test]
fn forgets_hosts_nobody_is_connected_to_except_this_computer_and_the_one_the_panel_shows() {
    let pve = environment_host_for_managed(&ManagedHost::profile(saved_pve())).unwrap();
    let build = environment_host_for_managed(&ManagedHost::ssh(config_host())).unwrap();
    let mut store = EnvironmentsStore::default();
    for host in [EnvironmentHost::local(), pve.clone(), build.clone()] {
        store.update(
            &host,
            EntryPatch {
                status: Some(EnvironmentStatus::Ready),
                environments: Some(vec![HostEnvironment::Kubernetes(KubernetesEnvironment {
                    version: None,
                    contexts: vec!["a".into()],
                    current_context: None,
                })]),
                ..EntryPatch::default()
            },
        );
    }
    let mut tabs = vec![tab(true, "c1", saved_tab_profile())];
    let forget = |store: &mut EnvironmentsStore, tabs: &[LiveTab], keep: Option<&str>| {
        for key in disconnected_keys(
            store,
            keep,
            |host| live_connection_for(tabs, host).is_some(),
            |_| false,
        ) {
            store.remove(&key);
        }
    };
    let keys = |store: &EnvironmentsStore| {
        let mut keys = store.keys();
        keys.sort();
        keys
    };
    forget(&mut store, &tabs, Some(&build.key));
    assert_eq!(keys(&store), ["local", "profile:p-pve", "ssh:build-01"]);
    forget(&mut store, &tabs, None);
    assert_eq!(keys(&store), ["local", "profile:p-pve"]);
    tabs.clear();
    forget(&mut store, &tabs, None);
    assert_eq!(keys(&store), ["local"]);
}

fn container(id: &str, name: &str, state: &str, lab: Option<&str>) -> ContainerInfo {
    ContainerInfo {
        id: id.into(),
        name: name.into(),
        image: "nginx".into(),
        state: state.into(),
        status: "Up".into(),
        ports: Vec::new(),
        compose_project: None,
        containerlab: lab.map(|lab| ContainerlabLabels {
            lab: lab.into(),
            node: "n1".into(),
            kind: "nokia_srlinux".into(),
            topology_file: None,
            owner: None,
        }),
    }
}

#[test]
fn counts_what_runs_leaving_lab_nodes_to_their_lab() {
    let environments = vec![
        HostEnvironment::Containers(ContainerRuntimeEnvironment {
            runtime: ContainerRuntime::Docker,
            version: None,
            containers: vec![
                container("1", "web", "running", None),
                container("2", "old", "exited", None),
                container("3", "clab-x-n1", "running", Some("x")),
            ],
        }),
        HostEnvironment::Containerlab(ContainerlabEnvironment {
            labs: vec![ContainerlabLab {
                name: "x".into(),
                runtime: ContainerRuntime::Docker,
                topology_file: None,
                owner: None,
                nodes: Vec::new(),
            }],
        }),
        HostEnvironment::Kubernetes(KubernetesEnvironment {
            version: None,
            contexts: vec!["a".into(), "b".into()],
            current_context: None,
        }),
        HostEnvironment::Libvirt(LibvirtEnvironment {
            version: None,
            domains: vec![LibvirtDomain {
                name: "off".into(),
                state: "shut off".into(),
                id: None,
                uri: Some(LibvirtUri::Session),
            }],
        }),
    ];
    let counts: Vec<(EnvironmentKind, usize)> = environment_badges(&environments)
        .into_iter()
        .map(|badge| (badge.kind, badge.count))
        .collect();
    assert_eq!(
        counts,
        [
            (EnvironmentKind::Docker, 1),
            (EnvironmentKind::Containerlab, 1),
            (EnvironmentKind::Kubernetes, 2),
            // A hypervisor shows even when its VMs are stopped.
            (EnvironmentKind::Libvirt, 0),
        ]
    );
    // A host with no running containers, or a hypervisor without VMs, shows nothing.
    assert_eq!(
        environment_badges(&[
            HostEnvironment::Containers(ContainerRuntimeEnvironment {
                runtime: ContainerRuntime::Podman,
                version: None,
                containers: Vec::new(),
            }),
            HostEnvironment::Libvirt(LibvirtEnvironment {
                version: None,
                domains: Vec::new(),
            }),
            HostEnvironment::Proxmox(ProxmoxEnvironment {
                version: None,
                guests: Vec::new(),
            }),
        ]),
        []
    );
}

fn remote() -> EnvironmentHost {
    environment_host_for_managed(&ManagedHost::profile(saved_pve())).unwrap()
}

#[test]
fn enters_a_target_locally_or_through_the_hosts_own_profile() {
    let target = EnvironmentTarget::Container {
        runtime: ContainerRuntime::Docker,
        container: "web".into(),
        shell: None,
    };
    assert_eq!(
        environment_profile(&EnvironmentHost::local(), target.clone()),
        SessionProfile::Local(LocalProfile {
            environment: Some(target.clone()),
            ..LocalProfile::default()
        })
    );
    let SessionProfile::Ssh(ssh) = environment_profile(&remote(), target.clone()) else {
        panic!("an SSH profile");
    };
    assert_eq!(ssh.profile_id.as_deref(), Some("p-pve"));
    assert_eq!(ssh.environment, Some(target));
}

#[test]
fn jumps_through_the_host_to_addresses_only_it_can_reach() {
    assert_eq!(
        ssh_through_host(&remote(), "10.0.0.21", Some("ubuntu")),
        SshProfile {
            proxy_jump: Some(vec![saved_host_hop("p-pve")]),
            ..SshProfile::new("ubuntu@10.0.0.21")
        }
    );
    let config = EnvironmentHost {
        key: "ssh:b".into(),
        label: "b".into(),
        profile: Some(SshProfile::new("b")),
        ..EnvironmentHost::default()
    };
    assert_eq!(
        ssh_through_host(&config, "fd00::1", None),
        SshProfile {
            proxy_jump: Some(vec!["b".into()]),
            ..SshProfile::new("[fd00::1]")
        }
    );
    assert_eq!(
        ssh_through_host(&EnvironmentHost::local(), "172.20.20.2", Some("admin")),
        SshProfile::new("admin@172.20.20.2")
    );
}

#[test]
fn opens_network_os_lab_nodes_with_ssh_and_linux_nodes_with_exec() {
    let node = |name: &str, kind: &str, ip: &str| ContainerlabNode {
        name: name.into(),
        container: format!("clab-evpn-{name}"),
        kind: kind.into(),
        image: "img".into(),
        state: "running".into(),
        ipv4: Some(ip.into()),
        ipv6: None,
    };
    let lab = ContainerlabLab {
        name: "evpn".into(),
        runtime: ContainerRuntime::Docker,
        topology_file: None,
        owner: None,
        nodes: vec![
            node("leaf1", "nokia_srlinux", "172.20.20.4"),
            node("client1", "linux", "172.20.20.7"),
        ],
    };
    let local = EnvironmentHost::local();
    assert_eq!(
        containerlab_node_entry(&local, &lab, &lab.nodes[0]).profile,
        crate::model::ssh_profile("admin@172.20.20.4")
    );
    assert_eq!(
        containerlab_node_entry(&local, &lab, &lab.nodes[1]).profile,
        SessionProfile::Local(LocalProfile {
            environment: Some(EnvironmentTarget::Container {
                runtime: ContainerRuntime::Docker,
                container: "clab-evpn-client1".into(),
                shell: None,
            }),
            ..LocalProfile::default()
        })
    );
}

#[test]
fn knows_web_ports_and_kubus_routes() {
    assert_eq!(port_scheme(8006), "https");
    assert_eq!(port_scheme(3000), "http");
    assert_eq!(
        kubus_pod_route("kind-dev", "default", "api-1"),
        "/r/core/v1/pods?sel=kind-dev%7Cdefault%7Capi-1"
    );
    assert_eq!(
        local_port_url("http", Some("0.0.0.0"), 3000),
        "http://localhost:3000/"
    );
    assert_eq!(
        local_port_url("https", Some("::1"), 8443),
        "https://[::1]:8443/"
    );
}

#[test]
fn names_the_deployment_behind_a_replica_set() {
    let pod = |owner: Option<(&str, &str)>| KubernetesPod {
        name: "p".into(),
        namespace: "n".into(),
        phase: "Running".into(),
        ready: "1/1".into(),
        restarts: 0,
        reason: None,
        owner: owner.map(|(kind, name)| KubernetesOwner {
            kind: kind.into(),
            name: name.into(),
        }),
        containers: Vec::new(),
        node: None,
        created_at: None,
    };
    assert_eq!(
        pod_workload(&pod(Some(("ReplicaSet", "api-7f9c4d")))),
        Some(("Deployment".into(), "api".into()))
    );
    assert_eq!(
        pod_workload(&pod(Some(("StatefulSet", "postgres")))),
        Some(("StatefulSet".into(), "postgres".into()))
    );
    assert_eq!(pod_workload(&pod(None)), None);
}

#[test]
fn offers_running_things_by_name_with_where_they_run() {
    let mut grafana = container("1", "grafana", "running", None);
    grafana.image = "grafana/grafana:11".into();
    grafana.compose_project = Some("monitoring".into());
    let mut stopped = container("2", "stopped", "exited", None);
    stopped.image = "x".into();
    let entry = HostEnvironmentsEntry {
        host: EnvironmentHost::local(),
        status: EnvironmentStatus::Ready,
        message: None,
        automatic: true,
        problems: Vec::new(),
        sampled_at: None,
        environments: vec![
            HostEnvironment::Containers(ContainerRuntimeEnvironment {
                runtime: ContainerRuntime::Docker,
                version: None,
                containers: vec![grafana, stopped],
            }),
            HostEnvironment::Proxmox(ProxmoxEnvironment {
                version: None,
                guests: vec![ProxmoxGuest {
                    vmid: 101,
                    name: "runner".into(),
                    guest_type: ProxmoxGuestType::Qemu,
                    status: "running".into(),
                    node: None,
                    cpu: None,
                    maxcpu: None,
                    mem: None,
                    maxmem: None,
                    uptime: None,
                    template: None,
                }],
            }),
        ],
    };
    let rows: Vec<(String, String)> = environment_launch_items(&[entry])
        .into_iter()
        .map(|item| (item.label, item.detail))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "grafana".to_string(),
                "This computer › Docker · monitoring · grafana/grafana:11".to_string()
            ),
            (
                "runner".to_string(),
                "This computer › Proxmox VM 101 · serial console".to_string()
            ),
        ]
    );
}

// --- environment-hosts.test.ts -----------------------------------------------------------------

fn hv_saved() -> SavedHostProfile {
    serde_json::from_value(json!({
        "id": "p-hv",
        "kind": "ssh",
        "name": "Hypervisor",
        "profile": { "kind": "ssh", "target": "hv.lab", "useConfig": false, "user": "root", "port": 2222, "profileId": "p-hv" },
        "metadata": { "profileId": "p-hv", "group": "Lab", "connectCount": 0 },
        "createdAt": "",
        "updatedAt": "",
    }))
    .expect("saved host")
}

fn openssh_build() -> SshHostEntry {
    let mut entry = crate::hosts::test_support::ssh_host("build-01", Some("CI"));
    entry.file = "/home/u/.ssh/config".into();
    entry.options.user = Some("ci".into());
    entry
}

fn saved_hv_host(_saved: &SavedHostProfile) -> EnvironmentHost {
    EnvironmentHost {
        key: "profile:p-hv".into(),
        label: "Hypervisor".into(),
        profile: Some(SshProfile {
            profile_id: Some("p-hv".into()),
            use_config: Some(false),
            user: Some("root".into()),
            port: Some(2222),
            ..SshProfile::new("hv.lab")
        }),
        ..EnvironmentHost::default()
    }
}

fn config_build_host() -> EnvironmentHost {
    EnvironmentHost {
        key: "ssh:build-01".into(),
        label: "build-01".into(),
        profile: Some(SshProfile::new("build-01")),
        ..EnvironmentHost::default()
    }
}

#[test]
fn copies_a_saved_host_and_runs_the_container_as_its_remote_command() {
    let saved = [hv_saved()];
    let ssh = [openssh_build()];
    let hosts = HostDirectory {
        ssh_hosts: &ssh,
        saved_profiles: &saved,
    };
    let draft = enter_host_draft(
        &saved_hv_host(&saved[0]),
        hosts,
        "web",
        &EnvironmentTarget::Container {
            runtime: ContainerRuntime::Docker,
            container: "web".into(),
            shell: None,
        },
        NewSshHostStorage::Openssh,
    )
    .unwrap();
    assert_eq!(draft.storage, Storage::Muxus);
    assert_eq!(draft.alias_text, "web");
    assert_eq!(draft.hostname, "hv.lab");
    assert_eq!(draft.user, "root");
    assert_eq!(draft.port, "2222");
    assert_eq!(draft.group, "Lab");
    assert_eq!(draft.remote_command_mode, RemoteCommandMode::Command);
    assert_eq!(draft.request_tty, Some(RequestTty::Force));
    assert!(draft.disable_sftp);
    assert_eq!(draft.discover_environments, EnvironmentDiscoveryDraft::Off);
    assert!(draft.remote_command.starts_with("docker exec -it "));
    assert!(draft.remote_command.contains(" web sh -c "));
}

#[test]
fn gives_a_copied_openssh_block_the_host_name_its_alias_stood_for() {
    let saved = [hv_saved()];
    let ssh = [openssh_build()];
    let hosts = HostDirectory {
        ssh_hosts: &ssh,
        saved_profiles: &saved,
    };
    let draft = enter_host_draft(
        &config_build_host(),
        hosts,
        "my db",
        &EnvironmentTarget::Container {
            runtime: ContainerRuntime::Podman,
            container: "db".into(),
            shell: None,
        },
        NewSshHostStorage::Openssh,
    )
    .unwrap();
    assert_eq!(draft.storage, Storage::Openssh);
    assert_eq!(draft.alias_text, "my-db");
    assert_eq!(draft.hostname, "build-01.example.test");
    assert_eq!(draft.user, "ci");
}

#[test]
fn jumps_through_the_host_to_an_address_only_it_reaches() {
    let saved = [hv_saved()];
    let ssh = [openssh_build()];
    let hosts = HostDirectory {
        ssh_hosts: &ssh,
        saved_profiles: &saved,
    };
    let lab = jump_host_draft(
        &saved_hv_host(&saved[0]),
        hosts,
        &JumpHostOptions {
            name: "lab-srl1".into(),
            address: "172.20.20.2".into(),
            user: Some("admin".into()),
            group: Some("lab".into()),
        },
        NewSshHostStorage::Openssh,
    );
    assert_eq!(lab.storage, Storage::Muxus);
    assert_eq!(lab.alias_text, "lab-srl1");
    assert_eq!(lab.hostname, "172.20.20.2");
    assert_eq!(lab.user, "admin");
    assert_eq!(lab.group, "lab");
    assert_eq!(lab.route_mode, RouteMode::Jump);
    assert_eq!(lab.proxy_jump, [saved_host_hop("p-hv")]);
    let vm = jump_host_draft(
        &config_build_host(),
        hosts,
        &JumpHostOptions {
            name: "vm 1".into(),
            address: "192.168.122.5".into(),
            ..JumpHostOptions::default()
        },
        NewSshHostStorage::Openssh,
    );
    assert_eq!(vm.storage, Storage::Openssh);
    assert_eq!(vm.alias_text, "vm-1");
    assert_eq!(vm.route_mode, RouteMode::Jump);
    assert_eq!(vm.proxy_jump, ["build-01"]);
    // This computer reaches its own guests directly.
    let own = jump_host_draft(
        &EnvironmentHost::local(),
        hosts,
        &JumpHostOptions {
            name: "vm".into(),
            address: "10.0.0.5".into(),
            ..JumpHostOptions::default()
        },
        NewSshHostStorage::Openssh,
    );
    assert_eq!(own.route_mode, RouteMode::Direct);
    assert!(own.proxy_jump.is_empty());
}

#[test]
fn keeps_a_container_on_this_computer_as_a_local_terminal_profile() {
    let profile = local_enter_profile(
        "web",
        &EnvironmentTarget::Container {
            runtime: ContainerRuntime::Docker,
            container: "web".into(),
            shell: None,
        },
    );
    assert_eq!(profile.name, "web");
    assert_eq!(profile.shell, "docker");
    for arg in ["exec", "-it", "web"] {
        assert!(profile.args.iter().any(|candidate| candidate == arg));
    }
}

#[test]
fn says_how_long_ago_a_host_was_read() {
    assert_eq!(age(None, 1_000), None);
    assert_eq!(age(Some(1_000), 30_000).as_deref(), Some("just now"));
    assert_eq!(
        age(Some(1_000), 1_000 + 5 * 60_000).as_deref(),
        Some("5 min ago")
    );
    assert_eq!(
        age(Some(1_000), 1_000 + 3 * 3_600_000).as_deref(),
        Some("3 h ago")
    );
}

// --- the topology dialog's reading of a .clab.yml ---------------------------------------------

#[test]
fn reads_the_nodes_and_links_of_a_topology() {
    use super::topology::{annotation_positions, layout, parse_topology};
    let yaml = r#"
name: evpn
topology:
  kinds:
    nokia_srlinux:
      image: ghcr.io/nokia/srlinux
  nodes:
    leaf1:
      kind: nokia_srlinux
    leaf2: { kind: nokia_srlinux }
    client1:
      kind: linux
  links:
    - endpoints: ["leaf1:e1-1", "leaf2:e1-1"]
    - endpoints:
        - leaf1:e1-2
        - client1:eth1
    - type: veth
      endpoints:
        - node: leaf2
          interface: e1-2
        - node: client1
          interface: eth2
"#;
    let topology = parse_topology(yaml);
    assert_eq!(topology.nodes, ["leaf1", "leaf2", "client1"]);
    assert_eq!(
        topology.links,
        [
            ("leaf1".to_string(), "leaf2".to_string()),
            ("leaf1".to_string(), "client1".to_string()),
            ("leaf2".to_string(), "client1".to_string()),
        ]
    );
    let saved = annotation_positions(Some(
        r#"{"nodeAnnotations":[{"id":"leaf1","position":{"x":0,"y":0}},{"id":"leaf2","position":{"x":100,"y":0}},{"id":"client1","position":{"x":50,"y":100}}]}"#,
    ));
    let placed = layout(&topology, &saved, 400., 300.);
    assert!(placed["leaf1"].0 < placed["leaf2"].0);
    assert!(placed["client1"].1 > placed["leaf1"].1);
    // Without saved positions the nodes sit on a circle inside the area.
    for (x, y) in layout(&topology, &Default::default(), 400., 300.).values() {
        assert!((0.0..=400.).contains(x) && (0.0..=300.).contains(y));
    }
}

#[test]
fn fits_two_saved_nodes_side_by_side_as_the_viewer_does() {
    use super::topology::{annotation_positions, layout, parse_topology};
    let topology = parse_topology(
        r#"topology:
  nodes:
    client1:
      kind: linux
    client2:
      kind: linux
  links:
    - endpoints: ["client1:eth1", "client2:eth1"]
"#,
    );
    let saved = annotation_positions(Some(
        r#"{"nodeAnnotations":[{"id":"client1","position":{"x":100,"y":200}},{"id":"client2","position":{"x":400,"y":180}}]}"#,
    ));
    let placed = layout(&topology, &saved, 1200., 640.);
    let (left, right) = (placed["client1"], placed["client2"]);
    // Centered with room on both sides (`fitView`, padding 0.6), not against the edges.
    assert!((left.0 + right.0 - 1200.).abs() < 0.01);
    assert!(left.0 > 200. && right.0 < 1000.);
    // One zoom for both axes: the slope between them survives.
    assert!(((right.1 - left.1) / (right.0 - left.0) + 20. / 300.).abs() < 0.001);
    // A lone node sits in the middle.
    let lone = parse_topology("topology:\n  nodes:\n    a:\n      kind: linux\n");
    let at = layout(
        &lone,
        &annotation_positions(Some(
            r#"{"nodeAnnotations":[{"id":"a","position":{"x":5,"y":5}}]}"#,
        )),
        400.,
        300.,
    )["a"];
    assert_eq!(at, (200., 150.));
}

#[test]
fn sizes_a_logo_to_its_square() {
    let sized = super::logos::sized_svg(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="360" height="360" viewBox="0 0 95 95"><path/></svg>"#,
        12.,
    );
    assert_eq!(
        sized,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 95 95" width="12" height="12"><path/></svg>"#
    );
}

#[test]
fn offers_the_refresh_intervals_of_settings() {
    let options: Vec<String> = super::refresh_options()
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        options,
        [
            "Only when connecting",
            "Every 15 seconds",
            "Every 30 seconds",
            "Every minute",
            "Every 2 minutes",
            "Every 5 minutes",
        ]
    );
}
