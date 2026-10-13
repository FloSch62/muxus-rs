//! The documentation's demo sandbox (`hack/demo-env.mjs`): the same hosts,
//! folders, saved hosts and tunnels, written into a scratch home and seeded
//! through a sandboxed [`Backend`]. The examples render the editors on it.

use std::path::{Path, PathBuf};

use super::palette::demo;
use muxus_core::api::{
    ForwardType, OpenSshMetadataPatch, SavedHostProfileInput, SessionLoggingPolicyInput,
    TunnelInput,
};
use muxus_core::js::Patch;
use muxus_core::protocol::{
    RdpProfile, SavedHostSessionProfile, SerialProfile, SshGateway, TelnetProfile, VncProfile,
};
use muxus_session::{Backend, BackendOptions};

struct DemoHost {
    alias: &'static str,
    hostname: &'static str,
    user: &'static str,
    folder: &'static str,
    color: Option<&'static str>,
    jump: Option<&'static str>,
    keyboard_interactive: bool,
}

const DEMO_HOSTS: [DemoHost; 8] = [
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
    DemoHost {
        alias: "lab-leaf-01",
        hostname: "leaf-01.lab.internal",
        user: "admin",
        folder: "Lab/Fabric",
        color: Some(demo::PURPLE),
        jump: None,
        keyboard_interactive: false,
    },
    DemoHost {
        alias: "lab-spine-01",
        hostname: "spine-01.lab.internal",
        user: "admin",
        folder: "Lab/Fabric",
        color: Some(demo::PURPLE),
        jump: None,
        keyboard_interactive: false,
    },
];

const SHOWCASE_LINES: [&str; 8] = [
    "    HostName db-primary.prod.internal",
    "    User postgres",
    "    Port 22",
    "    IdentityFile ~/.ssh/id_prod_ed25519",
    "    ProxyJump bastion",
    "    LocalForward 15432 127.0.0.1:5432",
    "    ForwardAgent yes",
    "    ServerAliveInterval 30",
];

fn ssh_config() -> String {
    let mut lines = vec![
        "# Your OpenSSH configuration — Muxus reads it and edits blocks in place.".to_owned(),
        String::new(),
        "Host *".into(),
        "    ServerAliveInterval 30".into(),
        "    ServerAliveCountMax 3".into(),
        String::new(),
        "Include ~/.ssh/config.d/lab.conf".into(),
        String::new(),
    ];
    for host in DEMO_HOSTS.iter().filter(|h| !h.alias.starts_with("lab-")) {
        lines.push(format!("Host {}", host.alias));
        lines.push(format!("    HostName {}", host.hostname));
        lines.push(format!("    User {}", host.user));
        if let Some(jump) = host.jump {
            lines.push(format!("    ProxyJump {jump}"));
        }
        if !host.keyboard_interactive {
            lines.push("    IdentityFile ~/.ssh/id_ed25519".into());
        }
        if host.alias == "web-01" {
            lines.push("    LocalForward 8080 127.0.0.1:8080".into());
        }
        lines.push(String::new());
    }
    lines.push("Host db-primary".into());
    lines.extend(SHOWCASE_LINES.iter().map(|l| (*l).to_owned()));
    lines.push(String::new());
    lines.join("\n")
}

fn lab_config() -> String {
    let mut lines = vec![
        "# Lab fabric — a separate file pulled in with Include.".to_owned(),
        String::new(),
    ];
    for host in DEMO_HOSTS.iter().filter(|h| h.alias.starts_with("lab-")) {
        lines.push(format!("Host {}", host.alias));
        lines.push(format!("    HostName {}", host.hostname));
        lines.push(format!("    User {}", host.user));
        lines.push("    IdentityFile ~/.ssh/id_ed25519".into());
        lines.push(String::new());
    }
    lines.join("\n")
}

/// Where a demo sandbox lives.
pub struct DemoSandbox {
    pub root: PathBuf,
    pub home: PathBuf,
    pub data: PathBuf,
}

impl DemoSandbox {
    /// Lay out `root/home` (with `.ssh/config` and its Include) and `root/data`.
    pub fn create(root: &Path) -> std::io::Result<Self> {
        let home = root.join("home");
        let data = root.join("data");
        let ssh = home.join(".ssh");
        std::fs::create_dir_all(ssh.join("config.d"))?;
        std::fs::create_dir_all(&data)?;
        std::fs::write(ssh.join("config"), ssh_config())?;
        std::fs::write(ssh.join("config.d").join("lab.conf"), lab_config())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for file in [ssh.join("config"), ssh.join("config.d").join("lab.conf")] {
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(Self {
            root: root.to_owned(),
            home,
            data,
        })
    }

    /// A sandboxed backend on this home and data folder; saved tunnels stay stopped.
    pub fn backend(&self) -> Result<Backend, String> {
        let mut options = BackendOptions::sandbox(&self.data, &self.home);
        options.autostart_tunnels = false;
        Backend::start_blocking(options).map_err(|e| e.message)
    }
}

fn group_patch(
    folder: &str,
    color: Option<&str>,
    display_name: Option<&str>,
) -> OpenSshMetadataPatch {
    OpenSshMetadataPatch {
        group: Patch::Set(folder.to_owned()),
        color: match color {
            Some(color) => Patch::Set(color.to_owned()),
            None => Patch::Clear,
        },
        display_name: match display_name {
            Some(name) => Patch::Set(name.to_owned()),
            None => Patch::Keep,
        },
        ..OpenSshMetadataPatch::default()
    }
}

/// Folders, colours, saved hosts and tunnels — the state `demo-env.mjs` seeds.
pub async fn seed(backend: &Backend) -> Result<(), String> {
    let err = |e: muxus_session::ApiError| e.message;
    for host in &DEMO_HOSTS {
        backend
            .patch_ssh_host_metadata(host.alias, group_patch(host.folder, host.color, None))
            .await
            .map_err(err)?;
    }
    backend
        .patch_ssh_host_metadata(
            "db-primary",
            OpenSshMetadataPatch {
                group: Patch::Set("Production/Data".into()),
                display_name: Patch::Set("db-primary (eu-central)".into()),
                ..OpenSshMetadataPatch::default()
            },
        )
        .await
        .map_err(err)?;
    backend
        .put_session_logging_policy(
            "*",
            SessionLoggingPolicyInput {
                enabled: true,
                capture_input: false,
                max_part_bytes: 5 * 1024 * 1024,
                max_parts: 10,
                log_to_file: None,
            },
        )
        .await
        .map_err(err)?;
    let saved = [
        (
            "core-switch (console)",
            SavedHostSessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: "192.0.2.20".into(),
                port: 23,
            }),
            "Lab/Fabric",
            Some(demo::YELLOW),
        ),
        (
            "lab-console",
            SavedHostSessionProfile::Serial(SerialProfile::new("/dev/ttyUSB0")),
            "Lab",
            None,
        ),
        (
            "win-build",
            SavedHostSessionProfile::Rdp(RdpProfile {
                profile_id: None,
                host: "win-build.corp.internal".into(),
                port: 3389,
                username: Some("CORP\\ci".into()),
                domain: None,
                ssh_gateway: Some(SshGateway {
                    target: "bastion".into(),
                    profile_id: None,
                }),
                share_clipboard: None,
            }),
            "Lab",
            Some(demo::BLUE),
        ),
        (
            "design-vm",
            SavedHostSessionProfile::Vnc(VncProfile {
                profile_id: None,
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
    ];
    for (name, profile, folder, color) in saved {
        let host = backend
            .save_saved_host(SavedHostProfileInput {
                id: None,
                name: name.into(),
                profile,
            })
            .await
            .map_err(err)?;
        let patch = OpenSshMetadataPatch {
            group: Patch::Set(folder.into()),
            color: color
                .map(|c| Patch::Set(c.to_owned()))
                .unwrap_or(Patch::Keep),
            ..OpenSshMetadataPatch::default()
        };
        backend
            .patch_saved_host_metadata(&host.id, patch)
            .await
            .map_err(err)?;
    }
    let tunnels = [
        (
            "Grafana",
            "web-01",
            ForwardType::Local,
            3000,
            Some(("127.0.0.1", 3000)),
        ),
        (
            "Postgres (via bastion)",
            "bastion",
            ForwardType::Local,
            15432,
            Some(("db-01.prod.internal", 5432)),
        ),
        (
            "Lab SOCKS proxy",
            "bastion",
            ForwardType::Dynamic,
            1080,
            None,
        ),
    ];
    for (name, target, kind, bind, dest) in tunnels {
        backend
            .save_tunnel(TunnelInput {
                id: None,
                name: Some(name.into()),
                target: target.into(),
                ssh_options: None,
                kind,
                bind_port: bind,
                target_host: dest.map(|d| d.0.to_owned()),
                target_port: dest.map(|d| d.1),
                auto_start: None,
                auto_reconnect: None,
            })
            .await
            .map_err(err)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_documented_ssh_config() {
        let text = ssh_config();
        assert!(text.contains("Host db-primary\n    HostName db-primary.prod.internal"));
        assert!(text.contains("Include ~/.ssh/config.d/lab.conf"));
        assert!(lab_config().contains("Host lab-spine-01"));
    }
}
