//! The demo catalog from `hack/demo-env.mjs` and `hack/capture.mjs`, so
//! screenshots of this app can be compared with the documentation ones.
//!
//! It reproduces what the Muxus sidebar shows once `demo-env.mjs` has seeded
//! its fake home: the ssh_config hosts with their folder metadata, the
//! "showcase" host that is never dialled, the saved Telnet, serial, RDP and VNC
//! hosts, and the folder colours and icons `capture.mjs` puts in the prefs.
//!
//! `lab-leaf-01` and `lab-spine-01` are left out on purpose. The demo writes
//! them to `~/.ssh/config.d/lab.conf`, but the `Include` that should pull that
//! file in sits inside the root config's `Host *` block. The Muxus config
//! parser (`server/src/ssh/ssh-config.ts`) only follows a top-level `Include`,
//! so the file is never read and its hosts never reach the sidebar.

use crate::model::palette::demo;
use indexmap::IndexMap;

use crate::model::{
    Catalog, ConfigForward, FolderStyle, ForwardKind, HostBlockOptions, HostMetadata, RdpProfile,
    ResolvedHostSettings, SavedHostProfile, SavedHostSessionProfile, SerialProfile, SshConfig,
    SshGateway, SshHostEntry, TelnetProfile, VncProfile,
};

/// The fake home every demo path lives under.
pub const DEMO_HOME: &str = "/home/demo";

/// `~/.ssh/config` of the demo home.
pub fn demo_config_path() -> String {
    format!("{DEMO_HOME}/.ssh/config")
}

/// One of `DEMO_HOSTS` in demo-env.mjs.
struct DemoHost {
    alias: &'static str,
    hostname: &'static str,
    user: &'static str,
    folder: &'static str,
    color: Option<&'static str>,
    jump: Option<&'static str>,
    /// Keyboard-interactive hosts get no IdentityFile line.
    keyboard_interactive: bool,
}

/// `DEMO_HOSTS` minus the `lab-*` hosts (see the module docs), in config order.
const DEMO_SSH_HOSTS: [DemoHost; 6] = [
    DemoHost {
        alias: "web-01",
        hostname: "web-01.prod.internal",
        user: "deploy",
        folder: "Production/Web",
        color: Some(demo::BLUE),
        jump: None,
        keyboard_interactive: false,
    },
    DemoHost {
        alias: "web-02",
        hostname: "web-02.prod.internal",
        user: "deploy",
        folder: "Production/Web",
        color: Some(demo::BLUE),
        jump: None,
        keyboard_interactive: false,
    },
    DemoHost {
        alias: "cache-01",
        hostname: "cache-01.prod.internal",
        user: "deploy",
        folder: "Production",
        color: Some(demo::BLUE),
        jump: None,
        keyboard_interactive: false,
    },
    DemoHost {
        alias: "bastion",
        hostname: "bastion.example.net",
        user: "jump",
        folder: "Production",
        color: Some(demo::RED),
        jump: None,
        keyboard_interactive: true,
    },
    DemoHost {
        alias: "db-01",
        hostname: "db-01.prod.internal",
        user: "postgres",
        folder: "Production/Data",
        color: None,
        jump: Some("bastion"),
        keyboard_interactive: false,
    },
    DemoHost {
        alias: "build-01",
        hostname: "build-01.lab.internal",
        user: "ci",
        folder: "Lab",
        color: Some(demo::GREEN),
        jump: None,
        keyboard_interactive: false,
    },
];

fn local_forward(bind_port: u16, target_host: &str, target_port: u16) -> ConfigForward {
    ConfigForward {
        kind: ForwardKind::Local,
        bind_port,
        target_host: Some(target_host.into()),
        target_port: Some(target_port),
    }
}

fn metadata(profile_id: &str, group: &str, color: Option<&str>) -> HostMetadata {
    HostMetadata {
        profile_id: profile_id.into(),
        group: Some(group.into()),
        color: color.map(Into::into),
        ..Default::default()
    }
}

/// The ssh_config hosts, in the order `sshConfig()` writes them.
fn demo_ssh_hosts() -> Vec<SshHostEntry> {
    let config = demo_config_path();
    let default_key = format!("{DEMO_HOME}/.ssh/id_ed25519");
    let mut hosts: Vec<SshHostEntry> = DEMO_SSH_HOSTS
        .iter()
        .map(|host| {
            let forwards = if host.alias == "web-01" {
                vec![local_forward(8080, "127.0.0.1", 8080)]
            } else {
                Vec::new()
            };
            let (written_key, resolved_key) = if host.keyboard_interactive {
                (Vec::new(), Vec::new())
            } else {
                (
                    vec!["~/.ssh/id_ed25519".to_string()],
                    vec![default_key.clone()],
                )
            };
            SshHostEntry {
                alias: host.alias.into(),
                aliases: vec![host.alias.into()],
                description: None,
                file: config.clone(),
                options: HostBlockOptions {
                    hostname: Some(host.hostname.into()),
                    user: Some(host.user.into()),
                    identity_files: (!written_key.is_empty()).then_some(written_key),
                    proxy_jump: host.jump.map(|jump| vec![jump.to_string()]),
                    forwards: (!forwards.is_empty()).then(|| forwards.clone()),
                    ..Default::default()
                },
                resolved: ResolvedHostSettings {
                    hostname: host.hostname.into(),
                    user: Some(host.user.into()),
                    port: 22,
                    identity_files: resolved_key,
                    proxy_jump: host
                        .jump
                        .map(|jump| vec![jump.to_string()])
                        .unwrap_or_default(),
                    forwards,
                    ..Default::default()
                },
                metadata: Some(metadata(
                    &format!("demo-ssh-{}", host.alias),
                    host.folder,
                    host.color,
                )),
            }
        })
        .collect();

    // SHOWCASE_HOSTS: only ever on screen, never dialled.
    hosts.push(SshHostEntry {
        alias: "db-primary".into(),
        aliases: vec!["db-primary".into()],
        description: None,
        file: config,
        options: HostBlockOptions {
            hostname: Some("db-primary.prod.internal".into()),
            user: Some("postgres".into()),
            port: Some(22),
            identity_files: Some(vec!["~/.ssh/id_prod_ed25519".into()]),
            proxy_jump: Some(vec!["bastion".into()]),
            forwards: Some(vec![local_forward(15432, "127.0.0.1", 5432)]),
            forward_agent: Some(true),
            ..Default::default()
        },
        resolved: ResolvedHostSettings {
            hostname: "db-primary.prod.internal".into(),
            user: Some("postgres".into()),
            port: 22,
            identity_files: vec![format!("{DEMO_HOME}/.ssh/id_prod_ed25519")],
            forward_agent: true,
            proxy_jump: vec!["bastion".into()],
            forwards: vec![local_forward(15432, "127.0.0.1", 5432)],
            ..Default::default()
        },
        metadata: Some(HostMetadata {
            display_name: Some("db-primary (eu-central)".into()),
            ..metadata("demo-ssh-db-primary", "Production/Data", None)
        }),
    });
    hosts
}

fn saved(
    id: &str,
    name: &str,
    profile: SavedHostSessionProfile,
    group: &str,
    color: Option<&str>,
) -> SavedHostProfile {
    SavedHostProfile {
        id: id.into(),
        kind: profile.kind(),
        name: name.into(),
        profile,
        metadata: metadata(id, group, color),
        created_at: "2026-07-25T09:00:00.000Z".into(),
        updated_at: "2026-07-25T09:00:00.000Z".into(),
    }
}

/// The saved Telnet, serial, RDP and VNC hosts `seed()` creates, with the
/// folder and colour it patches onto each.
fn demo_saved_hosts() -> Vec<SavedHostProfile> {
    vec![
        saved(
            "demo-core-switch",
            "core-switch (console)",
            SavedHostSessionProfile::Telnet(TelnetProfile {
                profile_id: Some("demo-core-switch".into()),
                host: "192.0.2.20".into(),
                port: 23,
            }),
            "Lab/Fabric",
            Some(demo::YELLOW),
        ),
        saved(
            "demo-lab-console",
            "lab-console",
            SavedHostSessionProfile::Serial(SerialProfile {
                profile_id: Some("demo-lab-console".into()),
                ..SerialProfile::new("/dev/ttyUSB0")
            }),
            "Lab",
            None,
        ),
        saved(
            "demo-win-build",
            "win-build",
            SavedHostSessionProfile::Rdp(RdpProfile {
                profile_id: Some("demo-win-build".into()),
                host: "win-build.corp.internal".into(),
                port: 3389,
                username: Some("CORP\\ci".into()),
                ssh_gateway: Some(SshGateway {
                    target: "bastion".into(),
                    profile_id: None,
                }),
                domain: None,
                share_clipboard: None,
            }),
            "Lab",
            Some(demo::BLUE),
        ),
        saved(
            "demo-design-vm",
            "design-vm",
            SavedHostSessionProfile::Vnc(VncProfile {
                profile_id: Some("demo-design-vm".into()),
                host: "design-vm.lab.internal".into(),
                port: 5900,
                username: None,
                ssh_gateway: None,
                resize_remote: None,
                view_only: None,
                share_clipboard: None,
            }),
            "Lab",
            None,
        ),
    ]
}

/// Everything the demo sidebar lists.
pub fn demo_catalog() -> Catalog {
    let path = demo_config_path();
    Catalog {
        ssh: SshConfig {
            // Only the root file is read (see the module docs).
            files: vec![path.clone()],
            path,
            hosts: demo_ssh_hosts(),
            error: None,
        },
        profiles: demo_saved_hosts(),
    }
}

/// `sidebarFolderStyles` from `BASE_PREFS` in capture.mjs.
pub fn demo_folder_styles() -> IndexMap<String, FolderStyle> {
    [
        ("folder:production", demo::RED, "cloud"),
        ("folder:production/web", demo::BLUE, "server"),
        ("folder:production/data", demo::ORANGE, "storage"),
        ("folder:lab", demo::GREEN, "lab"),
        ("folder:lab/fabric", demo::PURPLE, "lan"),
    ]
    .into_iter()
    .map(|(key, color, icon)| {
        (
            key.to_string(),
            FolderStyle {
                color: Some(color.into()),
                icon: Some(icon.into()),
            },
        )
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::tree::{BuildOptions, build_host_tree, flatten_visible_tree};
    use crate::hosts::{RowNode, group_managed_hosts};

    /// The sidebar the documentation screenshots show, row by row (the current
    /// demo also seeds `win-build` and `design-vm`, which postdate the July shots).
    #[test]
    fn lays_out_the_documented_sidebar() {
        let catalog = demo_catalog();
        let groups = group_managed_hosts(
            &catalog.ssh.hosts,
            &catalog.profiles,
            &catalog.ssh.files,
            Some(&catalog.ssh.path),
            "",
        );
        let tree = build_host_tree(&groups, &BuildOptions::default());
        let rows = flatten_visible_tree(&tree, &|_| true, &|_| None);
        let lines: Vec<String> = rows
            .iter()
            .map(|row| {
                let count = match &row.node {
                    RowNode::Folder {
                        descendant_host_count,
                        ..
                    } => format!(" ({descendant_host_count})"),
                    _ => String::new(),
                };
                format!("{}{}{count}", "  ".repeat(row.depth), row.node.label())
            })
            .collect();
        assert_eq!(
            lines,
            [
                "Lab (5)",
                "  Fabric (1)",
                "    core-switch (console)",
                "  build-01",
                "  design-vm",
                "  lab-console",
                "  win-build",
                "Production (6)",
                "  Data (2)",
                "    db-01",
                "    db-primary (eu-central)",
                "  Web (2)",
                "    web-01",
                "    web-02",
                "  bastion",
                "  cache-01",
            ]
        );
    }

    #[test]
    fn styles_the_documented_folders() {
        let styles = demo_folder_styles();
        assert_eq!(styles["folder:lab/fabric"].icon.as_deref(), Some("lan"));
        assert_eq!(styles.len(), 5);
    }
}
