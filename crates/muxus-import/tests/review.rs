//! The import dialog's model (`SessionImportDialog.tsx`) and the restore
//! plan it hands to the backend (`restoreImportedConnections`).

use muxus_core::api::{
    LoginSendStep, LoginSequence, LoginSequenceStep, ManagedHostRef, OpenSshProfileMetadata,
    ResolvedHostSettings, SavedHostProfile, SshHostEntry,
};
use muxus_core::js::Patch;
use muxus_core::protocol::SavedHostSessionProfile;
use muxus_import::review::{
    SKIPPED_SESSIONS_PER_PAGE, decode_import_file, searchable_session_values, session_details,
    session_kind_label, skipped_session_name,
};
use muxus_import::{
    ConflictStrategy, ExistingHosts, ImportReview, ImportVendor, ImportedSshStorage,
    PortableConnections, PortableHostMetadata, PortableSavedHost, RestoreResult, RestoreStep,
    SkippedImportedSession, plan_restore,
};

const MOBAXTERM: &str = r"
[Bookmarks]
SubRep=Lab
Inner=#109#0%10.0.1.5%22%root%%-1%-1%%bastion.example.com%2222%jump%0%0%0%_ProfileDir_\keys\lab%_ProfileDir_\keys\bastion%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Runs=#109#0%run.example.com%22%%%-1%-1%top%%%%-1%0#MobaFont%10
Windows=#91#4%win.example.com%3389%admin%0%-1%-1%-1%-1%0%0%-1%%%%%%%#MobaFont%10
Telnet=#98#1%switch.example.com%23%%%2%%22%%%0#MobaFont%10
";

fn ssh_entry(alias: &str, file: &str) -> SshHostEntry {
    SshHostEntry {
        alias: alias.to_owned(),
        aliases: vec![alias.to_owned(), format!("{alias}-other")],
        description: None,
        file: file.to_owned(),
        options: Default::default(),
        resolved: ResolvedHostSettings::default(),
        metadata: None,
    }
}

fn saved_entry(id: &str, profile: SavedHostSessionProfile) -> SavedHostProfile {
    SavedHostProfile {
        id: id.to_owned(),
        kind: profile.kind(),
        name: id.to_owned(),
        profile,
        metadata: OpenSshProfileMetadata::default(),
        created_at: String::new(),
        updated_at: String::new(),
    }
}

#[test]
fn review_starts_with_everything_selected_in_muxus_storage() {
    let mut review =
        ImportReview::from_content(ImportVendor::MobaXterm, MOBAXTERM, "MobaXterm.ini")
            .expect("review");
    assert_eq!(review.subtitle(), "3 supported sessions from MobaXterm.ini");
    assert_eq!(review.ssh_storage, ImportedSshStorage::Muxus);
    assert_eq!(review.conflicts, ConflictStrategy::Keep);
    assert_eq!(review.selected_count(), 3);
    assert!(review.all_selected());
    assert_eq!(review.toggle_all_label(), "Clear selection");
    assert_eq!(review.import_label(), "Import 3 sessions");
    assert!(review.has_ssh_hosts());
    assert_eq!(
        review.skipped_notice().as_deref(),
        Some("1 unsupported or incomplete session was skipped.")
    );

    let first = review.sessions()[0].id().to_owned();
    review.toggle(&first);
    assert!(!review.is_selected(&first));
    assert_eq!(review.toggle_all_label(), "Select all");
    review.toggle_all();
    assert_eq!(review.selected_count(), 3);
    review.toggle_all();
    assert_eq!(review.selected_count(), 0);
    assert_eq!(review.import_label(), "Import 0 sessions");
    assert!(review.plan(&ExistingHosts::default()).is_none());
    review.toggle(&first);
    assert_eq!(review.import_label(), "Import 1 session");
}

#[test]
fn search_matches_names_hosts_users_folders_kinds_and_jump_hosts() {
    let mut review =
        ImportReview::from_content(ImportVendor::MobaXterm, MOBAXTERM, "file").expect("review");
    let names = |review: &ImportReview| -> Vec<String> {
        review
            .filtered_sessions()
            .iter()
            .map(|session| session.name().to_owned())
            .collect()
    };
    review.search = "  RDP ".to_owned();
    assert_eq!(names(&review), ["Windows"]);
    review.search = "jump@bastion".to_owned();
    assert_eq!(names(&review), ["Inner"]);
    review.search = "lab".to_owned();
    assert_eq!(names(&review), ["Inner", "Runs", "Windows"]);
    review.search = "nothing".to_owned();
    assert!(review.filtered_sessions().is_empty());
    review.search = String::new();
    assert_eq!(names(&review).len(), 3);
    assert_eq!(
        searchable_session_values(&review.sessions()[2]),
        [
            Some("Windows".to_owned()),
            Some("RDP".to_owned()),
            Some("win.example.com".to_owned()),
            Some("admin".to_owned()),
            Some("Lab".to_owned()),
            None,
        ]
    );
}

#[test]
fn rows_describe_each_session() {
    let review =
        ImportReview::from_content(ImportVendor::MobaXterm, MOBAXTERM, "file").expect("review");
    let details: Vec<String> = review.sessions().iter().map(session_details).collect();
    assert_eq!(
        details,
        [
            "root@10.0.1.5:22 · key lab · via jump@bastion.example.com:2222",
            "run.example.com:22 · password prompt · runs top; exec \"$SHELL\" -l",
            "admin@win.example.com:3389",
        ]
    );
    assert_eq!(session_kind_label(&review.sessions()[0]), None);
    assert_eq!(session_kind_label(&review.sessions()[2]), Some("RDP"));

    let serial = ImportReview::from_content(
        ImportVendor::SecureCrt,
        r#"<VanDyke><key name="Sessions"><key name="Bench">
            <dword name="Is Session">1</dword>
            <string name="Protocol Name">Serial</string>
            <string name="Com Port">COM3</string>
            <dword name="Baud Rate">9600</dword>
            <dword name="Stop Bits">1</dword>
            <dword name="Parity">2</dword>
            <dword name="XON Flow">1</dword>
          </key><key name="Keyed">
            <dword name="Is Session">1</dword>
            <string name="Protocol Name">SSH2</string>
            <string name="Hostname">k.example.test</string>
          </key></key></VanDyke>"#,
        "export.xml",
    )
    .expect("review");
    assert_eq!(
        session_details(&serial.sessions()[0]),
        "COM3 · 9600 baud · 8E1.5 · XON/XOFF"
    );
    assert_eq!(session_kind_label(&serial.sessions()[0]), Some("Serial"));
    assert_eq!(
        session_details(&serial.sessions()[1]),
        "k.example.test:22 · SSH key / agent"
    );
    assert!(serial.has_ssh_hosts());
}

#[test]
fn ssh_storage_is_asked_only_when_ssh_hosts_are_imported() {
    let serial_only = ImportReview::from_content(
        ImportVendor::SecureCrt,
        r#"<VanDyke><key name="Sessions"><key name="Bench">
            <dword name="Is Session">1</dword>
            <string name="Protocol Name">Serial</string>
            <string name="Com Port">COM3</string>
          </key></key></VanDyke>"#,
        "export.xml",
    )
    .expect("review");
    assert!(!serial_only.has_ssh_hosts());

    let desktop_via_gateway = ImportReview::from_content(
        ImportVendor::MobaXterm,
        "[Bookmarks]\nConsole=#128#5%127.0.0.1%5901%-1%0%bastion.example.com%22%root%%-1\n",
        "file",
    )
    .expect("review");
    assert!(desktop_via_gateway.has_ssh_hosts());
    let plain_desktop = ImportReview::from_content(
        ImportVendor::MobaXterm,
        "[Bookmarks]\nConsole=#128#5%127.0.0.1%5901\n",
        "file",
    )
    .expect("review");
    assert!(!plain_desktop.has_ssh_hosts());
    assert_eq!(
        plain_desktop.storage_caption(),
        "SSH connection settings stay in the Muxus database; ssh_config is not changed."
    );
}

#[test]
fn conflicts_follow_the_chosen_storage() {
    let mut review =
        ImportReview::from_content(ImportVendor::MobaXterm, MOBAXTERM, "file").expect("review");
    let muxus = review.connections();
    let inner_id = muxus.saved_hosts[0].id.clone();
    let windows_id = muxus.saved_hosts[2].id.clone();
    let existing = ExistingHosts::new(
        &[ssh_entry("Runs", "/home/test/.ssh/config")],
        &[saved_entry(
            &inner_id,
            SavedHostSessionProfile::Ssh(muxus_core::protocol::SshProfile::new("x")),
        )],
    );

    let conflicts: Vec<bool> = review
        .sessions()
        .iter()
        .map(|session| review.is_conflict(session, &existing))
        .collect();
    assert_eq!(conflicts, [true, false, false]);
    assert_eq!(review.keep_label(&existing), "Keep existing (1)");

    review.ssh_storage = ImportedSshStorage::Openssh;
    let conflicts: Vec<bool> = review
        .sessions()
        .iter()
        .map(|session| review.is_conflict(session, &existing))
        .collect();
    // ssh_config hosts conflict by alias; the RDP session stays a saved host.
    assert_eq!(conflicts, [false, true, false]);
    assert_eq!(review.conflicting_count(&existing), 1);
    assert!(!windows_id.is_empty());
}

#[test]
fn plans_keep_and_replace_writes_in_restore_order() {
    let mut review =
        ImportReview::from_content(ImportVendor::MobaXterm, MOBAXTERM, "file").expect("review");
    review.ssh_storage = ImportedSshStorage::Openssh;
    let existing = ExistingHosts::new(&[ssh_entry("Runs", "/home/test/.ssh/extra.conf")], &[]);

    let keep = review.plan(&existing).expect("plan");
    assert_eq!(
        keep.result,
        RestoreResult {
            added: 3,
            updated: 0,
            skipped: 1
        }
    );
    let kinds: Vec<&str> = keep
        .steps
        .iter()
        .map(|step| match step {
            RestoreStep::UpsertSshHost { alias, .. } => alias.as_str(),
            RestoreStep::PutSavedHost { input, .. } => input.name.as_str(),
            RestoreStep::SetHostOrder(_) => "order",
        })
        .collect();
    assert_eq!(
        kinds,
        ["Inner", "jump-jump-bastion.example.com-2222", "Windows"]
    );

    review.conflicts = ConflictStrategy::Replace;
    let replace = review.plan(&existing).expect("plan");
    assert_eq!(
        replace.result,
        RestoreResult {
            added: 3,
            updated: 1,
            skipped: 0
        }
    );
    let RestoreStep::UpsertSshHost {
        request, metadata, ..
    } = &replace.steps[1]
    else {
        panic!("not an ssh host");
    };
    assert_eq!(request.previous_alias.as_deref(), Some("Runs"));
    assert_eq!(request.file.as_deref(), Some("/home/test/.ssh/extra.conf"));
    assert_eq!(
        request.description.as_deref(),
        Some("Imported from MobaXterm.")
    );
    let metadata = metadata.as_ref().expect("metadata");
    assert_eq!(metadata.display_name, Patch::Set("Runs".to_owned()));
    assert_eq!(metadata.group, Patch::Set("Lab".to_owned()));
    assert_eq!(metadata.color, Patch::Clear);
    assert_eq!(metadata.disable_sftp, Some(false));
    assert_eq!(metadata.login_sequence, Patch::Clear);

    assert_eq!(
        ImportVendor::MobaXterm.import_complete_message(&replace.result),
        "MobaXterm import complete — 3 added · 1 replaced."
    );
    assert_eq!(
        ImportVendor::SecureCrt.import_complete_message(&RestoreResult::default()),
        "SecureCRT import complete."
    );
}

#[test]
fn restore_writes_host_order_only_when_replacing() {
    let data = PortableConnections {
        ssh_hosts: Vec::new(),
        saved_hosts: vec![PortableSavedHost {
            id: "p1".to_owned(),
            name: "P1".to_owned(),
            profile: SavedHostSessionProfile::Ssh(muxus_core::protocol::SshProfile::new("h")),
            metadata: PortableHostMetadata {
                login_sequence: Some(LoginSequence {
                    steps: vec![LoginSequenceStep::Send(LoginSendStep {
                        id: "s1".to_owned(),
                        text: "hello".to_owned(),
                        enter: true,
                    })],
                }),
                sort_order: Some(3),
                ..PortableHostMetadata::default()
            },
        }],
        host_order: vec![ManagedHostRef::Profile {
            id: "p1".to_owned(),
        }],
    };
    let existing = ExistingHosts::new(
        &[],
        &[saved_entry(
            "p1",
            SavedHostSessionProfile::Ssh(muxus_core::protocol::SshProfile::new("old")),
        )],
    );

    let keep = plan_restore(&data, &existing, ConflictStrategy::Keep);
    assert!(keep.steps.is_empty());
    assert_eq!(keep.result.skipped, 1);

    let replace = plan_restore(&data, &existing, ConflictStrategy::Replace);
    assert_eq!(replace.steps.len(), 2);
    let RestoreStep::PutSavedHost { input, metadata } = &replace.steps[0] else {
        panic!("not a saved host");
    };
    assert_eq!(input.id.as_deref(), Some("p1"));
    assert!(matches!(metadata.login_sequence, Patch::Set(_)));
    assert!(
        matches!(&replace.steps[1], RestoreStep::SetHostOrder(order) if order.hosts.len() == 1)
    );
    assert_eq!(replace.result.updated, 1);
}

#[test]
fn skipped_sessions_are_paged_fifty_at_a_time() {
    let skipped: Vec<SkippedImportedSession> = (0..120)
        .map(|index| SkippedImportedSession {
            id: format!("s{index}"),
            name: format!("Session {index}"),
            folder: (index % 2 == 0).then(|| "Lab".to_owned()),
            reason: "Only SSH, RDP and VNC sessions can be imported".to_owned(),
        })
        .collect();
    let page = muxus_import::review::SkippedPage::new(&skipped, 0);
    assert_eq!(SKIPPED_SESSIONS_PER_PAGE, 50);
    assert_eq!((page.start, page.end, page.page_count), (0, 50, 3));
    assert!(page.has_pager() && !page.has_previous() && page.has_next());
    assert_eq!(page.label(), "Showing 1–50 of 120");
    let last = muxus_import::review::SkippedPage::new(&skipped, 9);
    assert_eq!((last.page, last.start, last.end), (2, 100, 120));
    assert_eq!(last.label(), "Showing 101–120 of 120");
    assert!(!last.has_next());
    assert_eq!(skipped_session_name(&skipped[0]), "Lab/Session 0");
    assert_eq!(skipped_session_name(&skipped[1]), "Session 1");
    let empty = muxus_import::review::SkippedPage::new(&[], 4);
    assert_eq!(
        (empty.page, empty.start, empty.end, empty.page_count),
        (0, 0, 0, 0)
    );
    assert!(!empty.has_pager());
}

#[test]
fn files_are_checked_and_decoded_like_the_browser() {
    assert_eq!(
        ImportVendor::MobaXterm
            .check_file_size(10 * 1024 * 1024 + 1)
            .unwrap_err()
            .to_string(),
        "That MobaXterm file is larger than 10 MB."
    );
    assert!(
        ImportVendor::MobaXterm
            .check_file_size(10 * 1024 * 1024)
            .is_ok()
    );
    assert_eq!(
        ImportVendor::SecureCrt
            .check_file_size(30 * 1024 * 1024)
            .unwrap_err()
            .to_string(),
        "That SecureCRT file is larger than 20 MB."
    );
    assert_eq!(
        decode_import_file(b"\xEF\xBB\xBF[Bookmarks]\xFF"),
        "[Bookmarks]\u{FFFD}"
    );
    assert_eq!(
        ImportVendor::SecureCrt.read_error(),
        "Could not read that SecureCRT file."
    );
    assert_eq!(
        ImportVendor::MobaXterm.detect_error(),
        "Could not read local MobaXterm sessions."
    );
    assert_eq!(
        ImportVendor::MobaXterm.import_error(),
        "The MobaXterm sessions could not be imported."
    );
    assert_eq!(ImportVendor::SecureCrt.auto_source(), None);
    assert_eq!(
        ImportVendor::MobaXterm.auto_source().is_some(),
        cfg!(windows)
    );
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn reads_a_chosen_file_by_name_and_refuses_oversized_ones() {
    let scratch = tempfile::tempdir().expect("scratch dir");
    let path = scratch.path().join("Sessions.mxtsessions");
    std::fs::write(
        &path,
        b"\xEF\xBB\xBF[Bookmarks]\r\nBox=#109#0%box.example.test%22%root%%\r\n",
    )
    .expect("file");
    let review = runtime()
        .block_on(ImportReview::read_file(ImportVendor::MobaXterm, &path))
        .expect("review");
    assert_eq!(
        review.subtitle(),
        "1 supported sessions from Sessions.mxtsessions"
    );

    let large = scratch.path().join("large.ini");
    let file = std::fs::File::create(&large).expect("file");
    file.set_len(10 * 1024 * 1024 + 1).expect("sparse length");
    let error = runtime()
        .block_on(ImportReview::read_file(ImportVendor::MobaXterm, &large))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "That MobaXterm file is larger than 10 MB."
    );

    let missing = runtime()
        .block_on(ImportReview::read_file(
            ImportVendor::SecureCrt,
            &scratch.path().join("missing.xml"),
        ))
        .unwrap_err();
    assert_eq!(missing.to_string(), "Could not read that SecureCRT file.");

    let invalid = scratch.path().join("export.xml");
    std::fs::write(&invalid, "<Other/>").expect("file");
    let error = runtime()
        .block_on(ImportReview::read_file(ImportVendor::SecureCrt, &invalid))
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "That is not a valid SecureCRT XML export."
    );
}

#[cfg(not(windows))]
#[test]
fn local_discovery_is_refused_off_windows() {
    let error = runtime()
        .block_on(ImportReview::detect_local_mobaxterm())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Automatic MobaXterm discovery is only available on Windows."
    );
}
