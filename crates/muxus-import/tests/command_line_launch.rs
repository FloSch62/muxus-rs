//! Port of `tests/unit/client/command-line-launch.test.ts`, plus the
//! decisions of `CommandLineLaunchHandler.tsx`.

use muxus_core::api::{
    CommandLineLaunch, OpenSshProfileMetadata, ResolvedHostSettings, SavedHostKind,
    SavedHostProfile, SshHostEntry, WorkspaceSummary,
};
use muxus_core::protocol::{SavedHostSessionProfile, SessionProfile, SshProfile, TelnetProfile};
use muxus_import::launch::{
    CommandLineTargetKind, LaunchAction, LaunchCatalogs, ManagedHost, TargetResolution,
    WorkspaceLaunchOutcome, folder_launch_message, link_label, plan_command_line_launch,
    resolution_error, resolve_command_line_folder, resolve_command_line_host,
    resolve_command_line_workspace, workspace_message,
};

const ROOT: &str = "/home/test/.ssh/config";
const LAB_FILE: &str = "/home/test/.ssh/lab.conf";

fn ssh_host(
    alias: &str,
    group: Option<&str>,
    file: &str,
    display_name: Option<&str>,
) -> SshHostEntry {
    SshHostEntry {
        alias: alias.to_owned(),
        aliases: vec![alias.to_owned(), format!("{alias}.example.test")],
        description: None,
        file: file.to_owned(),
        options: Default::default(),
        resolved: ResolvedHostSettings {
            hostname: format!("{alias}.example.test"),
            port: 22,
            ..ResolvedHostSettings::default()
        },
        metadata: (group.is_some() || display_name.is_some()).then(|| OpenSshProfileMetadata {
            profile_id: format!("ssh-{alias}"),
            group: group.map(str::to_owned),
            display_name: display_name.map(str::to_owned),
            ..OpenSshProfileMetadata::default()
        }),
    }
}

fn saved_host(name: &str, group: Option<&str>, display_name: Option<&str>) -> SavedHostProfile {
    SavedHostProfile {
        id: format!("saved-{}", name.to_lowercase().replace(' ', "-")),
        kind: SavedHostKind::Telnet,
        name: name.to_owned(),
        profile: SavedHostSessionProfile::Telnet(TelnetProfile {
            profile_id: Some(format!("saved-{name}")),
            host: format!("{}.example.test", name.to_lowercase()),
            port: 23,
        }),
        metadata: OpenSshProfileMetadata {
            profile_id: format!("saved-{name}"),
            group: group.map(str::to_owned),
            display_name: display_name.map(str::to_owned),
            ..OpenSshProfileMetadata::default()
        },
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn workspace(id: &str, name: &str) -> WorkspaceSummary {
    WorkspaceSummary {
        id: id.to_owned(),
        name: name.to_owned(),
        is_locked: false,
        is_startup: false,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
        last_opened_at: None,
    }
}

fn key(resolution: &TargetResolution<ManagedHost<'_>>) -> String {
    match resolution {
        TargetResolution::Found(host) => host.key(),
        other => panic!("not found: {other:?}"),
    }
}

#[test]
fn matches_ssh_aliases_and_saved_host_names_case_insensitively() {
    let ssh = [ssh_host("edge-1", None, ROOT, None)];
    let saved = [saved_host("Console One", None, None)];

    assert_eq!(
        key(&resolve_command_line_host(
            "EDGE-1.EXAMPLE.TEST",
            &ssh,
            &saved
        )),
        "ssh:edge-1"
    );
    assert_eq!(
        key(&resolve_command_line_host("console one", &ssh, &saved)),
        "profile:saved-console-one"
    );
}

#[test]
fn uses_display_names_only_after_stable_names_and_reports_collisions() {
    let ssh = [ssh_host("core", None, ROOT, Some("Shared"))];
    let saved = [saved_host("Console", None, Some("Shared"))];

    assert!(matches!(
        resolve_command_line_host("core", &ssh, &saved),
        TargetResolution::Found(ManagedHost::Ssh(_))
    ));
    assert_eq!(
        resolve_command_line_host("shared", &ssh, &saved),
        TargetResolution::Ambiguous { count: 2 }
    );
}

fn folder_fixture() -> (Vec<SshHostEntry>, Vec<SavedHostProfile>, Vec<String>) {
    (
        vec![
            ssh_host("edge-eu", Some("Production/EU/Edge"), ROOT, None),
            ssh_host("core-eu", Some("Production/EU/Core"), ROOT, None),
            ssh_host("edge-us", Some("Production/US/Edge"), ROOT, None),
            ssh_host("lab-router", None, LAB_FILE, None),
        ],
        vec![saved_host("EU console", Some("Production/EU"), None)],
        vec![ROOT.to_owned(), LAB_FILE.to_owned()],
    )
}

#[test]
fn launches_every_descendant_of_an_exact_folder_path() {
    let (hosts, saved, files) = folder_fixture();
    let TargetResolution::Found(folder) =
        resolve_command_line_folder("production/eu", &hosts, &saved, &files, Some(ROOT))
    else {
        panic!("not found");
    };
    assert_eq!(folder.label, "Production/EU");
    let mut keys: Vec<String> = folder.hosts.iter().map(ManagedHost::key).collect();
    // Render order: subfolders (alphabetical) first, then the folder's own hosts.
    assert_eq!(
        keys,
        ["ssh:core-eu", "ssh:edge-eu", "profile:saved-eu-console"]
    );
    keys.sort();
    assert_eq!(
        keys,
        ["profile:saved-eu-console", "ssh:core-eu", "ssh:edge-eu"]
    );
}

#[test]
fn accepts_ssh_config_file_group_labels_and_rejects_ambiguous_leaf_labels() {
    let (hosts, saved, files) = folder_fixture();
    let TargetResolution::Found(file_group) =
        resolve_command_line_folder("lab", &hosts, &saved, &files, Some(ROOT))
    else {
        panic!("not found");
    };
    assert_eq!(
        file_group
            .hosts
            .iter()
            .map(ManagedHost::key)
            .collect::<Vec<_>>(),
        ["ssh:lab-router"]
    );
    assert_eq!(file_group.label, "Ungrouped · lab");
    assert_eq!(
        resolve_command_line_folder("Edge", &hosts, &saved, &files, Some(ROOT)),
        TargetResolution::Ambiguous { count: 2 }
    );
    assert_eq!(
        resolve_command_line_folder("Nowhere", &hosts, &saved, &files, Some(ROOT)),
        TargetResolution::NotFound
    );
}

#[test]
fn ungrouped_saved_hosts_join_the_root_file_group() {
    let hosts = [ssh_host("plain", None, ROOT, None)];
    let saved = [saved_host("Loose", None, None)];
    let files = [ROOT.to_owned()];
    // Without custom folders the root file group is "Hosts"; a saved host
    // joining it drops the file tooltip, so only the label still matches.
    let TargetResolution::Found(group) =
        resolve_command_line_folder("hosts", &hosts, &saved, &files, Some(ROOT))
    else {
        panic!("not found");
    };
    assert_eq!(
        group.hosts.iter().map(ManagedHost::key).collect::<Vec<_>>(),
        ["profile:saved-loose", "ssh:plain"]
    );
    assert_eq!(
        resolve_command_line_folder("config", &hosts, &saved, &files, Some(ROOT)),
        TargetResolution::NotFound
    );
    let only_ssh = resolve_command_line_folder("config", &hosts, &[], &files, Some(ROOT));
    assert!(matches!(only_ssh, TargetResolution::Found(_)));
}

#[test]
fn matches_ids_before_names_and_detects_duplicate_names() {
    let workspaces = [
        workspace("production-id", "Production"),
        workspace("duplicate-a", "Lab"),
        workspace("duplicate-b", "lab"),
    ];
    let found = |name: &str| match resolve_command_line_workspace(name, &workspaces) {
        TargetResolution::Found(workspace) => workspace.id.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(found("PRODUCTION-ID"), "production-id");
    assert_eq!(found("production"), "production-id");
    assert_eq!(
        resolve_command_line_workspace("lab", &workspaces),
        TargetResolution::Ambiguous { count: 2 }
    );
    assert_eq!(
        resolve_command_line_workspace("missing", &workspaces),
        TargetResolution::NotFound
    );
}

#[test]
fn plans_each_request_kind_with_its_message() {
    let (hosts, saved, files) = folder_fixture();
    let workspaces = [workspace("w1", "Night shift")];
    let catalogs = LaunchCatalogs {
        ssh_hosts: &hosts,
        saved_profiles: &saved,
        files: &files,
        root_file: Some(ROOT),
        workspaces: &workspaces,
    };

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Connect {
            name: "admin@edge:2222".to_owned(),
        },
        catalogs,
    );
    assert_eq!(
        action,
        LaunchAction::ConnectTarget {
            target: "admin@edge:2222".to_owned()
        }
    );
    assert_eq!(message.as_deref(), Some("Connecting to “admin@edge:2222”."));

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Host {
            name: "edge-eu".to_owned(),
        },
        catalogs,
    );
    assert!(matches!(
        action,
        LaunchAction::ConnectHost {
            host: ManagedHost::Ssh(_)
        }
    ));
    assert_eq!(message.as_deref(), Some("Connecting to “edge-eu”."));

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Host {
            name: "nope".to_owned(),
        },
        catalogs,
    );
    assert_eq!(action, LaunchAction::Fail);
    assert_eq!(message.as_deref(), Some("No host named “nope” was found."));

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Folder {
            name: "Edge".to_owned(),
        },
        catalogs,
    );
    assert_eq!(action, LaunchAction::Fail);
    assert_eq!(
        message.as_deref(),
        Some("More than one folder matches “Edge”. Use its exact path.")
    );

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Folder {
            name: "Production/US".to_owned(),
        },
        catalogs,
    );
    let LaunchAction::LaunchFolder { folder } = action else {
        panic!("not a folder launch");
    };
    assert_eq!(message, None);
    assert_eq!(
        folder_launch_message(&folder.label, folder.hosts.len()).as_deref(),
        Some("Launching 1 session from “Production/US”.")
    );
    assert_eq!(folder_launch_message("X", 0), None);
    assert_eq!(
        folder_launch_message("X", 3).as_deref(),
        Some("Launching 3 sessions from “X”.")
    );

    let (action, _) = plan_command_line_launch(
        &CommandLineLaunch::Workspace {
            name: "night shift".to_owned(),
        },
        catalogs,
    );
    let LaunchAction::OpenWorkspace { workspace } = action else {
        panic!("not a workspace");
    };
    assert_eq!(
        workspace_message(&workspace.name, WorkspaceLaunchOutcome::NewWindow),
        "Opening workspace “Night shift” in a new window."
    );
    assert_eq!(
        workspace_message("A", WorkspaceLaunchOutcome::AlreadyOpen),
        "Workspace “A” is already open."
    );
    assert_eq!(
        workspace_message("A", WorkspaceLaunchOutcome::Focused),
        "Switched to the window showing “A”."
    );
    let (_, message) = plan_command_line_launch(
        &CommandLineLaunch::Workspace {
            name: "day".to_owned(),
        },
        catalogs,
    );
    assert_eq!(
        message.as_deref(),
        Some("No workspace named “day” was found.")
    );
    assert_eq!(
        resolution_error(
            CommandLineTargetKind::Workspace,
            "x",
            &TargetResolution::<()>::Ambiguous { count: 2 }
        )
        .as_deref(),
        Some("More than one workspace matches “x”. Use its exact name or ID.")
    );
}

#[test]
fn plans_links_and_reports_malformed_ones() {
    let saved = [saved_host("Console", Some("Lab"), None)];
    let catalogs = LaunchCatalogs {
        ssh_hosts: &[],
        saved_profiles: &saved,
        files: &[],
        root_file: None,
        workspaces: &[],
    };
    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Url {
            name: "telnet://console".to_owned(),
        },
        catalogs,
    );
    let LaunchAction::OpenProfile {
        profile,
        title,
        color,
    } = action
    else {
        panic!("not a profile");
    };
    assert_eq!(title, "Console");
    assert_eq!(color, None);
    assert_eq!(profile.profile_id(), Some("saved-console"));
    assert_eq!(message.as_deref(), Some("Connecting to “Console”."));

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Url {
            name: "ssh://admin@10.9.9.9".to_owned(),
        },
        catalogs,
    );
    assert_eq!(
        action,
        LaunchAction::OpenProfile {
            profile: SessionProfile::Ssh(SshProfile::new("admin@10.9.9.9")),
            title: "admin@10.9.9.9".to_owned(),
            color: None,
        }
    );
    assert_eq!(message.as_deref(), Some("Connecting to “admin@10.9.9.9”."));

    let (action, message) = plan_command_line_launch(
        &CommandLineLaunch::Url {
            name: "sftp://host".to_owned(),
        },
        catalogs,
    );
    assert_eq!(action, LaunchAction::Fail);
    assert!(
        message
            .expect("message")
            .starts_with("Cannot open “sftp://host”. ")
    );
}

#[test]
fn link_labels_are_short_and_printable() {
    assert_eq!(link_label("  ssh://a\u{7}b  "), "ssh://a\u{FFFD}b");
    let long = format!("ssh://{}", "x".repeat(100));
    let label = link_label(&long);
    assert_eq!(label.chars().count(), 80);
    assert!(label.ends_with('…'));
}
