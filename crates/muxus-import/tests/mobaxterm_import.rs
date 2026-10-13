//! Port of `tests/unit/client/mobaxterm-import.test.ts`.

use std::sync::Arc;

use muxus_core::api::{HostBlockOptions, saved_host_hop};
use muxus_core::protocol::{
    RdpProfile, RequestTty, SavedHostSessionProfile, SshGateway, SshProfile,
};
use muxus_import::{
    ImportedAuthMode, ImportedSession, ImportedSshSession, ImportedSshStorage,
    PortableHostMetadata, PortableSavedHost, PortableSshHost, mobaxterm_connections,
    parse_mobaxterm_sessions,
};

fn ssh(session: &ImportedSession) -> &ImportedSshSession {
    match session {
        ImportedSession::Ssh(ssh) => ssh,
        other => panic!("not an SSH session: {other:?}"),
    }
}

#[test]
fn reads_ssh_fields_nested_folders_and_authentication_intent() {
    let parsed = parse_mobaxterm_sessions(
        r"
[Misc]
LastSession=Do not import|#109#0%ignored.example.com%22%root%%rest

[Bookmarks]
SubRep=Production\Europe
Prod SSH=#109#0%prod.example.com%2222%deploy%%rest

[Bookmarks_1]
SubRep=Lab
Key box=#109#0%key.example.com%22%root%3%rest
",
    )
    .expect("parsed");

    assert_eq!(parsed.ignored_count, 0);
    assert!(parsed.skipped_sessions.is_empty());
    assert_eq!(parsed.sessions.len(), 2);
    let prod = ssh(&parsed.sessions[0]);
    assert_eq!(
        (
            prod.name.as_str(),
            prod.alias.as_str(),
            prod.host.as_str(),
            prod.port,
            prod.username.as_deref(),
            prod.folder.as_deref(),
            prod.auth_mode
        ),
        (
            "Prod SSH",
            "Prod-SSH",
            "prod.example.com",
            2222,
            Some("deploy"),
            Some("Production/Europe"),
            ImportedAuthMode::Password
        )
    );
    assert!(
        prod.identity_file.is_none() && prod.remote_command.is_none() && prod.jump_host.is_none()
    );
    let key = ssh(&parsed.sessions[1]);
    assert_eq!(
        (
            key.name.as_str(),
            key.alias.as_str(),
            key.host.as_str(),
            key.port,
            key.username.as_deref(),
            key.folder.as_deref(),
            key.auth_mode
        ),
        (
            "Key box",
            "Key-box",
            "key.example.com",
            22,
            Some("root"),
            Some("Lab"),
            ImportedAuthMode::Key
        )
    );
}

#[test]
fn makes_duplicate_and_openssh_invalid_names_safe_without_losing_display_names() {
    let parsed = parse_mobaxterm_sessions(
        "
[Bookmarks]
SubRep=
My host!=#109#0%one.example.com%not-a-port%%%rest
My host!=#109#0%two.example.com%22%%%rest
",
    )
    .expect("parsed");

    let rows: Vec<_> = parsed
        .sessions
        .iter()
        .map(|session| {
            let session = ssh(session);
            (
                session.alias.as_str(),
                session.port,
                session.username.clone(),
            )
        })
        .collect();
    assert_eq!(rows, [("My-host", 22, None), ("My-host-2", 22, None)]);
}

#[test]
fn uses_the_protocol_field_instead_of_the_leading_metadata_identifier() {
    let parsed = parse_mobaxterm_sessions(
        "
[Bookmarks]
SubRep=
Custom icon SSH=#114#0%airframe.example.com%2222%root%%rest
",
    )
    .expect("parsed");

    assert_eq!(parsed.sessions.len(), 1);
    let session = ssh(&parsed.sessions[0]);
    assert_eq!(session.name, "Custom icon SSH");
    assert_eq!(session.alias, "Custom-icon-SSH");
    assert_eq!(session.host, "airframe.example.com");
    assert_eq!(session.port, 2222);
    assert_eq!(session.username.as_deref(), Some("root"));
    assert_eq!(session.folder, None);
    assert_eq!(session.auth_mode, ImportedAuthMode::Password);
}

#[test]
fn counts_unsupported_and_malformed_bookmark_entries() {
    let parsed = parse_mobaxterm_sessions(
        "
[Bookmarks]
Telnet=#98#1%switch.example.com%23%%%2%%22%%%0#MobaFont%10
WSL=#105#14%Ubuntu%
Broken SSH=#109#0%%22%root%%
Broken RDP=#91#4%%3389%admin%0
Valid=#109#0%valid.example.com%22%root%%
",
    )
    .expect("parsed");

    assert_eq!(parsed.sessions.len(), 1);
    assert_eq!(parsed.ignored_count, 4);
    let skipped: Vec<_> = parsed
        .skipped_sessions
        .iter()
        .map(|skipped| {
            (
                skipped.name.as_str(),
                skipped.folder.as_deref(),
                skipped.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        skipped,
        [
            (
                "Telnet",
                None,
                "Only SSH, RDP and VNC sessions can be imported"
            ),
            (
                "WSL",
                None,
                "Only SSH, RDP and VNC sessions can be imported"
            ),
            ("Broken SSH", None, "SSH session has no hostname"),
            ("Broken RDP", None, "RDP session has no hostname"),
        ]
    );
}

#[test]
fn rejects_files_with_nothing_it_can_import() {
    let error = parse_mobaxterm_sessions(
        "
[Bookmarks]
SubRep=
Telnet=#98#1%switch.example.com%23%%%2%%22%%%0
",
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("No SSH, RDP or VNC sessions were found")
    );
}

#[test]
fn rejects_files_over_ten_megabytes() {
    let text = "x".repeat(10 * 1024 * 1024 + 1);
    let error = parse_mobaxterm_sessions(&text).unwrap_err();
    assert_eq!(
        error.to_string(),
        "That MobaXterm file is larger than 10 MB."
    );
}

#[test]
fn reads_private_keys_and_ssh_gateways_without_reading_terminal_settings() {
    let parsed = parse_mobaxterm_sessions(
        r"
[Bookmarks]
SubRep=Lab
Direct key=#109#0%direct.example.com%22%admin%%-1%-1%%%%%0%0%0%_ProfileDir_\ssh_keys\id_ecdsa%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15%236,236,236
Via bastion=#109#0%10.0.0.5%22%root%%-1%-1%%bastion.example.com%2222%jump%0%0%0%%_CurrentDrive_:\keys\bastion.ppk%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Two hops=#109#0%10.0.1.5%22%root%%-1%-1%%bastion.example.com__PIPE__inner.example.com%2222__PIPE__22%jump__PIPE__ops%0%0%0%%_CurrentDrive_:\keys\bastion.ppk__PIPE__%-1%0%0%0%%1080%%0%0%1#MobaFont%10
",
    )
    .expect("parsed");

    let direct = ssh(&parsed.sessions[0]);
    assert_eq!(direct.host, "direct.example.com");
    assert_eq!(direct.auth_mode, ImportedAuthMode::Key);
    assert_eq!(direct.identity_file.as_deref(), Some("~/ssh_keys/id_ecdsa"));
    assert!(direct.jump_host.is_none());

    let via_bastion = ssh(&parsed.sessions[1]);
    assert_eq!(via_bastion.auth_mode, ImportedAuthMode::Password);
    assert!(via_bastion.identity_file.is_none());
    let bastion = via_bastion.jump_host.as_ref().expect("bastion");
    assert_eq!(bastion.alias, "jump-jump-bastion.example.com-2222");
    assert_eq!(bastion.name, "jump@bastion.example.com:2222");
    assert_eq!(bastion.host, "bastion.example.com");
    assert_eq!(bastion.port, 2222);
    assert_eq!(bastion.username.as_deref(), Some("jump"));
    assert_eq!(
        bastion.identity_file.as_deref(),
        Some("C:/keys/bastion.ppk")
    );
    assert!(bastion.via.is_none());

    let two_hops = ssh(&parsed.sessions[2]);
    let inner = two_hops.jump_host.as_ref().expect("inner hop");
    assert_eq!(inner.alias, "jump-ops-inner.example.com");
    assert_eq!(
        inner.name,
        "ops@inner.example.com via jump@bastion.example.com:2222"
    );
    assert_eq!(inner.host, "inner.example.com");
    assert_eq!(inner.port, 22);
    assert_eq!(inner.username.as_deref(), Some("ops"));
    assert!(inner.identity_file.is_none());
    // One shared value per distinct hop, so the bastion is imported once.
    assert!(Arc::ptr_eq(inner.via.as_ref().expect("via"), bastion));
}

#[test]
fn keeps_the_execute_command_as_the_startup_command() {
    let parsed = parse_mobaxterm_sessions(
        "
[Bookmarks]
Root=#109#0%a.example.com%22%admin%%-1%-1%sudo su -__PTVIRG__ echo done__PTVIRG__%%%%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Nested=#109#0%b.example.com%22%root%%-1%-1%echo pw __PIPE__ sshpass -p pw ssh -oHostKeyAlgorithms__EQUAL__+ssh-rsa admin@10.0.0.1%%%%-1%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Plain=#109#0%c.example.com%22%root%%-1%-1%%%%%-1%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
",
    )
    .expect("parsed");

    let commands: Vec<_> = parsed
        .sessions
        .iter()
        .map(|session| ssh(session).remote_command.clone())
        .collect();
    assert_eq!(
        commands,
        [
            Some("sudo su -; echo done".to_owned()),
            Some(
                "echo pw | sshpass -p pw ssh -oHostKeyAlgorithms=+ssh-rsa admin@10.0.0.1; exec \"$SHELL\" -l"
                    .to_owned()
            ),
            None,
        ]
    );

    let muxus = mobaxterm_connections(&parsed.sessions, ImportedSshStorage::Muxus);
    let SavedHostSessionProfile::Ssh(root) = &muxus.saved_hosts[0].profile else {
        panic!("not SSH");
    };
    assert_eq!(root.remote_command.as_deref(), Some("sudo su -; echo done"));
    assert_eq!(root.request_tty, Some(RequestTty::Yes));
    let openssh = mobaxterm_connections(&parsed.sessions, ImportedSshStorage::Openssh);
    let plain = &openssh.ssh_hosts[2].options;
    assert!(plain.remote_command.is_none());
    assert!(plain.request_tty.is_none());
}

#[test]
fn reads_rdp_and_vnc_sessions_with_their_ssh_gateways() {
    let parsed = parse_mobaxterm_sessions(
        r"
[Bookmarks]
SubRep=Desktops
Windows=#91#4%win.example.com%3390%Administrator%0%-1%-1%-1%-1%0%0%-1%%bastion.example.com%22%root%0%0%_ProfileDir_\keys\id_ed25519%-1%%-1%-1%0%-1%0%-1#MobaFont%10%0
Console=#128#5%127.0.0.1%5901%-1%0%bastion.example.com%22%root%%-1#MobaFont%10%0%0%-1%15%236,236,236
Plain VNC=#128#5%vnc.example.com%%-1%0%%22%%%-1#MobaFont%10
",
    )
    .expect("parsed");

    assert_eq!(parsed.sessions.len(), 3);
    let ImportedSession::Rdp(windows) = &parsed.sessions[0] else {
        panic!("not RDP");
    };
    assert!(windows.id.starts_with("mobaxterm-rdp-"));
    assert_eq!(windows.name, "Windows");
    assert_eq!(windows.host, "win.example.com");
    assert_eq!(windows.port, 3390);
    assert_eq!(windows.username.as_deref(), Some("Administrator"));
    assert_eq!(windows.folder.as_deref(), Some("Desktops"));
    let gateway = windows.jump_host.as_ref().expect("gateway");
    assert_eq!(gateway.name, "root@bastion.example.com");
    assert_eq!(gateway.identity_file.as_deref(), Some("~/keys/id_ed25519"));

    let ImportedSession::Vnc(console) = &parsed.sessions[1] else {
        panic!("not VNC");
    };
    assert!(console.id.starts_with("mobaxterm-vnc-"));
    assert_eq!(console.name, "Console");
    assert_eq!(console.host, "127.0.0.1");
    assert_eq!(console.port, 5901);
    assert_eq!(console.username, None);
    assert_eq!(console.folder.as_deref(), Some("Desktops"));
    assert_eq!(
        console.jump_host.as_ref().expect("gateway").name,
        "root@bastion.example.com (2)"
    );

    let ImportedSession::Vnc(plain) = &parsed.sessions[2] else {
        panic!("not VNC");
    };
    assert!(plain.id.starts_with("mobaxterm-vnc-"));
    assert_eq!(plain.name, "Plain VNC");
    assert_eq!(plain.host, "vnc.example.com");
    assert_eq!(plain.port, 5900);
    assert_eq!(plain.username, None);
    assert_eq!(plain.folder.as_deref(), Some("Desktops"));
    assert!(plain.jump_host.is_none());
}

#[test]
fn maps_sessions_into_muxus_hosts_without_copying_secrets() {
    let parsed = parse_mobaxterm_sessions(
        r"
[Bookmarks]
SubRep=Customers\Acme
Password host=#109#0%pw.example.com%2200%alice%%rest
Key host=#109#0%key.example.com%22%bob%3%rest
",
    )
    .expect("parsed");
    let portable = mobaxterm_connections(&parsed.sessions, ImportedSshStorage::default());

    assert!(portable.saved_hosts.is_empty());
    assert!(portable.host_order.is_empty());
    let metadata = |name: &str| PortableHostMetadata {
        display_name: Some(name.to_owned()),
        group: Some("Customers/Acme".to_owned()),
        ..PortableHostMetadata::default()
    };
    assert_eq!(
        portable.ssh_hosts,
        [
            PortableSshHost {
                alias: "Password-host".to_owned(),
                aliases: vec!["Password-host".to_owned()],
                description: Some("Imported from MobaXterm.".to_owned()),
                options: HostBlockOptions {
                    hostname: Some("pw.example.com".to_owned()),
                    user: Some("alice".to_owned()),
                    port: Some(2200),
                    password_only: Some(true),
                    ..HostBlockOptions::default()
                },
                metadata: Some(metadata("Password host")),
            },
            PortableSshHost {
                alias: "Key-host".to_owned(),
                aliases: vec!["Key-host".to_owned()],
                description: Some("Imported from MobaXterm.".to_owned()),
                options: HostBlockOptions {
                    hostname: Some("key.example.com".to_owned()),
                    user: Some("bob".to_owned()),
                    ..HostBlockOptions::default()
                },
                metadata: Some(metadata("Key host")),
            },
        ]
    );
}

#[test]
fn can_keep_imported_ssh_sessions_in_muxus_app_data() {
    let parsed = parse_mobaxterm_sessions(
        r"
[Bookmarks]
SubRep=Customers\Acme
Password host=#109#0%pw.example.com%2200%alice%%rest
",
    )
    .expect("parsed");
    let portable = mobaxterm_connections(&parsed.sessions, ImportedSshStorage::Muxus);

    assert!(portable.ssh_hosts.is_empty());
    assert_eq!(portable.saved_hosts.len(), 1);
    let saved = &portable.saved_hosts[0];
    assert!(saved.id.starts_with("mobaxterm-ssh-"));
    assert_eq!(saved.name, "Password host");
    assert_eq!(
        saved.profile,
        SavedHostSessionProfile::Ssh(SshProfile {
            use_config: Some(false),
            user: Some("alice".to_owned()),
            port: Some(2200),
            password_only: Some(true),
            ..SshProfile::new("pw.example.com")
        })
    );
    assert_eq!(
        saved.metadata,
        PortableHostMetadata::in_group(Some("Customers/Acme"))
    );
}

const GATEWAY_FILE: &str = r"
[Bookmarks]
SubRep=Lab
Inner=#109#0%10.0.1.5%22%root%%-1%-1%%bastion.example.com__PIPE__inner.example.com%2222__PIPE__22%jump__PIPE__ops%0%0%0%_ProfileDir_\keys\lab%_ProfileDir_\keys\bastion__PIPE__%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Windows=#91#4%win.example.com%3389%admin%0%-1%-1%-1%-1%0%0%-1%%bastion.example.com%2222%jump%0%0%_ProfileDir_\keys\bastion%-1%%-1%-1%0%-1%0%-1#MobaFont%10
";

#[test]
fn keeps_keys_and_jump_hosts_as_saved_muxus_hosts() {
    let parsed = parse_mobaxterm_sessions(GATEWAY_FILE).expect("parsed");
    let portable = mobaxterm_connections(&parsed.sessions, ImportedSshStorage::Muxus);

    assert!(portable.ssh_hosts.is_empty());
    assert_eq!(portable.saved_hosts.len(), 4);
    let [inner, windows, inner_hop, bastion] = portable.saved_hosts.as_slice() else {
        panic!("four saved hosts");
    };
    let jump_group = PortableHostMetadata::in_group(Some("MobaXterm jump hosts"));
    assert!(bastion.id.starts_with("mobaxterm-jump-"));
    assert_eq!(
        bastion,
        &PortableSavedHost {
            id: bastion.id.clone(),
            name: "jump@bastion.example.com:2222".to_owned(),
            profile: SavedHostSessionProfile::Ssh(SshProfile {
                use_config: Some(false),
                user: Some("jump".to_owned()),
                port: Some(2222),
                identity_files: Some(vec!["~/keys/bastion".to_owned()]),
                ..SshProfile::new("bastion.example.com")
            }),
            metadata: jump_group.clone(),
        }
    );
    assert!(inner_hop.id.starts_with("mobaxterm-jump-"));
    assert_eq!(
        inner_hop,
        &PortableSavedHost {
            id: inner_hop.id.clone(),
            name: "ops@inner.example.com via jump@bastion.example.com:2222".to_owned(),
            profile: SavedHostSessionProfile::Ssh(SshProfile {
                use_config: Some(false),
                user: Some("ops".to_owned()),
                password_only: Some(true),
                proxy_jump: Some(vec![saved_host_hop(&bastion.id)]),
                ..SshProfile::new("inner.example.com")
            }),
            metadata: jump_group,
        }
    );
    assert!(inner.id.starts_with("mobaxterm-ssh-"));
    assert_eq!(
        inner,
        &PortableSavedHost {
            id: inner.id.clone(),
            name: "Inner".to_owned(),
            profile: SavedHostSessionProfile::Ssh(SshProfile {
                use_config: Some(false),
                user: Some("root".to_owned()),
                identity_files: Some(vec!["~/keys/lab".to_owned()]),
                proxy_jump: Some(vec![saved_host_hop(&inner_hop.id)]),
                ..SshProfile::new("10.0.1.5")
            }),
            metadata: PortableHostMetadata::in_group(Some("Lab")),
        }
    );
    assert!(windows.id.starts_with("mobaxterm-rdp-"));
    assert_eq!(
        windows,
        &PortableSavedHost {
            id: windows.id.clone(),
            name: "Windows".to_owned(),
            profile: SavedHostSessionProfile::Rdp(RdpProfile {
                profile_id: None,
                host: "win.example.com".to_owned(),
                port: 3389,
                username: Some("admin".to_owned()),
                domain: None,
                ssh_gateway: Some(SshGateway {
                    target: "bastion.example.com".to_owned(),
                    profile_id: Some(bastion.id.clone()),
                }),
                share_clipboard: None,
            }),
            metadata: PortableHostMetadata::in_group(Some("Lab")),
        }
    );
}

#[test]
fn writes_jump_hosts_as_chained_host_blocks_for_openssh_config() {
    let parsed = parse_mobaxterm_sessions(GATEWAY_FILE).expect("parsed");
    let portable = mobaxterm_connections(&parsed.sessions, ImportedSshStorage::Openssh);

    let blocks: Vec<_> = portable
        .ssh_hosts
        .iter()
        .map(|host| (host.alias.as_str(), host.options.clone()))
        .collect();
    assert_eq!(
        blocks,
        [
            (
                "Inner",
                HostBlockOptions {
                    hostname: Some("10.0.1.5".to_owned()),
                    user: Some("root".to_owned()),
                    identity_files: Some(vec!["~/keys/lab".to_owned()]),
                    proxy_jump: Some(vec!["jump-ops-inner.example.com".to_owned()]),
                    ..HostBlockOptions::default()
                }
            ),
            (
                "jump-ops-inner.example.com",
                HostBlockOptions {
                    hostname: Some("inner.example.com".to_owned()),
                    user: Some("ops".to_owned()),
                    password_only: Some(true),
                    proxy_jump: Some(vec!["jump-jump-bastion.example.com-2222".to_owned()]),
                    ..HostBlockOptions::default()
                }
            ),
            (
                "jump-jump-bastion.example.com-2222",
                HostBlockOptions {
                    hostname: Some("bastion.example.com".to_owned()),
                    user: Some("jump".to_owned()),
                    port: Some(2222),
                    identity_files: Some(vec!["~/keys/bastion".to_owned()]),
                    ..HostBlockOptions::default()
                }
            ),
        ]
    );
    assert_eq!(
        portable.ssh_hosts[2].metadata,
        Some(PortableHostMetadata {
            display_name: Some("jump@bastion.example.com:2222".to_owned()),
            group: Some("MobaXterm jump hosts".to_owned()),
            ..PortableHostMetadata::default()
        })
    );
    assert_eq!(portable.saved_hosts.len(), 1);
    assert_eq!(portable.saved_hosts[0].name, "Windows");
    let SavedHostSessionProfile::Rdp(rdp) = &portable.saved_hosts[0].profile else {
        panic!("not RDP");
    };
    assert_eq!(
        rdp.ssh_gateway,
        Some(SshGateway {
            target: "jump-jump-bastion.example.com-2222".to_owned(),
            profile_id: None,
        })
    );
}
