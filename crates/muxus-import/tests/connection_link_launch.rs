//! Port of `tests/unit/client/connection-link-launch.test.ts`.

use muxus_core::api::{
    OpenSshProfileMetadata, ResolvedHostSettings, SavedHostProfile, SshHostEntry,
};
use muxus_core::connection_links::{ConnectionLink, parse_connection_link};
use muxus_core::protocol::{SavedHostSessionProfile, SessionProfile, SshProfile, TelnetProfile};
use muxus_import::launch::{ConnectionLinkLaunch, ManagedHost, resolve_connection_link};

const FINGERPRINT: &str = "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8";

fn link(url: &str) -> ConnectionLink {
    parse_connection_link(url).unwrap_or_else(|error| panic!("{url}: {error}"))
}

fn ssh_host(
    aliases: &[&str],
    hostname: &str,
    port: Option<u16>,
    user: Option<&str>,
    display_name: Option<&str>,
) -> SshHostEntry {
    SshHostEntry {
        alias: aliases[0].to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        description: None,
        file: "/home/test/.ssh/config".to_owned(),
        options: Default::default(),
        resolved: ResolvedHostSettings {
            hostname: hostname.to_owned(),
            port: port.unwrap_or(22),
            user: user.map(str::to_owned),
            ..ResolvedHostSettings::default()
        },
        metadata: display_name.map(|display_name| OpenSshProfileMetadata {
            profile_id: format!("ssh-{}", aliases[0]),
            display_name: Some(display_name.to_owned()),
            ..OpenSshProfileMetadata::default()
        }),
    }
}

fn saved(id: &str, name: &str, profile: SavedHostSessionProfile) -> SavedHostProfile {
    SavedHostProfile {
        id: id.to_owned(),
        kind: profile.kind(),
        name: name.to_owned(),
        profile,
        metadata: OpenSshProfileMetadata {
            profile_id: id.to_owned(),
            ..OpenSshProfileMetadata::default()
        },
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

struct Fixture {
    router: SshHostEntry,
    router_console: SshHostEntry,
    lab: SavedHostProfile,
    legacy: SavedHostProfile,
}

impl Fixture {
    fn new() -> Self {
        Self {
            router: ssh_host(
                &["edge-router", "edge"],
                "10.0.0.1",
                None,
                Some("admin"),
                Some("Edge router"),
            ),
            router_console: ssh_host(
                &["edge-console"],
                "10.0.0.1",
                Some(2222),
                Some("console"),
                None,
            ),
            lab: saved(
                "lab-1",
                "lab-switch",
                SavedHostSessionProfile::Ssh(SshProfile {
                    user: Some("netops".to_owned()),
                    port: Some(830),
                    use_config: Some(false),
                    ..SshProfile::new("192.0.2.10")
                }),
            ),
            legacy: saved(
                "telnet-1",
                "legacy-switch",
                SavedHostSessionProfile::Telnet(TelnetProfile {
                    profile_id: None,
                    host: "192.0.2.20".to_owned(),
                    port: 2323,
                }),
            ),
        }
    }

    fn ssh_hosts(&self) -> Vec<SshHostEntry> {
        vec![self.router.clone(), self.router_console.clone()]
    }

    fn profiles(&self) -> Vec<SavedHostProfile> {
        vec![self.lab.clone(), self.legacy.clone()]
    }
}

fn ssh_profile(target: &str, fingerprint: Option<&str>) -> SessionProfile {
    SessionProfile::Ssh(SshProfile {
        host_key_fingerprint: fingerprint.map(str::to_owned),
        ..SshProfile::new(target)
    })
}

/// The launch's host entry and SSH target, for compact assertions.
fn host_target<'a>(launch: &'a ConnectionLinkLaunch<'_>) -> (Option<String>, Option<&'a str>) {
    match launch {
        ConnectionLinkLaunch::Host { host, profile } => (
            Some(host.key()),
            match profile {
                SessionProfile::Ssh(ssh) => Some(ssh.target.as_str()),
                _ => None,
            },
        ),
        ConnectionLinkLaunch::AdHoc { .. } => (None, None),
    }
}

fn is_ad_hoc(launch: &ConnectionLinkLaunch<'_>) -> bool {
    matches!(launch, ConnectionLinkLaunch::AdHoc { .. })
}

#[test]
fn uses_an_ssh_config_host_named_by_its_alias_with_the_links_user_and_port_winning() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    assert_eq!(
        resolve_connection_link(&link("ssh://edge-router"), &hosts, &profiles),
        ConnectionLinkLaunch::Host {
            host: ManagedHost::Ssh(&hosts[0]),
            profile: ssh_profile("edge-router", None),
        }
    );
    let launch = resolve_connection_link(&link("ssh://root@EDGE:2200"), &hosts, &profiles);
    assert_eq!(
        host_target(&launch),
        (Some("ssh:edge-router".to_owned()), Some("root@edge:2200"))
    );
}

#[test]
fn matches_an_ssh_config_host_by_host_name_and_port() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    let launch = resolve_connection_link(&link("ssh://admin@10.0.0.1"), &hosts, &profiles);
    assert_eq!(
        host_target(&launch),
        (Some("ssh:edge-router".to_owned()), Some("edge-router"))
    );
    let launch = resolve_connection_link(&link("ssh://10.0.0.1:2222"), &hosts, &profiles);
    assert_eq!(
        host_target(&launch),
        (Some("ssh:edge-console".to_owned()), Some("edge-console"))
    );
}

#[test]
fn uses_a_saved_muxus_host_by_name_or_address_when_user_and_port_fit() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    let mut expected_profile: SessionProfile = fixture.lab.profile.clone().into();
    if let SessionProfile::Ssh(ssh) = &mut expected_profile {
        ssh.profile_id = Some("lab-1".to_owned());
    }
    let expected = ConnectionLinkLaunch::Host {
        host: ManagedHost::Profile(&profiles[0]),
        profile: expected_profile,
    };
    assert_eq!(
        resolve_connection_link(&link("ssh://lab-switch"), &hosts, &profiles),
        expected
    );
    assert_eq!(
        resolve_connection_link(&link("ssh://netops@192.0.2.10:830"), &hosts, &profiles),
        expected
    );
}

#[test]
fn dials_a_saved_muxus_hosts_address_ad_hoc_when_the_link_asks_for_another_user_or_port() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    assert_eq!(
        resolve_connection_link(&link("ssh://root@192.0.2.10:830"), &hosts, &profiles),
        ConnectionLinkLaunch::AdHoc {
            profile: ssh_profile("root@192.0.2.10:830", None),
            title: "root@192.0.2.10:830".to_owned(),
        }
    );
    assert!(is_ad_hoc(&resolve_connection_link(
        &link("ssh://192.0.2.10"),
        &hosts,
        &profiles
    )));
}

#[test]
fn connects_like_quick_connect_when_nothing_matches() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    assert_eq!(
        resolve_connection_link(&link("ssh://admin@10.9.9.9:2222"), &hosts, &profiles),
        ConnectionLinkLaunch::AdHoc {
            profile: ssh_profile("admin@10.9.9.9:2222", None),
            title: "admin@10.9.9.9:2222".to_owned(),
        }
    );
}

#[test]
fn narrows_several_address_matches_by_user_and_otherwise_does_not_guess() {
    let fixture = Fixture::new();
    let second = ssh_host(&["edge-ro"], "10.0.0.1", None, Some("readonly"), None);
    let hosts = vec![fixture.router.clone(), second];
    let launch = resolve_connection_link(&link("ssh://readonly@10.0.0.1"), &hosts, &[]);
    assert_eq!(
        host_target(&launch),
        (Some("ssh:edge-ro".to_owned()), Some("edge-ro"))
    );
    assert!(is_ad_hoc(&resolve_connection_link(
        &link("ssh://10.0.0.1"),
        &hosts,
        &[]
    )));
    assert!(is_ad_hoc(&resolve_connection_link(
        &link("ssh://bob@10.0.0.1"),
        &hosts,
        &[]
    )));
}

#[test]
fn prefers_an_alias_over_an_address_match() {
    let fixture = Fixture::new();
    let named = ssh_host(&["10.0.0.1"], "10.0.0.99", None, None, None);
    let hosts = vec![fixture.router.clone(), named];
    let launch = resolve_connection_link(&link("ssh://10.0.0.1"), &hosts, &[]);
    assert_eq!(host_target(&launch).0.as_deref(), Some("ssh:10.0.0.1"));
}

#[test]
fn carries_a_fingerprint_into_every_kind_of_ssh_launch() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    let url = |address: &str| format!("ssh://;fingerprint={FINGERPRINT}@{address}");
    let fingerprint_of = |launch: &ConnectionLinkLaunch<'_>| match launch {
        ConnectionLinkLaunch::Host {
            profile: SessionProfile::Ssh(ssh),
            ..
        }
        | ConnectionLinkLaunch::AdHoc {
            profile: SessionProfile::Ssh(ssh),
            ..
        } => (
            ssh.target.clone(),
            ssh.profile_id.clone(),
            ssh.host_key_fingerprint.clone(),
        ),
        other => panic!("not SSH: {other:?}"),
    };

    let alias = resolve_connection_link(&link(&url("edge")), &hosts, &profiles);
    assert_eq!(
        fingerprint_of(&alias),
        ("edge".to_owned(), None, Some(FINGERPRINT.to_owned()))
    );
    let saved = resolve_connection_link(&link(&url("lab-switch")), &hosts, &profiles);
    assert_eq!(
        fingerprint_of(&saved),
        (
            "192.0.2.10".to_owned(),
            Some("lab-1".to_owned()),
            Some(FINGERPRINT.to_owned())
        )
    );
    let ad_hoc = resolve_connection_link(&link(&url("10.9.9.9")), &hosts, &profiles);
    assert!(is_ad_hoc(&ad_hoc));
    assert_eq!(
        fingerprint_of(&ad_hoc),
        ("10.9.9.9".to_owned(), None, Some(FINGERPRINT.to_owned()))
    );
}

#[test]
fn uses_a_saved_telnet_host_by_name_or_address_and_port() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    let expected = ConnectionLinkLaunch::Host {
        host: ManagedHost::Profile(&profiles[1]),
        profile: SessionProfile::Telnet(TelnetProfile {
            profile_id: Some("telnet-1".to_owned()),
            host: "192.0.2.20".to_owned(),
            port: 2323,
        }),
    };
    assert_eq!(
        resolve_connection_link(&link("telnet://legacy-switch"), &hosts, &profiles),
        expected
    );
    assert_eq!(
        resolve_connection_link(&link("telnet://192.0.2.20:2323"), &hosts, &profiles),
        expected
    );
}

#[test]
fn opens_other_addresses_as_a_new_telnet_session_on_port_23_by_default() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    assert_eq!(
        resolve_connection_link(&link("telnet://192.0.2.20"), &hosts, &profiles),
        ConnectionLinkLaunch::AdHoc {
            profile: SessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: "192.0.2.20".to_owned(),
                port: 23,
            }),
            title: "192.0.2.20".to_owned(),
        }
    );
    assert_eq!(
        resolve_connection_link(&link("telnet://[2001:db8::7]:2323"), &hosts, &profiles),
        ConnectionLinkLaunch::AdHoc {
            profile: SessionProfile::Telnet(TelnetProfile {
                profile_id: None,
                host: "2001:db8::7".to_owned(),
                port: 2323,
            }),
            title: "[2001:db8::7]:2323".to_owned(),
        }
    );
}

#[test]
fn never_opens_an_ssh_host_for_a_telnet_link() {
    let fixture = Fixture::new();
    let (hosts, profiles) = (fixture.ssh_hosts(), fixture.profiles());
    assert!(is_ad_hoc(&resolve_connection_link(
        &link("telnet://edge-router"),
        &hosts,
        &profiles
    )));
    assert!(is_ad_hoc(&resolve_connection_link(
        &link("ssh://legacy-switch"),
        &hosts,
        &profiles
    )));
}
