//! Port of tests/unit/server/chain.test.ts (dial-plan parts) and the buildChain cases of
//! folder-auth.test.ts.

use crate::types::ConfigForwardExt;
use std::collections::HashMap;

use super::*;
use crate::config::tests::Fixture;
use crate::folder_auth::folder_auth_option_lines;
use crate::types::{
    FolderAuthSettings, ForwardType, RequestTty, StrictHostKeyChecking, saved_host_hop,
};

fn chain(f: &Fixture, content: &str, profile: SshProfile) -> Vec<ChainHop> {
    build_chain(&f.doc(content), &profile, ChainSources::default()).unwrap()
}

fn p(target: &str) -> SshProfile {
    SshProfile::new(target)
}

fn lines(parts: &[&str]) -> String {
    parts.join("\n")
}

#[test]
fn is_a_single_hop_without_proxy_jump() {
    let f = Fixture::new();
    let c = chain(
        &f,
        &lines(&[
            "Host web",
            "  HostName web.example.com",
            "  User deploy",
            "  Port 2222",
        ]),
        p("web"),
    );
    assert_eq!(c.len(), 1);
    assert_eq!(
        (c[0].user.as_str(), c[0].port, c[0].hop_label.clone()),
        ("deploy", 2222, None)
    );
    assert_eq!(c[0].resolved.hostname, "web.example.com");
}

#[test]
fn dials_jump_hops_first_resolving_each_through_the_config() {
    let f = Fixture::new();
    let c = chain(
        &f,
        &lines(&[
            "Host app",
            "  HostName app.internal",
            "  ProxyJump bastion",
            "",
            "Host bastion",
            "  HostName bastion.example.com",
            "  User jumpuser",
            "  Port 2200",
        ]),
        p("app"),
    );
    let hosts: Vec<&str> = c.iter().map(|h| h.resolved.hostname.as_str()).collect();
    assert_eq!(hosts, vec!["bastion.example.com", "app.internal"]);
    assert_eq!(
        (c[0].user.as_str(), c[0].port, c[0].hop_label.as_deref()),
        ("jumpuser", 2200, Some("bastion"))
    );
    assert_eq!(c[1].hop_label, None);
}

#[test]
fn uses_the_muxus_keepalive_fallback_without_overriding_ssh_configuration() {
    let f = Fixture::new();
    let c = chain(
        &f,
        &lines(&[
            "Host app",
            "  ProxyJump bastion",
            "  ServerAliveInterval 90",
            "",
            "Host bastion",
            "  HostName bastion.example.com",
        ]),
        SshProfile {
            keepalive_interval_seconds: Some(30),
            ..p("app")
        },
    );
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].resolved.server_alive_interval, Some(30.0));
    assert_eq!(c[1].resolved.server_alive_interval, Some(90.0));
}

#[test]
fn expands_nested_and_comma_listed_jumps_in_dial_order() {
    let f = Fixture::new();
    let c = chain(
        &f,
        &lines(&[
            "Host app",
            "  ProxyJump j1,ops@j2:2202",
            "",
            "Host j1",
            "  HostName j1.example.com",
            "  ProxyJump j0",
            "",
            "Host j0",
            "  HostName j0.example.com",
        ]),
        p("app"),
    );
    let specs: Vec<&str> = c.iter().map(|h| h.spec.host.as_str()).collect();
    assert_eq!(specs, vec!["j0", "j1", "j2", "app"]);
    assert_eq!((c[2].user.as_str(), c[2].port), ("ops", 2202));
}

#[test]
fn applies_profile_user_port_overrides_to_the_final_target_only() {
    let f = Fixture::new();
    let c = chain(
        &f,
        &lines(&[
            "Host app",
            "  User configured",
            "  ProxyJump bastion",
            "",
            "Host bastion",
            "  User jumpuser",
        ]),
        SshProfile {
            user: Some("override".into()),
            port: Some(2022),
            ..p("app")
        },
    );
    assert_eq!(c[0].user, "jumpuser");
    assert_eq!((c[1].user.as_str(), c[1].port), ("override", 2022));
}

#[test]
fn uses_a_self_contained_final_profile_while_resolving_configured_jumps() {
    let f = Fixture::new();
    let c = chain(
        &f,
        &lines(&[
            "Host app.internal",
            "  HostName wrong.example.com",
            "  User wrong-user",
            "  IdentityFile ~/.ssh/wrong",
            "  ProxyJump wrong-hop",
            "",
            "Host bastion",
            "  HostName bastion.example.com",
            "  User jumpuser",
        ]),
        SshProfile {
            use_config: Some(false),
            user: Some("deploy".into()),
            port: Some(2222),
            identity_files: Some(vec!["~/.ssh/tunnel_ed25519".into()]),
            certificate_files: Some(vec!["~/.ssh/tunnel_ed25519-cert.pub".into()]),
            identities_only: Some(true),
            identity_agent: Some("none".into()),
            proxy_jump: Some(vec!["bastion".into()]),
            forwards: Some(vec![ConfigForward::to(
                ForwardType::Local,
                8080,
                "127.0.0.1",
                80,
            )]),
            remote_command: Some("tmux new -A -s main".into()),
            request_tty: Some(RequestTty::Yes),
            strict_host_key_checking: Some(StrictHostKeyChecking::AcceptNew),
            ..p("app.internal")
        },
    );
    let hosts: Vec<&str> = c.iter().map(|h| h.resolved.hostname.as_str()).collect();
    assert_eq!(hosts, vec!["bastion.example.com", "app.internal"]);
    assert_eq!((c[0].user.as_str(), c[0].port), ("jumpuser", 22));
    assert_eq!((c[1].user.as_str(), c[1].port), ("deploy", 2222));
    let r = &c[1].resolved;
    assert!(r.identities_only);
    assert_eq!(r.identity_agent.as_deref(), Some("none"));
    assert_eq!(r.proxy_jump, vec!["bastion".to_string()]);
    assert_eq!(
        r.forwards,
        vec![ConfigForward::to(ForwardType::Local, 8080, "127.0.0.1", 80)]
    );
    assert_eq!(r.remote_command.as_deref(), Some("tmux new -A -s main"));
    assert_eq!(r.request_tty, Some(RequestTty::Yes));
    assert_eq!(
        r.strict_host_key_checking,
        Some(StrictHostKeyChecking::AcceptNew)
    );
    assert!(r.identity_files[0].ends_with("/.ssh/tunnel_ed25519"));
    assert!(r.certificate_files[0].ends_with("/.ssh/tunnel_ed25519-cert.pub"));
}

#[test]
fn parses_ad_hoc_user_host_port_targets() {
    let f = Fixture::new();
    let c = chain(&f, "", p("root@203.0.113.7:2222"));
    assert_eq!((c[0].user.as_str(), c[0].port), ("root", 2222));
    assert_eq!(c[0].resolved.hostname, "203.0.113.7");
}

#[test]
fn detects_proxy_jump_cycles() {
    let f = Fixture::new();
    let err = build_chain(
        &f.doc(&lines(&[
            "Host a",
            "  ProxyJump b",
            "",
            "Host b",
            "  ProxyJump a",
        ])),
        &p("a"),
        ChainSources::default(),
    )
    .unwrap_err();
    assert!(err.0.contains("cycle"));
}

#[test]
fn keeps_proxy_command_on_a_direct_target_and_lets_a_profile_jump_override_it() {
    let f = Fixture::new();
    let content = lines(&[
        "Host app",
        "  HostName app.internal",
        "  ProxyCommand tunnel %h %p",
        "",
        "Host bastion",
        "  HostName bastion.example.com",
    ]);
    assert_eq!(
        chain(&f, &content, p("app"))[0]
            .resolved
            .proxy_command
            .as_deref(),
        Some("tunnel %h %p")
    );
    let jumped = chain(
        &f,
        &content,
        SshProfile {
            proxy_jump: Some(vec!["bastion".into()]),
            ..p("app")
        },
    );
    let specs: Vec<&str> = jumped.iter().map(|h| h.spec.host.as_str()).collect();
    assert_eq!(specs, vec!["bastion", "app"]);
    assert_eq!(jumped[1].resolved.proxy_command, None);
}

// --- saved Muxus jump hosts ---

fn saved_hosts() -> HashMap<String, SshProfile> {
    let saved = |id: &str, target: &str, f: &dyn Fn(&mut SshProfile)| {
        let mut profile = SshProfile {
            profile_id: Some(id.into()),
            use_config: Some(false),
            ..SshProfile::new(target)
        };
        f(&mut profile);
        (id.to_string(), profile)
    };
    HashMap::from([
        saved("bastion", "bastion.example.com", &|p| {
            p.user = Some("jumpuser".into());
            p.port = Some(2200);
            p.identity_files = Some(vec!["~/.ssh/bastion_ed25519".into()]);
            p.password_only = Some(false);
        }),
        saved("inner", "inner.example.com", &|p| {
            p.user = Some("relay".into());
            p.proxy_jump = Some(vec![saved_host_hop("bastion")]);
        }),
        saved("edge", "edge.example.com", &|p| {
            p.proxy_jump = Some(vec!["configured-jump".into()])
        }),
        saved("loopA", "a.example.com", &|p| {
            p.proxy_jump = Some(vec![saved_host_hop("loopB")])
        }),
        saved("loopB", "b.example.com", &|p| {
            p.proxy_jump = Some(vec![saved_host_hop("loopA")])
        }),
    ])
}

fn with_saved(
    doc: &ConfigDocument,
    profile: SshProfile,
    hosts: &HashMap<String, SshProfile>,
    folder: Option<FolderLookup<'_>>,
) -> Result<Vec<ChainHop>, DialPlanError> {
    let lookup = |id: &str| hosts.get(id).cloned();
    build_chain(
        doc,
        &profile,
        ChainSources {
            saved_profile: Some(&lookup),
            profile_folder_auth: folder,
            folder_auth: None,
        },
    )
}

#[test]
fn dials_a_saved_jump_host_with_its_own_user_port_and_key() {
    let f = Fixture::new();
    let hosts = saved_hosts();
    let c = with_saved(
        &f.doc("Host bastion.example.com\n  User config-user\n  IdentityFile ~/.ssh/wrong"),
        SshProfile {
            use_config: Some(false),
            user: Some("deploy".into()),
            port: Some(2022),
            identity_files: Some(vec!["~/.ssh/app_ed25519".into()]),
            password_only: Some(true),
            proxy_jump: Some(vec![saved_host_hop("bastion")]),
            ..p("app.internal")
        },
        &hosts,
        None,
    )
    .unwrap();
    let hostnames: Vec<&str> = c.iter().map(|h| h.resolved.hostname.as_str()).collect();
    assert_eq!(hostnames, vec!["bastion.example.com", "app.internal"]);
    // The saved host is self-contained, and the target's own overrides stay on the target
    // instead of leaking into the hop.
    assert_eq!(
        (c[0].user.as_str(), c[0].port, c[0].hop_label.as_deref()),
        ("jumpuser", 2200, Some("bastion.example.com"))
    );
    assert!(!c[0].resolved.password_only);
    assert_eq!(c[0].resolved.identity_files.len(), 1);
    assert!(c[0].resolved.identity_files[0].ends_with("/.ssh/bastion_ed25519"));
    assert_eq!((c[1].user.as_str(), c[1].port), ("deploy", 2022));
    assert!(c[1].resolved.password_only);
    assert!(c[1].resolved.identity_files[0].ends_with("/.ssh/app_ed25519"));
}

#[test]
fn follows_a_saved_jump_host_through_its_own_saved_jump_host() {
    let f = Fixture::new();
    let c = with_saved(
        &f.doc(""),
        SshProfile {
            use_config: Some(false),
            proxy_jump: Some(vec![saved_host_hop("inner")]),
            ..p("app.internal")
        },
        &saved_hosts(),
        None,
    )
    .unwrap();
    let plan: Vec<(&str, &str, u16)> = c
        .iter()
        .map(|h| (h.user.as_str(), h.resolved.hostname.as_str(), h.port))
        .collect();
    assert_eq!(plan[0], ("jumpuser", "bastion.example.com", 2200));
    assert_eq!(plan[1], ("relay", "inner.example.com", 22));
    assert_eq!((plan[2].1, plan[2].2), ("app.internal", 22));
}

#[test]
fn resolves_a_saved_jump_host_that_jumps_through_an_ssh_config_alias() {
    let f = Fixture::new();
    let c = with_saved(
        &f.doc(&lines(&[
            "Host configured-jump",
            "  HostName jump.example.com",
            "  User cfg",
        ])),
        SshProfile {
            use_config: Some(false),
            proxy_jump: Some(vec![saved_host_hop("edge")]),
            ..p("app.internal")
        },
        &saved_hosts(),
        None,
    )
    .unwrap();
    let specs: Vec<&str> = c.iter().map(|h| h.spec.host.as_str()).collect();
    assert_eq!(
        specs,
        vec!["configured-jump", "edge.example.com", "app.internal"]
    );
    assert_eq!(
        (c[0].user.as_str(), c[0].hop_label.as_deref()),
        ("cfg", Some("configured-jump"))
    );
}

#[test]
fn applies_the_saved_jump_host_folder_defaults_not_its_config_block() {
    let f = Fixture::new();
    let mut hosts = saved_hosts();
    hosts.get_mut("edge").unwrap().proxy_jump = None;
    let folder = |id: &str| {
        (id == "edge").then(|| FolderAuthDefaults {
            option_lines: folder_auth_option_lines(&FolderAuthSettings {
                user: Some("folder-user".into()),
                ..Default::default()
            }),
            passwords: vec![FolderPasswordRef {
                account: "acct".into(),
                label: "Folder".into(),
            }],
        })
    };
    let c = with_saved(
        &f.doc("Host edge.example.com\n  User config-user"),
        SshProfile {
            use_config: Some(false),
            proxy_jump: Some(vec![saved_host_hop("edge")]),
            ..p("app.internal")
        },
        &hosts,
        Some(&folder),
    )
    .unwrap();
    assert_eq!(c[0].user, "folder-user");
    assert_eq!(
        c[0].folder_passwords,
        Some(vec![FolderPasswordRef {
            account: "acct".into(),
            label: "Folder".into()
        }])
    );
}

#[test]
fn names_a_missing_saved_jump_host() {
    let f = Fixture::new();
    let err = with_saved(
        &f.doc(""),
        SshProfile {
            proxy_jump: Some(vec![saved_host_hop("deleted")]),
            ..p("app.internal")
        },
        &saved_hosts(),
        None,
    )
    .unwrap_err();
    assert!(
        err.0
            .contains("saved SSH host \"deleted\" used as a jump host was not found")
    );
}

#[test]
fn detects_cycles_between_saved_jump_hosts() {
    let f = Fixture::new();
    let hosts = saved_hosts();
    let err = with_saved(
        &f.doc(""),
        SshProfile {
            use_config: Some(false),
            proxy_jump: Some(vec![saved_host_hop("loopA")]),
            ..p("app.internal")
        },
        &hosts,
        None,
    )
    .unwrap_err();
    assert!(err.0.contains("cycle"));
    // A saved host that jumps through itself is a cycle too.
    let own = SshProfile {
        proxy_jump: Some(vec![saved_host_hop("loopA")]),
        ..hosts["loopA"].clone()
    };
    assert!(
        with_saved(&f.doc(""), own, &hosts, None)
            .unwrap_err()
            .0
            .contains("cycle")
    );
}

// --- ProxyCommand expansion, metadata aliases ---

#[test]
fn expands_openssh_destination_tokens_at_dial_time() {
    assert_eq!(
        expand_proxy_command(
            "proxy %% %h %n %p %r %x",
            &ProxyCommandTokens {
                hostname: "real.internal",
                original_host: "alias",
                port: 2222,
                user: "deploy"
            }
        ),
        "proxy % real.internal alias 2222 deploy %x"
    );
}

#[test]
fn only_attributes_recent_use_metadata_to_concrete_openssh_aliases() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host production prod",
        "  HostName 203.0.113.10",
        "",
        "Host *.internal",
        "  User deploy",
    ]));
    assert_eq!(
        find_metadata_alias(&doc, "production").as_deref(),
        Some("production")
    );
    assert_eq!(find_metadata_alias(&doc, "prod").as_deref(), Some("prod"));
    assert_eq!(find_metadata_alias(&doc, "web.internal"), None);
    assert_eq!(find_metadata_alias(&doc, "203.0.113.11"), None);
}

// --- muxKey ---

#[test]
fn shares_transports_across_keepalive_policies() {
    let f = Fixture::new();
    let without = mux_key(&chain(&f, "", p("web")), false, false);
    let with = mux_key(
        &chain(
            &f,
            "",
            SshProfile {
                keepalive_interval_seconds: Some(30),
                ..p("web")
            },
        ),
        false,
        false,
    );
    assert_eq!(with, without);
}

#[test]
fn matches_when_different_targets_resolve_to_the_same_dial_plan() {
    let f = Fixture::new();
    let content = lines(&[
        "Host web web-alias",
        "  HostName web.example.com",
        "  User deploy",
        "  Port 2222",
    ]);
    let direct = mux_key(&chain(&f, &content, p("web")), false, false);
    let aliased = mux_key(&chain(&f, &content, p("web-alias")), false, false);
    assert_eq!(direct, "deploy@web.example.com:2222;agentForward=no");
    assert_eq!(aliased, direct);
}

#[test]
fn separates_plans_that_differ_in_user_port_or_jump_chain() {
    let f = Fixture::new();
    let content = lines(&[
        "Host app",
        "  HostName app.internal",
        "  ProxyJump bastion",
        "",
        "Host bastion",
        "  HostName bastion.example.com",
    ]);
    let via_jump = mux_key(&chain(&f, &content, p("app")), false, false);
    let other_user = mux_key(
        &chain(
            &f,
            &content,
            SshProfile {
                user: Some("admin".into()),
                ..p("app")
            },
        ),
        false,
        false,
    );
    let other_port = mux_key(
        &chain(
            &f,
            &content,
            SshProfile {
                port: Some(2200),
                ..p("app")
            },
        ),
        false,
        false,
    );
    assert!(via_jump.contains("bastion.example.com:22;agentForward=no -> "));
    let set: std::collections::HashSet<_> =
        [via_jump, other_user, other_port].into_iter().collect();
    assert_eq!(set.len(), 3);
}

#[test]
fn includes_the_expanded_proxy_command_transport_in_the_plan_identity() {
    let f = Fixture::new();
    let content = lines(&[
        "Host tunneled tunneled-alias",
        "  HostName inner.example.com",
        "  ProxyCommand route --alias %n --host %h --port %p",
    ]);
    let plain = mux_key(
        &chain(
            &f,
            "Host tunneled\n  HostName inner.example.com",
            p("tunneled"),
        ),
        false,
        false,
    );
    let proxied = mux_key(&chain(&f, &content, p("tunneled")), false, false);
    let aliased = mux_key(&chain(&f, &content, p("tunneled-alias")), false, false);
    assert!(proxied.contains("proxy(route --alias tunneled --host inner.example.com --port 22)"));
    assert_ne!(proxied, plain);
    assert_ne!(aliased, proxied);
}

#[test]
fn separates_plans_by_effective_agent_forwarding_policy() {
    let f = Fixture::new();
    let content = lines(&[
        "Host no-agent",
        "  HostName app.example.com",
        "  ForwardAgent no",
        "",
        "Host with-agent",
        "  HostName app.example.com",
        "  ForwardAgent yes",
    ]);
    let no_agent = mux_key(&chain(&f, &content, p("no-agent")), false, false);
    let with_agent = mux_key(&chain(&f, &content, p("with-agent")), false, false);
    assert!(no_agent.contains("agentForward=no"));
    assert!(with_agent.contains("agentForward=yes"));
    assert_ne!(with_agent, no_agent);
    assert!(
        mux_key(&chain(&f, &content, p("no-agent")), true, true)
            .ends_with(";muxusSftp=disabled;muxusConsole=compatible")
    );
}

// --- folder-auth.test.ts: buildChain with folder defaults ---

fn folder_defaults(
    user: Option<&str>,
    port: Option<u16>,
    account: &str,
    label: &str,
) -> FolderAuthDefaults {
    FolderAuthDefaults {
        option_lines: folder_auth_option_lines(&FolderAuthSettings {
            user: user.map(str::to_string),
            port,
            ..Default::default()
        }),
        passwords: vec![FolderPasswordRef {
            account: account.into(),
            label: label.into(),
        }],
    }
}

#[test]
fn applies_each_hop_its_own_folder_and_passwords_ride_on_the_hop() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host app",
        "  HostName app.internal",
        "  ProxyJump bastion",
        "",
        "Host bastion",
        "  HostName bastion.example.com",
    ]));
    let lookup = |alias: &str| match alias {
        "app" => Some(folder_defaults(
            Some("appuser"),
            None,
            "acct-app",
            "Folder app",
        )),
        "bastion" => Some(folder_defaults(
            Some("jumpuser"),
            Some(2200),
            "acct-bastion",
            "Folder bastion",
        )),
        _ => None,
    };
    let c = build_chain(
        &doc,
        &p("app"),
        ChainSources {
            folder_auth: Some(&lookup),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((c[0].user.as_str(), c[0].port), ("jumpuser", 2200));
    assert_eq!((c[1].user.as_str(), c[1].port), ("appuser", 22));
    assert_eq!(
        c[1].folder_passwords,
        Some(vec![FolderPasswordRef {
            account: "acct-app".into(),
            label: "Folder app".into()
        }])
    );
}

#[test]
fn keeps_per_connection_overrides_above_folder_defaults() {
    let f = Fixture::new();
    let doc = f.doc("Host web\n  HostName web.example.com");
    let lookup = |_: &str| Some(folder_defaults(Some("folderuser"), None, "a", "b"));
    let c = build_chain(
        &doc,
        &SshProfile {
            user: Some("override".into()),
            ..p("web")
        },
        ChainSources {
            folder_auth: Some(&lookup),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(c[0].user, "override");
}

#[test]
fn ignores_folder_defaults_for_self_contained_dials() {
    let f = Fixture::new();
    let doc = f.doc("Host web\n  HostName web.example.com");
    let lookup = |_: &str| {
        Some(folder_defaults(
            Some("folderuser"),
            Some(2222),
            "acct",
            "Folder",
        ))
    };
    let c = build_chain(
        &doc,
        &SshProfile {
            use_config: Some(false),
            ..p("web")
        },
        ChainSources {
            folder_auth: Some(&lookup),
            ..Default::default()
        },
    )
    .unwrap();
    assert_ne!(c[0].user, "folderuser");
    assert_eq!(c[0].port, 22);
    assert_eq!(c[0].folder_passwords, None);
}

#[test]
fn applies_folder_defaults_to_saved_muxus_profiles_without_reading_ssh_config() {
    let f = Fixture::new();
    let doc = f.doc("Host db.example.test\n  User config-user\n  Port 2200");
    let lookup = |_: &str| {
        Some(FolderAuthDefaults {
            option_lines: folder_auth_option_lines(&FolderAuthSettings {
                user: Some("folder-user".into()),
                port: Some(2222),
                identity_files: Some(vec!["~/.ssh/folder_key".into()]),
                ..Default::default()
            }),
            passwords: vec![FolderPasswordRef {
                account: "acct".into(),
                label: "Folder".into(),
            }],
        })
    };
    let c = build_chain(
        &doc,
        &SshProfile {
            use_config: Some(false),
            profile_id: Some("saved-1".into()),
            ..p("db.example.test")
        },
        ChainSources {
            profile_folder_auth: Some(&lookup),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((c[0].user.as_str(), c[0].port), ("folder-user", 2222));
    assert_eq!(
        c[0].folder_passwords,
        Some(vec![FolderPasswordRef {
            account: "acct".into(),
            label: "Folder".into()
        }])
    );
    assert_eq!(c[0].resolved.hostname, "db.example.test");
    assert!(c[0].resolved.identity_files[0].ends_with("/folder_key"));
    assert_eq!(c[0].resolved.identity_files.len(), 1);
}
