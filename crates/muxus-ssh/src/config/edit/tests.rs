//! Port of tests/unit/server/ssh-config-edit.test.ts.

use std::path::{Path, PathBuf};

use super::*;
use crate::config::{list_hosts, load_config_document};
use crate::types::{HostBlockOptions, Patch, StrictHostKeyChecking};

struct Homes {
    dir: tempfile::TempDir,
    env: LocalEnv,
    counter: std::cell::Cell<usize>,
}

impl Homes {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let env = LocalEnv {
            home: dir.path().join("home"),
            username: "localuser".into(),
            hostname: "laptop".into(),
            uid: 1000,
        };
        Self {
            dir,
            env,
            counter: std::cell::Cell::new(0),
        }
    }

    fn next(&self) -> usize {
        let n = self.counter.get();
        self.counter.set(n + 1);
        n
    }

    /// Path for a config whose .ssh directory does not exist yet.
    fn fresh_root(&self) -> PathBuf {
        self.dir
            .path()
            .join(format!("home-{}", self.next()))
            .join(".ssh")
            .join("config")
    }

    fn seed(&self, content: &str) -> PathBuf {
        let dir = self
            .dir
            .path()
            .join(format!("home-{}", self.next()))
            .join(".ssh");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config");
        std::fs::write(&file, content).unwrap();
        file
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn lines(parts: &[&str]) -> String {
    parts.join("\n")
}

fn req() -> HostUpsertRequest {
    HostUpsertRequest {
        aliases: vec!["web".into()],
        options: HostBlockOptions {
            hostname: Some("web.example.com".into()),
            user: Some("deploy".into()),
            port: Some(2222),
            ..Default::default()
        },
        description: None,
        file: None,
        previous_alias: None,
    }
}

fn editing(previous: &str) -> HostUpsertRequest {
    HostUpsertRequest {
        previous_alias: Some(previous.into()),
        ..req()
    }
}

fn aliases(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// --- upsertHost ---

#[test]
fn creates_the_config_0600_when_none_exists() {
    let h = Homes::new();
    let root = h.fresh_root();
    upsert_host(
        &HostUpsertRequest {
            description: Some("Main web box".into()),
            ..req()
        },
        &root,
        &h.env,
    )
    .unwrap();
    assert_eq!(
        read(&root),
        lines(&[
            "# Main web box",
            "Host web",
            "  HostName web.example.com",
            "  User deploy",
            "  Port 2222",
            ""
        ])
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn appends_after_existing_content_with_a_blank_separator() {
    let h = Homes::new();
    let existing = lines(&[
        "# hand-written banner",
        "Host *",
        "\tServerAliveInterval 60",
        "",
        "Host db",
        "\tHostName db.internal",
    ]);
    let root = h.seed(&format!("{existing}\n"));
    upsert_host(&req(), &root, &h.env).unwrap();
    let text = read(&root);
    assert!(text.starts_with(&format!("{existing}\n")));
    // Indentation style of the file (tabs) is picked up for the new block.
    assert!(text.contains("\tHostName web.example.com"));
}

#[test]
fn edits_a_block_in_place_leaving_neighbors_byte_identical() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "# before",
        "Host a",
        "  User alpha",
        "",
        "# web comment",
        "Host web",
        "  HostName old.example.com",
        "",
        "Host z",
        "  User zed",
        "",
    ]));
    upsert_host(
        &HostUpsertRequest {
            description: Some("web comment".into()),
            ..editing("web")
        },
        &root,
        &h.env,
    )
    .unwrap();
    assert_eq!(
        read(&root),
        lines(&[
            "# before",
            "Host a",
            "  User alpha",
            "",
            "# web comment",
            "Host web",
            "  HostName web.example.com",
            "  User deploy",
            "  Port 2222",
            "",
            "Host z",
            "  User zed",
            "",
        ])
    );
}

#[test]
fn renames_an_alias_and_preserves_wildcard_co_patterns() {
    let h = Homes::new();
    let root = h.seed(&lines(&["Host web w-*", "  HostName web.example.com", ""]));
    upsert_host(
        &HostUpsertRequest {
            aliases: aliases(&["website"]),
            ..editing("web")
        },
        &root,
        &h.env,
    )
    .unwrap();
    assert!(read(&root).contains("Host website w-*"));
    assert_eq!(
        list_hosts(&load_config_document(&root, &h.env), None)[0].alias,
        "website"
    );
}

#[test]
fn rejects_an_alias_that_already_exists_in_another_block() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host web", "  User a", "", "Host db", "  User b", "",
    ]));
    let err = upsert_host(
        &HostUpsertRequest {
            aliases: aliases(&["db"]),
            ..editing("web")
        },
        &root,
        &h.env,
    )
    .unwrap_err();
    assert!(err.message.contains("already exists"));
    assert_eq!(err.status, 409);
    assert_eq!(err.code.as_deref(), Some("host-exists"));
    assert!(
        upsert_host(&req(), &root, &h.env)
            .unwrap_err()
            .message
            .contains("already exists")
    );
}

#[test]
fn round_trips_a_listed_host_without_semantic_drift() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "# my app",
        "Host app",
        "  HostName app.example.com",
        "  ProxyJump bastion",
        "  IdentityFile ~/.ssh/app",
        "  CertificateFile ~/.ssh/app-cert.pub",
        "  IdentityAgent ${ONEPASSWORD_SSH_AUTH_SOCK}",
        "  ProxyCommand custom-proxy %h %p",
        "  LocalForward 8080 localhost:80",
        "  StrictHostKeyChecking accept-new",
        "  RemoteCommand tmux new -A -s main",
        "  RequestTTY yes",
        "  Compression yes",
        "",
    ]));
    let before = list_hosts(&load_config_document(&root, &h.env), None).remove(0);
    upsert_host(
        &HostUpsertRequest {
            aliases: before.aliases.clone(),
            description: before.description.clone(),
            options: before.options.clone(),
            previous_alias: Some(before.alias.clone()),
            file: None,
        },
        &root,
        &h.env,
    )
    .unwrap();
    let after = list_hosts(&load_config_document(&root, &h.env), None).remove(0);
    assert_eq!(after, before);
}

#[test]
fn moves_a_host_to_a_new_include_file_and_wires_the_include_into_the_root() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host web",
        "  HostName web.example.com",
        "",
        "Host db",
        "  User b",
        "",
    ]));
    let group_file = root.parent().unwrap().join("config.d").join("work");
    upsert_host(
        &HostUpsertRequest {
            file: Some(path_string(&group_file)),
            ..editing("web")
        },
        &root,
        &h.env,
    )
    .unwrap();
    let root_text = read(&root);
    assert!(!root_text.contains("web.example.com"));
    assert!(root_text.contains("Include config.d/work"));
    assert!(read(&group_file).contains("HostName web.example.com"));
    let hosts = list_hosts(&load_config_document(&root, &h.env), None);
    let mut names: Vec<&str> = hosts.iter().map(|h| h.alias.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["db", "web"]);
    assert_eq!(
        hosts.iter().find(|h| h.alias == "web").unwrap().file,
        path_string(&group_file)
    );
}

#[test]
fn moves_a_host_from_an_include_file_back_to_the_root_config() {
    let h = Homes::new();
    let root = h.seed(&lines(&["Host web", "  HostName web.example.com", ""]));
    let group_file = root.parent().unwrap().join("config.d").join("work");
    upsert_host(
        &HostUpsertRequest {
            file: Some(path_string(&group_file)),
            ..editing("web")
        },
        &root,
        &h.env,
    )
    .unwrap();
    upsert_host(
        &HostUpsertRequest {
            file: Some(path_string(&root)),
            ..editing("web")
        },
        &root,
        &h.env,
    )
    .unwrap();
    assert!(read(&root).contains("HostName web.example.com"));
    assert!(!read(&group_file).contains("Host web"));
    assert_eq!(
        list_hosts(&load_config_document(&root, &h.env), None)[0].file,
        path_string(&root)
    );
}

#[test]
fn refuses_config_files_outside_the_root_config_directory() {
    let h = Homes::new();
    let root = h.seed("");
    let err = upsert_host(
        &HostUpsertRequest {
            file: Some("/etc/passwd".into()),
            ..req()
        },
        &root,
        &h.env,
    )
    .unwrap_err();
    assert!(err.message.contains("must live under"));
}

#[test]
fn validates_aliases_and_extras() {
    let h = Homes::new();
    let root = h.seed("");
    let message = |r: HostUpsertRequest| upsert_host(&r, &root, &h.env).unwrap_err().message;
    assert!(
        message(HostUpsertRequest {
            aliases: aliases(&["has space"]),
            ..req()
        })
        .contains("invalid alias")
    );
    assert!(
        message(HostUpsertRequest {
            aliases: aliases(&["star*"]),
            ..req()
        })
        .contains("invalid alias")
    );
    let with_extra = |keyword: &str| HostUpsertRequest {
        options: HostBlockOptions {
            extras: Some(vec![crate::types::extra(keyword, "x")]),
            ..Default::default()
        },
        ..req()
    };
    assert!(message(with_extra("Host")).contains("invalid option keyword"));
    assert!(message(with_extra("Weird Key")).contains("invalid option keyword"));
    assert!(
        message(HostUpsertRequest {
            options: HostBlockOptions {
                proxy_jump: Some(aliases(&["bastion"])),
                proxy_command: Some("proxy %h %p".into()),
                ..Default::default()
            },
            ..req()
        })
        .contains("mutually exclusive")
    );
}

#[test]
fn refuses_jump_hosts_stored_in_muxus_which_openssh_cannot_resolve() {
    let h = Homes::new();
    let root = h.seed("");
    let err = upsert_host(
        &HostUpsertRequest {
            options: HostBlockOptions {
                proxy_jump: Some(aliases(&["bastion", "muxus-host:saved-1"])),
                ..Default::default()
            },
            ..req()
        },
        &root,
        &h.env,
    )
    .unwrap_err();
    assert!(err.message.contains("stored in Muxus"));
}

#[test]
fn keeps_a_muxus_bak_of_the_previous_content() {
    let h = Homes::new();
    let root = h.seed(&lines(&["Host web", "  User old", ""]));
    upsert_host(&editing("web"), &root, &h.env).unwrap();
    assert!(read(&PathBuf::from(format!("{}.muxus.bak", path_string(&root)))).contains("User old"));
}

#[cfg(unix)]
#[test]
fn preserves_a_config_symlink_and_atomically_replaces_its_target() {
    let h = Homes::new();
    let home = h.dir.path().join(format!("home-{}", h.next()));
    let ssh_dir = home.join(".ssh");
    let target_dir = home.join("dotfiles");
    let target = target_dir.join("ssh-config");
    let root = ssh_dir.join("config");
    std::fs::create_dir_all(&ssh_dir).unwrap();
    std::fs::create_dir_all(&target_dir).unwrap();
    std::fs::write(&target, lines(&["Host web", "  User old", ""])).unwrap();
    std::os::unix::fs::symlink("../dotfiles/ssh-config", &root).unwrap();

    upsert_host(&editing("web"), &root, &h.env).unwrap();

    assert!(
        std::fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(read(&target).contains("HostName web.example.com"));
    assert!(read(&PathBuf::from(format!("{}.muxus.bak", path_string(&root)))).contains("User old"));
}

// --- patchHosts ---

fn patch_user(user: &str) -> HostOptionsPatch {
    HostOptionsPatch {
        user: Patch::Set(user.into()),
        ..Default::default()
    }
}

#[test]
fn patch_changes_only_the_patched_lines() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "# web box",
        "Host web",
        "  HostName web.example.com",
        "  # the deploy account",
        "  user=alice",
        "  Compression yes",
        "",
        "Host db",
        "  HostName db.internal",
        "",
        "Host z",
        "  User zed",
        "",
    ]));
    let patch = HostOptionsPatch {
        port: Patch::Set(2222),
        ..patch_user("ops")
    };
    assert_eq!(
        patch_hosts(&aliases(&["web", "db"]), &patch, &root, &h.env).unwrap(),
        2
    );
    assert_eq!(
        read(&root),
        lines(&[
            "# web box",
            "Host web",
            "  HostName web.example.com",
            "  # the deploy account",
            "  user ops",
            "  Compression yes",
            "  Port 2222",
            "",
            "Host db",
            "  HostName db.internal",
            "  User ops",
            "  Port 2222",
            "",
            "Host z",
            "  User zed",
            "",
        ])
    );
}

#[test]
fn patch_removes_an_option_set_to_null_and_drops_repeats_of_a_set_one() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host a",
        "  User first",
        "  ForwardAgent yes",
        "  Port 22",
        "  User second",
        "  forwardagent no",
        "",
    ]));
    let patch = HostOptionsPatch {
        forward_agent: Patch::Clear,
        strict_host_key_checking: Patch::Set(StrictHostKeyChecking::AcceptNew),
        ..patch_user("ops")
    };
    patch_hosts(&aliases(&["a"]), &patch, &root, &h.env).unwrap();
    assert_eq!(
        read(&root),
        lines(&[
            "Host a",
            "  User ops",
            "  Port 22",
            "  StrictHostKeyChecking accept-new",
            ""
        ])
    );
    assert_eq!(
        list_hosts(&load_config_document(&root, &h.env), None)[0].options,
        HostBlockOptions {
            user: Some("ops".into()),
            port: Some(22),
            strict_host_key_checking: Some(StrictHostKeyChecking::AcceptNew),
            ..Default::default()
        }
    );
}

#[test]
fn patch_writes_yes_no_flags_and_quotes_a_user_with_spaces() {
    let h = Homes::new();
    let root = h.seed(&lines(&["Host a", "\tHostName a.internal", ""]));
    let patch = HostOptionsPatch {
        forward_agent: Patch::Set(false),
        forward_x11: Patch::Set(true),
        ..patch_user("Jane Doe")
    };
    patch_hosts(&aliases(&["a"]), &patch, &root, &h.env).unwrap();
    assert_eq!(
        read(&root),
        lines(&[
            "Host a",
            "\tHostName a.internal",
            "\tUser \"Jane Doe\"",
            "\tForwardAgent no",
            "\tForwardX11 yes",
            ""
        ])
    );
}

#[test]
fn patch_counts_aliases_that_share_a_block_once() {
    let h = Homes::new();
    let root = h.seed(&lines(&["Host db db-alias", "  HostName db.internal", ""]));
    let patch = HostOptionsPatch {
        port: Patch::Set(2200),
        ..Default::default()
    };
    assert_eq!(
        patch_hosts(&aliases(&["db", "db-alias"]), &patch, &root, &h.env).unwrap(),
        1
    );
    assert_eq!(
        read(&root),
        lines(&[
            "Host db db-alias",
            "  HostName db.internal",
            "  Port 2200",
            ""
        ])
    );
}

#[test]
fn patch_spans_included_files_writing_each_file_once() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host web",
        "  User old",
        "",
        "Host db",
        "  User old",
        "",
    ]));
    let group_file = root.parent().unwrap().join("config.d").join("work");
    upsert_host(
        &HostUpsertRequest {
            aliases: aliases(&["edge"]),
            file: Some(path_string(&group_file)),
            options: HostBlockOptions {
                user: Some("old".into()),
                ..Default::default()
            },
            description: None,
            previous_alias: None,
        },
        &root,
        &h.env,
    )
    .unwrap();
    let before = read(&root);
    assert_eq!(
        patch_hosts(
            &aliases(&["web", "db", "edge"]),
            &patch_user("new"),
            &root,
            &h.env
        )
        .unwrap(),
        3
    );
    let hosts = list_hosts(&load_config_document(&root, &h.env), None);
    for alias in ["web", "db", "edge"] {
        let host = hosts.iter().find(|h| h.alias == alias).unwrap();
        assert_eq!(host.options.user.as_deref(), Some("new"));
    }
    // One write per file: the backup holds the content from before the whole edit.
    assert_eq!(
        read(&PathBuf::from(format!("{}.muxus.bak", path_string(&root)))),
        before
    );
}

#[test]
fn patch_refuses_blocks_included_from_outside_the_root_config_directory() {
    let h = Homes::new();
    let outside = h.dir.path().join(format!("outside-{}", h.next()));
    std::fs::write(&outside, lines(&["Host far", "  User old", ""])).unwrap();
    let root = h.seed(&lines(&[
        &format!("Include {}", path_string(&outside)),
        "",
        "Host near",
        "  User old",
        "",
    ]));
    let err = patch_hosts(
        &aliases(&["near", "far"]),
        &patch_user("new"),
        &root,
        &h.env,
    )
    .unwrap_err();
    assert!(err.message.contains("must live under"));
    assert!(read(&outside).contains("User old"));
    assert!(read(&root).contains("User old"));
}

#[test]
fn patch_leaves_every_file_untouched_when_any_alias_or_value_is_invalid() {
    let h = Homes::new();
    let content = lines(&["Host web", "  User old", ""]);
    let root = h.seed(&content);
    let msg = |a: &[&str], p: HostOptionsPatch| {
        patch_hosts(&aliases(a), &p, &root, &h.env)
            .unwrap_err()
            .message
    };
    assert!(msg(&["web", "missing"], patch_user("new")).contains("no Host block for \"missing\""));
    assert!(msg(&["web"], patch_user("a\"b")).contains("without quotes"));
    assert!(
        msg(
            &["web"],
            HostOptionsPatch {
                port: Patch::Set(0),
                ..Default::default()
            }
        )
        .contains("port")
    );
    assert!(msg(&["web"], HostOptionsPatch::default()).contains("at least one option"));
    assert_eq!(read(&root), content);
}

// --- deleteHost / deleteHosts ---

#[test]
fn delete_removes_the_block_including_its_prelude_comment_collapsing_blanks() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host a",
        "  User alpha",
        "",
        "# doomed",
        "Host web",
        "  HostName web.example.com",
        "",
        "Host z",
        "  User zed",
        "",
    ]));
    delete_host("web", &root, &h.env).unwrap();
    assert_eq!(
        read(&root),
        lines(&["Host a", "  User alpha", "", "Host z", "  User zed", ""])
    );
}

#[test]
fn delete_404s_for_unknown_aliases() {
    let h = Homes::new();
    let root = h.seed("Host a\n  User alpha\n");
    let err = delete_host("nope", &root, &h.env).unwrap_err();
    assert!(err.message.contains("no Host block"));
    assert_eq!(err.status, 404);
}

#[test]
fn delete_hosts_removes_every_listed_block() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host a",
        "  User alpha",
        "",
        "# doomed",
        "Host web",
        "  HostName web.example.com",
        "",
        "Host keep",
        "  User kept",
        "",
        "Host db",
        "  HostName db.internal",
        "",
    ]));
    assert_eq!(
        delete_hosts(&aliases(&["db", "web"]), &root, &h.env).unwrap(),
        2
    );
    assert_eq!(
        read(&root),
        lines(&["Host a", "  User alpha", "", "Host keep", "  User kept", ""])
    );
}

#[test]
fn delete_hosts_spans_included_files_writing_each_file_once() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host web",
        "  User old",
        "",
        "Host db",
        "  User old",
        "",
        "Host z",
        "  User zed",
        "",
    ]));
    let group_file = root.parent().unwrap().join("config.d").join("work");
    upsert_host(
        &HostUpsertRequest {
            aliases: aliases(&["edge"]),
            file: Some(path_string(&group_file)),
            options: HostBlockOptions {
                user: Some("old".into()),
                ..Default::default()
            },
            description: None,
            previous_alias: None,
        },
        &root,
        &h.env,
    )
    .unwrap();
    let before = read(&root);
    assert_eq!(
        delete_hosts(&aliases(&["web", "db", "edge"]), &root, &h.env).unwrap(),
        3
    );
    let names: Vec<String> = list_hosts(&load_config_document(&root, &h.env), None)
        .into_iter()
        .map(|h| h.alias)
        .collect();
    assert_eq!(names, aliases(&["z"]));
    assert_eq!(
        read(&PathBuf::from(format!("{}.muxus.bak", path_string(&root)))),
        before
    );
}

#[test]
fn delete_hosts_counts_aliases_that_share_a_block_once() {
    let h = Homes::new();
    let root = h.seed(&lines(&[
        "Host db db-alias",
        "  HostName db.internal",
        "",
        "Host z",
        "  User zed",
        "",
    ]));
    assert_eq!(
        delete_hosts(&aliases(&["db", "db-alias"]), &root, &h.env).unwrap(),
        1
    );
    assert_eq!(read(&root), lines(&["Host z", "  User zed", ""]));
}

#[test]
fn delete_hosts_leaves_the_file_untouched_when_any_alias_is_unknown() {
    let h = Homes::new();
    let content = lines(&["Host web", "  User old", ""]);
    let root = h.seed(&content);
    let err = delete_hosts(&aliases(&["web", "missing"]), &root, &h.env).unwrap_err();
    assert!(err.message.contains("no Host block for \"missing\""));
    assert_eq!(read(&root), content);
}

// --- previewHost ---

#[test]
fn preview_renders_exactly_what_upsert_would_write() {
    let h = Homes::new();
    let root = h.seed("");
    let text = preview_host(
        &HostUpsertRequest {
            description: Some("Web box".into()),
            ..req()
        },
        &root,
        &h.env,
    )
    .unwrap();
    assert_eq!(
        text,
        lines(&[
            "# Web box",
            "Host web",
            "  HostName web.example.com",
            "  User deploy",
            "  Port 2222"
        ])
    );
    assert!(!PathBuf::from(format!("{}.muxus.tmp", path_string(&root))).exists());
}

#[test]
fn preview_renders_modeled_authentication_security_and_startup_options() {
    let h = Homes::new();
    let root = h.seed("");
    let text = preview_host(
        &HostUpsertRequest {
            options: HostBlockOptions {
                identity_files: Some(aliases(&["~/.ssh/app"])),
                identities_only: Some(true),
                certificate_files: Some(aliases(&["~/.ssh/app-cert.pub"])),
                identity_agent: Some("${ONEPASSWORD_SSH_AUTH_SOCK}".into()),
                proxy_command: Some("cloudflared access ssh --hostname %h".into()),
                strict_host_key_checking: Some(StrictHostKeyChecking::AcceptNew),
                remote_command: Some("tmux new -A -s main".into()),
                request_tty: Some(crate::types::RequestTty::Yes),
                forward_x11: Some(false),
                ..Default::default()
            },
            ..req()
        },
        &root,
        &h.env,
    )
    .unwrap();
    for expected in [
        "  ForwardX11 no",
        "  IdentityFile ~/.ssh/app",
        "  IdentitiesOnly yes",
        "  CertificateFile ~/.ssh/app-cert.pub",
        "  IdentityAgent ${ONEPASSWORD_SSH_AUTH_SOCK}",
        "  ProxyCommand cloudflared access ssh --hostname %h",
        "  StrictHostKeyChecking accept-new",
        "  RemoteCommand tmux new -A -s main",
        "  RequestTTY yes",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in {text}");
    }
}

#[test]
fn preview_double_quotes_paths_containing_whitespace_or_apostrophes() {
    let h = Homes::new();
    let root = h.seed("");
    let windows_key = r"C:\Users\toweber\OneDrive - Nokia\SSH Key\toweber";
    let apostrophe_key = r"C:\Users\O'Neil\.ssh\id_ed25519";
    let request = HostUpsertRequest {
        options: HostBlockOptions {
            user: Some("O'Neil".into()),
            identity_files: Some(aliases(&[windows_key, apostrophe_key])),
            ..Default::default()
        },
        ..req()
    };
    let text = preview_host(&request, &root, &h.env).unwrap();
    assert!(text.contains("  User \"O'Neil\""));
    assert!(text.contains(&format!("  IdentityFile \"{windows_key}\"")));
    assert!(text.contains(&format!("  IdentityFile \"{apostrophe_key}\"")));

    upsert_host(&request, &root, &h.env).unwrap();
    let host = list_hosts(&load_config_document(&root, &h.env), None).remove(0);
    assert_eq!(host.options.user.as_deref(), Some("O'Neil"));
    assert_eq!(host.resolved.user.as_deref(), Some("O'Neil"));
    assert_eq!(
        host.options.identity_files,
        Some(aliases(&[windows_key, apostrophe_key]))
    );
}
