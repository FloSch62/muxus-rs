//! Port of `tests/unit/client/shared-sessions.test.ts` and
//! `tests/unit/client/shared-sessions-export.test.ts`.

use std::collections::{HashMap, HashSet};

use muxus_core::api::{FolderSettingsRecord, SavedHostProfile, SshHostEntry, saved_host_hop};
use muxus_import::launch::ManagedHost;
use muxus_import::{
    ShareFolderOptions, SharedListHost, SharedSessionsFormat, SharedSessionsList,
    parse_shared_sessions, share_folder, shared_sessions_filename,
};
use serde_json::{Value, json};

const LINK: &str = "lab-link";
const FOLDER: &str = "Lab";

// One entry as a lab portal publishes it for MobaXterm's shared sessions.
const PORTAL_FILE: &str = "[Bookmarks_1]
SubRep=clab
ImgNum=41
node0 [sudo]=#196#0%node0.lab.example.net%22%alice%%-1%-1%%%%%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15%236,236,236%30,30,30%180,180,192%0%-1%0%%xterm%-1%0%_Std_Colors_0_%80%24%0%1%-1%<none>%%0%0%-1%-1#0# clab, sudo #-1
";

fn parse(content: &str) -> SharedSessionsList {
    parse_shared_sessions(content, LINK, FOLDER).unwrap()
}

fn named<'a>(list: &'a SharedSessionsList, name: &str) -> &'a SharedListHost {
    list.hosts
        .iter()
        .find(|host| host.name == name)
        .unwrap_or_else(|| panic!("no host named {name}"))
}

fn profile(host: &SharedListHost) -> Value {
    Value::Object(host.profile.clone())
}

fn names(list: &SharedSessionsList) -> Vec<&str> {
    list.hosts.iter().map(|host| host.name.as_str()).collect()
}

mod from_a_mobaxterm_file {
    use super::*;

    #[test]
    fn reads_a_portal_entry_into_the_link_folder_with_the_user_filled_in() {
        let list = parse(PORTAL_FILE);

        assert_eq!(list.format, SharedSessionsFormat::MobaXterm);
        assert_eq!(list.skipped, 0);
        assert_eq!(list.hosts.len(), 1);
        let host = &list.hosts[0];
        assert!(host.id.starts_with("shared-"));
        assert_eq!(host.name, "node0 [sudo]");
        assert_eq!(host.group.as_deref(), Some("Lab/clab"));
        assert_eq!((host.color.as_deref(), host.icon.as_deref()), (None, None));
        assert_eq!(
            profile(host),
            json!({
                "kind": "ssh",
                "target": "node0.lab.example.net",
                "useConfig": false,
                "user": "alice",
                "passwordOnly": true,
            })
        );
    }

    #[test]
    fn keeps_an_entry_on_the_same_host_across_checks_and_apart_between_links() {
        let first = parse(PORTAL_FILE).hosts[0].id.clone();
        // The portal filled in more settings; it is still the same entry.
        let again = parse(&PORTAL_FILE.replace("%22%alice", "%2222%bob")).hosts[0]
            .id
            .clone();
        let other_link = parse_shared_sessions(PORTAL_FILE, "other-link", FOLDER)
            .unwrap()
            .hosts[0]
            .id
            .clone();

        assert_eq!(again, first);
        assert_ne!(other_link, first);
    }

    #[test]
    fn brings_ssh_gateways_along_as_jump_hosts_inside_the_link_folder() {
        let list = parse(
            "[Bookmarks]
SubRep=
Inner=#109#0%10.0.1.5%22%root%%-1%-1%%bastion.example.com%2222%jump%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Desk=#91#4%10.0.1.9%3389%admin%0%-1%-1%0%0%0%0%0%-1%bastion.example.com%2222%jump%0%0%%-1
",
        );

        let jump = named(&list, "jump@bastion.example.com:2222");
        assert_eq!(jump.group.as_deref(), Some("Lab/Jump hosts"));
        let jump_profile = profile(jump);
        assert_eq!(jump_profile["kind"], "ssh");
        assert_eq!(jump_profile["target"], "bastion.example.com");
        assert_eq!(jump_profile["port"], 2222);
        assert_eq!(jump_profile["user"], "jump");
        assert_eq!(jump_profile["useConfig"], false);

        let inner = named(&list, "Inner");
        assert_eq!(inner.group.as_deref(), Some("Lab"));
        assert_eq!(
            profile(inner)["proxyJump"],
            json!([saved_host_hop(&jump.id)])
        );
        let desk = profile(named(&list, "Desk"));
        assert_eq!(desk["kind"], "rdp");
        assert_eq!(
            desk["sshGateway"],
            json!({ "target": "bastion.example.com", "profileId": jump.id })
        );
        // Every host is one of this link's: nothing keeps an ID a plain import would also use.
        assert!(list.hosts.iter().all(|host| host.id.starts_with("shared-")));
    }

    #[test]
    fn counts_sessions_muxus_does_not_import() {
        let list = parse(
            "[Bookmarks]
SubRep=
Box=#109#0%box.example.com%22%root%%rest
Files=#130#6%ftp.example.com%21%anonymous%rest
",
        );

        assert_eq!(names(&list), ["Box"]);
        assert_eq!(list.skipped, 1);
    }
}

mod from_a_muxus_backup {
    use super::*;

    #[test]
    fn needs_only_the_format_and_the_hosts() {
        let list = parse(
            &json!({
                "format": "muxus-backup",
                "data": {
                    "savedHosts": [
                        {
                            "name": "Core router",
                            "profile": { "kind": "ssh", "target": "core.example.net", "user": "admin" },
                            "metadata": {
                                "group": "Network/Core", "color": "#ff8800", "icon": "router",
                                "loginSequence": { "steps": [] },
                            },
                        },
                        {
                            "name": "Console",
                            "profile": { "kind": "telnet", "host": "console.example.net", "port": 2001 },
                        },
                    ],
                },
            })
            .to_string(),
        );

        assert_eq!(list.format, SharedSessionsFormat::Muxus);
        assert_eq!(list.skipped, 0);
        assert_eq!(list.hosts.len(), 2);
        let [router, console] = &list.hosts[..] else {
            unreachable!()
        };
        assert!(router.id.starts_with("shared-"));
        assert_eq!(router.name, "Core router");
        assert_eq!(router.group.as_deref(), Some("Lab/Network/Core"));
        assert_eq!(router.color.as_deref(), Some("#ff8800"));
        assert_eq!(router.icon.as_deref(), Some("router"));
        assert_eq!(
            profile(router),
            json!({ "kind": "ssh", "target": "core.example.net", "user": "admin", "useConfig": false })
        );
        assert!(console.id.starts_with("shared-"));
        assert_eq!(console.name, "Console");
        assert_eq!(console.group.as_deref(), Some("Lab"));
        assert_eq!(
            (console.color.as_deref(), console.icon.as_deref()),
            (None, None)
        );
        assert_eq!(
            profile(console),
            json!({ "kind": "telnet", "host": "console.example.net", "port": 2001 })
        );
        // The entry is what the backend takes to apply: absent settings are left out.
        assert_eq!(
            serde_json::to_value(console).unwrap(),
            json!({
                "id": console.id,
                "name": "Console",
                "profile": { "kind": "telnet", "host": "console.example.net", "port": 2001 },
                "group": "Lab",
            })
        );
    }

    #[test]
    fn points_jump_hosts_and_gateways_at_the_hosts_the_list_brought() {
        let list = parse(
            &json!({
                "format": "muxus-backup",
                "version": 2,
                "data": {
                    "savedHosts": [
                        {
                            "id": "bastion", "name": "Bastion",
                            "profile": {
                                "kind": "ssh", "target": "bastion.example.net",
                                "useConfig": false, "profileId": "bastion",
                            },
                            "metadata": {},
                        },
                        {
                            "id": "inner", "name": "Inner",
                            "profile": {
                                "kind": "ssh", "target": "10.0.0.5", "useConfig": false,
                                "proxyJump": [saved_host_hop("bastion")],
                                "proxyJumpFallbacks": [[saved_host_hop("bastion"), "elsewhere"]],
                            },
                            "metadata": {},
                        },
                        {
                            "id": "desk", "name": "Desk",
                            "profile": {
                                "kind": "vnc", "host": "10.0.0.9", "port": 5900,
                                "sshGateway": { "target": "bastion.example.net", "profileId": "bastion" },
                            },
                            "metadata": {},
                        },
                        {
                            "id": "outside", "name": "Outside",
                            "profile": {
                                "kind": "ssh", "target": "10.0.0.6",
                                "proxyJump": [saved_host_hop("not-in-the-list")],
                            },
                            "metadata": {},
                        },
                    ],
                },
            })
            .to_string(),
        );
        let [bastion, inner, desk, outside] = &list.hosts[..] else {
            unreachable!()
        };

        assert_ne!(bastion.id, "bastion");
        assert_eq!(
            profile(bastion),
            json!({ "kind": "ssh", "target": "bastion.example.net", "useConfig": false })
        );
        assert_eq!(
            profile(inner)["proxyJump"],
            json!([saved_host_hop(&bastion.id)])
        );
        assert_eq!(
            profile(inner)["proxyJumpFallbacks"],
            json!([[saved_host_hop(&bastion.id), "elsewhere"]])
        );
        assert_eq!(
            profile(desk)["sshGateway"],
            json!({ "target": "bastion.example.net", "profileId": bastion.id })
        );
        // A hop the list does not bring is left as written and fails when dialed.
        assert_eq!(
            profile(outside)["proxyJump"],
            json!([saved_host_hop("not-in-the-list")])
        );
    }

    #[test]
    fn turns_the_ssh_config_hosts_of_a_backup_into_hosts_of_their_own() {
        let list = parse(
            &json!({
                "format": "muxus-backup",
                "version": 2,
                "data": {
                    "sshHosts": [
                        {
                            "alias": "jump", "aliases": ["jump", "gw"],
                            "options": { "hostname": "jump.example.net", "user": "ops", "port": 2222 },
                        },
                        {
                            "alias": "db", "aliases": ["db"],
                            "options": {
                                "hostname": "10.1.0.7",
                                "proxyJump": ["gw", "user@literal.example.net"],
                                "identityFiles": ["~/.ssh/lab"],
                                "extras": [{ "keyword": "Compression", "value": "yes" }],
                            },
                            "metadata": { "displayName": "Database", "group": "Data" },
                        },
                    ],
                    "savedHosts": [
                        {
                            "id": "ui", "name": "Lab UI",
                            "profile": {
                                "kind": "web", "url": "https://ui.lab.example.net",
                                "sshGateway": { "target": "jump" },
                            },
                            "metadata": {},
                        },
                    ],
                },
            })
            .to_string(),
        );
        let jump = named(&list, "jump");

        assert_eq!(
            profile(jump),
            json!({
                "kind": "ssh", "target": "jump.example.net", "useConfig": false,
                "user": "ops", "port": 2222,
            })
        );
        let database = named(&list, "Database");
        assert!(database.id.starts_with("shared-"));
        assert_eq!(database.group.as_deref(), Some("Lab/Data"));
        assert_eq!(
            profile(database),
            json!({
                "kind": "ssh", "target": "10.1.0.7", "useConfig": false,
                "identityFiles": ["~/.ssh/lab"],
                "proxyJump": [saved_host_hop(&jump.id), "user@literal.example.net"],
            })
        );
        assert_eq!(
            profile(named(&list, "Lab UI"))["sshGateway"],
            json!({ "target": "jump.example.net", "profileId": jump.id })
        );
    }

    #[test]
    fn leaves_out_entries_that_are_not_hosts_and_tells_same_named_ones_apart() {
        let list = parse(
            &json!({
                "format": "muxus-backup",
                "data": {
                    "savedHosts": [
                        { "name": "Twin", "profile": { "kind": "telnet", "host": "a.example.net" } },
                        { "name": "Twin", "profile": { "kind": "telnet", "host": "b.example.net" } },
                        { "name": "Shell", "profile": { "kind": "local" } },
                        { "profile": { "kind": "telnet", "host": "nameless.example.net" } },
                        "nonsense",
                    ],
                },
            })
            .to_string(),
        );

        assert_eq!(names(&list), ["Twin", "Twin"]);
        let ids: HashSet<&str> = list.hosts.iter().map(|host| host.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(list.skipped, 3);
    }
}

#[test]
fn refuses_what_cannot_be_used() {
    let cases = [
        ("", "The link returned nothing."),
        (
            "<!DOCTYPE html><html><body>Sign in</body></html>",
            "The link returned a web page instead of a session list. It may need a sign-in.",
        ),
        (
            "not found",
            "The link returned neither a MobaXterm session file nor a Muxus session list.",
        ),
        (
            r#"{"format":"something-else","data":{}}"#,
            "The link returned JSON that is neither a Muxus session list nor a Muxus backup.",
        ),
        (
            r#"{"format":"#,
            "The link returned JSON that cannot be read.",
        ),
        (
            r#"{"format":"muxus-backup","data":{"savedHosts":[]}}"#,
            "The shared sessions list has no hosts.",
        ),
        (
            "[Bookmarks]\nSubRep=\n",
            "No SSH, RDP or VNC sessions were found in this MobaXterm file.",
        ),
    ];
    for (content, message) in cases {
        let error = parse_shared_sessions(content, LINK, FOLDER).unwrap_err();
        assert_eq!(error.to_string(), message, "for {content:?}");
    }
}

#[test]
fn reads_a_list_that_starts_with_a_byte_order_mark() {
    let list = parse(&format!("\u{feff}{PORTAL_FILE}"));
    assert_eq!(names(&list), ["node0 [sudo]"]);
}

const NOW: &str = "2026-11-20T10:00:00.000Z";
const ALL: ShareFolderOptions = ShareFolderOptions {
    include_users: true,
    include_key_paths: true,
};

/// The sidebar's hosts, from both sources, in the order the sidebar has them.
enum Host {
    Saved(SavedHostProfile),
    Configured(SshHostEntry),
}

fn managed(hosts: &[Host]) -> Vec<ManagedHost<'_>> {
    hosts
        .iter()
        .map(|host| match host {
            Host::Saved(entry) => ManagedHost::Profile(entry),
            Host::Configured(entry) => ManagedHost::Ssh(entry),
        })
        .collect()
}

fn merged(base: Value, overrides: Value) -> Value {
    let mut merged = base.as_object().unwrap().clone();
    merged.extend(overrides.as_object().unwrap().clone());
    Value::Object(merged)
}

fn saved(id: &str, profile: Value, metadata: Value) -> Host {
    Host::Saved(
        serde_json::from_value(json!({
            "id": id,
            "kind": profile["kind"],
            "name": id,
            "profile": merged(profile.clone(), json!({ "profileId": id })),
            "metadata": merged(json!({ "profileId": id, "connectCount": 0 }), metadata),
            "createdAt": "",
            "updatedAt": "",
        }))
        .unwrap(),
    )
}

fn configured(alias: &str, resolved: Value, metadata: Value, options: Value) -> Host {
    Host::Configured(
        serde_json::from_value(json!({
            "alias": alias,
            "aliases": [alias],
            "file": "/home/me/.ssh/config",
            "options": options,
            "resolved": merged(
                json!({
                    "hostname": alias, "port": 22, "identityFiles": [], "certificateFiles": [],
                    "identitiesOnly": false, "forwardAgent": false, "proxyJump": [],
                    "forwards": [], "passwordOnly": false,
                }),
                resolved,
            ),
            "metadata": merged(
                json!({ "profileId": format!("meta-{alias}"), "connectCount": 0 }),
                metadata,
            ),
        }))
        .unwrap(),
    )
}

fn sidebar() -> Vec<Host> {
    vec![
        saved(
            "leaf",
            json!({
                "kind": "ssh", "target": "leaf-01.lab.example.net", "useConfig": false,
                "user": "admin", "identityFiles": ["~/.ssh/lab"],
                "proxyJump": [saved_host_hop("bastion")],
            }),
            json!({ "group": "Lab/Fabric", "color": "#a855f7", "icon": "router" }),
        ),
        saved(
            "console",
            json!({ "kind": "telnet", "host": "console.lab.example.net", "port": 2001 }),
            json!({ "group": "Lab" }),
        ),
        saved(
            "desk",
            json!({
                "kind": "vnc", "host": "10.0.0.9", "port": 5900, "username": "me",
                "sshGateway": { "target": "gw" },
            }),
            json!({ "group": "lab/Desktops" }),
        ),
        configured(
            "build",
            json!({
                "hostname": "build-01.lab.example.net", "user": "ci", "port": 2222,
                "proxyJump": ["gw"], "forwardAgent": true, "proxyCommand": "nc %h %p",
            }),
            json!({ "group": "Lab", "displayName": "Build box" }),
            json!({ "remoteCommand": "tmux attach", "strictHostKeyChecking": "no" }),
        ),
        // Outside the folder: only what the folder's hosts go through comes along.
        saved(
            "bastion",
            json!({
                "kind": "ssh", "target": "bastion.example.net", "useConfig": false,
                "user": "jump", "proxyJump": ["gw"],
            }),
            json!({ "group": "Infra" }),
        ),
        configured(
            "gw",
            json!({ "hostname": "gw.example.net", "user": "ops" }),
            json!({ "group": "Infra" }),
            json!({}),
        ),
        saved(
            "other",
            json!({ "kind": "ssh", "target": "other.example.net", "useConfig": false }),
            json!({ "group": "Production" }),
        ),
        saved(
            "labrador",
            json!({ "kind": "ssh", "target": "dog.example.net", "useConfig": false }),
            json!({ "group": "Labrador" }),
        ),
    ]
}

mod sharing_a_folder_as_a_session_list {
    use super::*;

    #[test]
    fn lists_the_folders_hosts_with_the_folders_they_sit_in() {
        let hosts = sidebar();
        let shared = share_folder("Lab", &managed(&hosts), &[], ALL, NOW);

        assert_eq!(shared.host_count, 4);
        assert_eq!(shared.jump_host_count, 2);
        let document = &shared.document;
        assert_eq!(document.format, "muxus-shared-sessions");
        assert_eq!(document.version, 1);
        assert_eq!(document.name, "Lab");
        assert_eq!(document.created_at, "2026-11-20T10:00:00.000Z");
        let placed: Vec<(&str, Option<&str>)> = document
            .hosts
            .iter()
            .map(|host| (host.name.as_str(), host.folder.as_deref()))
            .collect();
        assert_eq!(
            placed,
            [
                ("leaf", Some("Fabric")),
                ("console", None),
                ("desk", Some("Desktops")),
                ("Build box", None),
                ("bastion", Some("Jump hosts")),
                ("gw", Some("Jump hosts")),
            ]
        );
        assert_eq!(
            serde_json::to_value(&document.hosts[0]).unwrap(),
            json!({
                "id": "leaf",
                "name": "leaf",
                "folder": "Fabric",
                "color": "#a855f7",
                "icon": "router",
                "profile": {
                    "kind": "ssh",
                    "target": "leaf-01.lab.example.net",
                    "useConfig": false,
                    "user": "admin",
                    "identityFiles": ["~/.ssh/lab"],
                    "proxyJump": [saved_host_hop("bastion")],
                },
            })
        );
        // The file reads top down: what it is, then the hosts.
        let written = serde_json::to_string(document).unwrap();
        assert!(written.starts_with(
            r#"{"format":"muxus-shared-sessions","version":1,"name":"Lab","createdAt":"2026-11-20T10:00:00.000Z","hosts":[{"id":"leaf","name":"leaf","folder":"Fabric","#
        ));
    }

    #[test]
    fn writes_ssh_config_hosts_with_what_they_resolve_to_and_names_every_hop_by_its_entry() {
        let hosts = sidebar();
        let document = share_folder("Lab", &managed(&hosts), &[], ALL, NOW).document;
        let by_id: HashMap<&str, Value> = document
            .hosts
            .iter()
            .map(|host| (host.id.as_str(), Value::Object(host.profile.clone())))
            .collect();

        assert_eq!(
            by_id["ssh:build"],
            json!({
                "kind": "ssh",
                "target": "build-01.lab.example.net",
                "useConfig": false,
                "user": "ci",
                "port": 2222,
                "proxyJump": [saved_host_hop("ssh:gw")],
                "remoteCommand": "tmux attach",
            })
        );
        assert_eq!(
            by_id["bastion"]["proxyJump"],
            json!([saved_host_hop("ssh:gw")])
        );
        assert_eq!(
            by_id["desk"]["sshGateway"],
            json!({ "target": "gw.example.net", "profileId": "ssh:gw" })
        );
        assert_eq!(
            by_id["ssh:gw"],
            json!({ "kind": "ssh", "target": "gw.example.net", "useConfig": false, "user": "ops" })
        );
    }

    #[test]
    fn leaves_user_names_and_key_paths_out_when_asked_to() {
        let hosts = sidebar();
        let options = ShareFolderOptions {
            include_users: false,
            include_key_paths: false,
        };
        let document = share_folder("Lab", &managed(&hosts), &[], options, NOW).document;
        let by_id: HashMap<&str, Value> = document
            .hosts
            .iter()
            .map(|host| (host.id.as_str(), Value::Object(host.profile.clone())))
            .collect();

        assert_eq!(
            by_id["leaf"],
            json!({
                "kind": "ssh",
                "target": "leaf-01.lab.example.net",
                "useConfig": false,
                "proxyJump": [saved_host_hop("bastion")],
            })
        );
        assert_eq!(by_id["desk"].get("username"), None);
        assert_eq!(by_id["ssh:build"].get("user"), None);
    }

    #[test]
    fn fills_in_what_a_host_inherits_from_its_folder() {
        let settings: Vec<FolderSettingsRecord> = serde_json::from_value(json!([
            {
                "id": "1", "path": "Lab",
                "auth": { "user": "labuser", "port": 2200, "identityFiles": ["~/.ssh/folder"] },
                "hasPassword": true, "createdAt": "", "updatedAt": "",
            },
            {
                "id": "2", "path": "Lab/Fabric", "auth": { "user": "fabric" },
                "hasPassword": false, "createdAt": "", "updatedAt": "",
            },
        ]))
        .unwrap();
        let inheriting = vec![
            saved(
                "a",
                json!({ "kind": "ssh", "target": "a.example.net", "useConfig": false }),
                json!({ "group": "Lab/Fabric" }),
            ),
            saved(
                "b",
                json!({ "kind": "ssh", "target": "b.example.net", "useConfig": false, "user": "own" }),
                json!({ "group": "Lab" }),
            ),
            configured(
                "c",
                json!({ "hostname": "c.example.net" }),
                json!({ "group": "Lab" }),
                json!({}),
            ),
        ];

        let document = share_folder("Lab", &managed(&inheriting), &settings, ALL, NOW).document;

        let profiles: Vec<Value> = document
            .hosts
            .iter()
            .map(|host| Value::Object(host.profile.clone()))
            .collect();
        assert_eq!(
            profiles,
            [
                json!({
                    "kind": "ssh", "target": "a.example.net", "useConfig": false,
                    "user": "fabric", "port": 2200, "identityFiles": ["~/.ssh/folder"],
                }),
                json!({
                    "kind": "ssh", "target": "b.example.net", "useConfig": false,
                    "user": "own", "port": 2200, "identityFiles": ["~/.ssh/folder"],
                }),
                json!({
                    "kind": "ssh", "target": "c.example.net", "useConfig": false,
                    "user": "labuser", "port": 2200, "identityFiles": ["~/.ssh/folder"],
                }),
            ]
        );
    }

    #[test]
    fn names_the_file_after_the_folder() {
        assert_eq!(
            shared_sessions_filename("Teams/Lab EU (new)"),
            "lab-eu-new.muxus-sessions.json"
        );
        assert_eq!(shared_sessions_filename("…"), "shared.muxus-sessions.json");
    }
}

mod following_a_shared_folder {
    use super::*;

    #[test]
    fn rebuilds_the_folder_with_every_hop_pointing_at_the_host_the_list_brought() {
        let hosts = sidebar();
        let document = share_folder("Lab", &managed(&hosts), &[], ALL, NOW).document;

        let list = parse_shared_sessions(
            &serde_json::to_string(&document).unwrap(),
            "link",
            "Team lab",
        )
        .unwrap();
        let id = |name: &str| named(&list, name).id.clone();

        assert_eq!(list.format, SharedSessionsFormat::Muxus);
        assert_eq!(list.skipped, 0);
        let placed: Vec<(&str, Option<&str>)> = list
            .hosts
            .iter()
            .map(|host| (host.name.as_str(), host.group.as_deref()))
            .collect();
        assert_eq!(
            placed,
            [
                ("leaf", Some("Team lab/Fabric")),
                ("console", Some("Team lab")),
                ("desk", Some("Team lab/Desktops")),
                ("Build box", Some("Team lab")),
                ("bastion", Some("Team lab/Jump hosts")),
                ("gw", Some("Team lab/Jump hosts")),
            ]
        );
        let leaf = named(&list, "leaf");
        assert_eq!(leaf.color.as_deref(), Some("#a855f7"));
        assert_eq!(leaf.icon.as_deref(), Some("router"));
        assert_eq!(profile(leaf)["user"], "admin");
        assert_eq!(
            profile(leaf)["proxyJump"],
            json!([saved_host_hop(&id("bastion"))])
        );
        assert_eq!(
            profile(named(&list, "bastion"))["proxyJump"],
            json!([saved_host_hop(&id("gw"))])
        );
        assert_eq!(
            profile(named(&list, "Build box"))["proxyJump"],
            json!([saved_host_hop(&id("gw"))])
        );
        assert_eq!(
            profile(named(&list, "desk"))["sshGateway"],
            json!({ "target": "gw.example.net", "profileId": id("gw") })
        );
        assert!(list.hosts.iter().all(|host| host.id.starts_with("shared-")));
    }

    #[test]
    fn refuses_a_list_from_a_newer_muxus_instead_of_guessing() {
        let error = parse_shared_sessions(
            &json!({ "format": "muxus-shared-sessions", "version": 2, "hosts": [] }).to_string(),
            "link",
            "Lab",
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "This session list was written by a newer Muxus. Update Muxus to follow it."
        );
    }

    #[test]
    fn reads_a_hand_written_list_that_has_only_names_and_profiles() {
        let list = parse_shared_sessions(
            &json!({
                "format": "muxus-shared-sessions",
                "hosts": [
                    {
                        "name": "node0", "folder": "clab",
                        "profile": { "kind": "ssh", "target": "node0.lab.example.net", "user": "alice" },
                    },
                    { "name": "broken" },
                ],
            })
            .to_string(),
            "link",
            "Lab",
        )
        .unwrap();

        assert_eq!(list.skipped, 1);
        assert_eq!(list.hosts.len(), 1);
        let host = &list.hosts[0];
        assert!(host.id.starts_with("shared-"));
        assert_eq!(host.name, "node0");
        assert_eq!(host.group.as_deref(), Some("Lab/clab"));
        assert_eq!(
            profile(host),
            json!({
                "kind": "ssh", "target": "node0.lab.example.net", "user": "alice",
                "useConfig": false,
            })
        );
    }
}
