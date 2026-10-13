//! Port of tests/unit/server/folder-auth.test.ts (the buildChain cases live with the connection
//! chain tests) and the resolver part of folder-settings.test.ts.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::config::{list_hosts, resolve_host};
use crate::folder_paths::folder_path_key;

fn auth(f: impl FnOnce(&mut FolderAuthSettings)) -> FolderAuthSettings {
    let mut a = FolderAuthSettings::default();
    f(&mut a);
    a
}

#[test]
fn merge_lets_the_nearest_folder_win_per_field_filling_gaps_from_ancestors() {
    let merged = merge_folder_auth(&[
        auth(|a| a.port = Some(2222)),
        auth(|a| {
            a.user = Some("parent".into());
            a.port = Some(22);
            a.forward_agent = Some(true);
        }),
    ]);
    assert_eq!(
        merged,
        auth(|a| {
            a.user = Some("parent".into());
            a.port = Some(2222);
            a.forward_agent = Some(true);
        })
    );
}

#[test]
fn option_lines_render_one_line_per_set_field_and_quote_paths() {
    let lines = folder_auth_option_lines(&auth(|a| {
        a.user = Some("admin".into());
        a.port = Some(2222);
        a.identity_files = Some(vec!["/keys/my key".into(), "/keys/O'Neil".into()]);
        a.identities_only = Some(true);
    }));
    let pairs: Vec<(&str, &str)> = lines
        .iter()
        .map(|l| (l.key.as_str(), l.value.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("user", "admin"),
            ("port", "2222"),
            ("identityfile", "\"/keys/my key\""),
            ("identityfile", "\"/keys/O'Neil\""),
            ("identitiesonly", "yes"),
        ]
    );
    assert_eq!(lines[2].args, vec!["/keys/my key".to_string()]);
    assert_eq!(lines[3].args, vec!["/keys/O'Neil".to_string()]);
}

// --- resolveHost with folder fallback ---

fn basenames(files: &[String]) -> Vec<String> {
    files
        .iter()
        .map(|f| {
            std::path::Path::new(f)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn fallback_fills_user_and_port_the_config_leaves_unset() {
    let f = crate::config::tests::Fixture::new();
    let doc = f.doc("Host web\n  HostName web.example.com");
    let fallback = folder_auth_option_lines(&auth(|a| {
        a.user = Some("admin".into());
        a.port = Some(2222);
    }));
    let resolved = resolve_host(&doc, "web", Some(&fallback));
    assert_eq!(resolved.user.as_deref(), Some("admin"));
    assert_eq!(resolved.port, 2222);
}

#[test]
fn fallback_never_beats_the_host_block_or_host_star_defaults() {
    let f = crate::config::tests::Fixture::new();
    let doc = f.doc("Host web\n  User blockuser\n\nHost *\n  Port 2200");
    let fallback = folder_auth_option_lines(&auth(|a| {
        a.user = Some("admin".into());
        a.port = Some(9999);
    }));
    let resolved = resolve_host(&doc, "web", Some(&fallback));
    assert_eq!(resolved.user.as_deref(), Some("blockuser"));
    assert_eq!(resolved.port, 2200);
}

#[test]
fn fallback_omits_folder_keys_when_ssh_config_already_supplies_one() {
    let f = crate::config::tests::Fixture::new();
    let doc = f.doc("Host web\n  IdentityFile ~/.ssh/host_key");
    let fallback = folder_auth_option_lines(&auth(|a| {
        a.identity_files = Some(vec!["~/.ssh/folder_key".into()])
    }));
    let resolved = resolve_host(&doc, "web", Some(&fallback));
    assert_eq!(basenames(&resolved.identity_files), vec!["host_key"]);
}

#[test]
fn fallback_supplies_a_key_and_identities_only_to_hosts_with_none_of_their_own() {
    let f = crate::config::tests::Fixture::new();
    let doc = f.doc("Host web\n  HostName web.example.com");
    let fallback = folder_auth_option_lines(&auth(|a| {
        a.identity_files = Some(vec!["~/.ssh/folder_key".into()]);
        a.identities_only = Some(true);
    }));
    let resolved = resolve_host(&doc, "web", Some(&fallback));
    assert_eq!(basenames(&resolved.identity_files), vec!["folder_key"]);
    assert!(resolved.identities_only);
}

#[test]
fn fallback_passes_an_agent_socket_path_containing_spaces_through_intact() {
    let f = crate::config::tests::Fixture::new();
    let doc = f.doc("Host web\n  HostName web.example.com");
    let fallback = folder_auth_option_lines(&auth(|a| {
        a.identity_agent = Some("/tmp/agent dir/agent.sock".into())
    }));
    let resolved = resolve_host(&doc, "web", Some(&fallback));
    assert_eq!(
        resolved.identity_agent.as_deref(),
        Some("/tmp/agent dir/agent.sock")
    );
}

// --- folderAuthResolver ---

struct Source {
    settings: HashMap<String, FolderSettingsRow>,
    groups: HashMap<String, String>,
    saved_groups: HashMap<String, String>,
    lookups: Mutex<Vec<String>>,
}

impl Source {
    fn new(groups: &[(&str, &str)]) -> Self {
        let row = |id: &str, path: &str, auth: FolderAuthSettings| FolderSettingsRow {
            id: id.into(),
            path: path.into(),
            auth,
        };
        let settings = HashMap::from([
            (
                "prod".to_string(),
                row(
                    "row-prod",
                    "Prod",
                    auth(|a| {
                        a.user = Some("root".into());
                        a.port = Some(2200);
                    }),
                ),
            ),
            (
                "prod/eu".to_string(),
                row("row-eu", "Prod/EU", auth(|a| a.port = Some(2222))),
            ),
        ]);
        Self {
            settings,
            groups: groups
                .iter()
                .map(|(a, g)| (a.to_string(), g.to_string()))
                .collect(),
            saved_groups: HashMap::from([("saved-1".to_string(), "Prod/EU".to_string())]),
            lookups: Mutex::new(Vec::new()),
        }
    }
}

impl FolderAuthSource for Source {
    fn group_for_alias(&self, alias: &str) -> Option<String> {
        if let Some(group) = self.groups.get(alias) {
            return Some(group.clone());
        }
        if alias.starts_with("web") {
            return self.groups.get("web*").cloned();
        }
        if alias.starts_with("db") {
            return self.groups.get("db*").cloned();
        }
        None
    }

    fn folder_settings_for_path(&self, path: &str) -> Option<FolderSettingsRow> {
        self.lookups.lock().unwrap().push(path.to_string());
        self.settings.get(&folder_path_key(path)).cloned()
    }

    fn group_for_saved_host(&self, id: &str) -> Option<String> {
        self.saved_groups.get(id).cloned()
    }
}

fn by_key(defaults: &FolderAuthDefaults) -> Vec<(String, String)> {
    defaults
        .option_lines
        .iter()
        .map(|l| (l.key.clone(), l.args[0].clone()))
        .collect()
}

#[test]
fn resolver_collapses_the_ancestor_chain_nearest_first_and_lists_all_passwords() {
    let resolver = folder_auth_resolver(Arc::new(Source::new(&[("web", "Prod/EU")])));
    let defaults = resolver("web").unwrap();
    assert_eq!(
        by_key(&defaults),
        vec![
            ("user".into(), "root".into()),
            ("port".into(), "2222".into())
        ]
    );
    assert_eq!(
        defaults.passwords,
        vec![
            FolderPasswordRef {
                account: folder_password_account("row-eu"),
                label: "Folder Prod/EU".into()
            },
            FolderPasswordRef {
                account: folder_password_account("row-prod"),
                label: "Folder Prod".into()
            },
        ]
    );
}

#[test]
fn resolver_returns_nothing_for_hosts_outside_folders_or_folders_without_settings() {
    let resolver = folder_auth_resolver(Arc::new(Source::new(&[("web", "Prod/EU")])));
    assert!(resolver("bare").is_none());
    let empty = folder_auth_resolver(Arc::new(Source::new(&[("web", "Unrelated")])));
    assert!(empty("web").is_none());
}

#[test]
fn resolver_resolves_the_same_defaults_for_a_saved_profile_through_its_database_id() {
    let resolver = saved_profile_folder_auth_resolver(Arc::new(Source::new(&[])));
    let defaults = resolver("saved-1").unwrap();
    assert_eq!(
        by_key(&defaults),
        vec![
            ("user".into(), "root".into()),
            ("port".into(), "2222".into())
        ]
    );
    assert!(resolver("missing").is_none());
}

#[test]
fn batch_resolver_reads_each_folder_chain_once_when_listing_many_hosts() {
    let source = Source::new(&[("web*", "Prod/EU"), ("db*", "Prod")]);
    let aliases = ["web-1", "web-2", "db-1", "web-3", "db-2", "bare"];
    let expected: Vec<_> = aliases
        .iter()
        .map(|a| folder_auth_for_group(&source, source.group_for_alias(a).as_deref()))
        .collect();
    source.lookups.lock().unwrap().clear();

    let mut batch = batch_folder_auth_resolver(&source);
    let got: Vec<_> = aliases.iter().map(|a| batch(a)).collect();
    assert_eq!(got, expected);
    // Prod/EU walks two folders, Prod one: once each, not once per host.
    assert_eq!(
        *source.lookups.lock().unwrap(),
        vec!["Prod/EU", "Prod", "Prod"]
    );
}

#[test]
fn account_encoding_matches_the_typescript_vault() {
    // Buffer.from(JSON.stringify(['folder', 'row-1'])).toString('base64url')
    assert_eq!(folder_password_account("row-1"), "WyJmb2xkZXIiLCJyb3ctMSJd");
    // Buffer.from(JSON.stringify(['tester', '127.0.0.1', 22])).toString('base64url')
    assert_eq!(
        ssh_password_account("tester", "127.0.0.1", 22),
        "WyJ0ZXN0ZXIiLCIxMjcuMC4wLjEiLDIyXQ"
    );
    assert_eq!(ssh_password_label("tester", "Host", 22), "tester@Host:22");
}

/// folder-settings.test.ts: "feeds folder defaults into the resolved host list".
#[test]
fn feeds_folder_defaults_into_the_resolved_host_list() {
    let f = crate::config::tests::Fixture::new();
    let doc = f.doc("Host web\n  HostName web.example.com\n\nHost other\n  User own\n");
    let source = Source::new(&[("web", "Prod/EU"), ("other", "Prod")]);
    let mut batch = batch_folder_auth_resolver(&source);
    let mut fallback = |alias: &str| batch(alias).map(|d| d.option_lines);
    let hosts = list_hosts(&doc, Some(&mut fallback));
    let web = hosts.iter().find(|h| h.alias == "web").unwrap();
    assert_eq!(web.resolved.user.as_deref(), Some("root"));
    assert_eq!(web.resolved.port, 2222);
    let other = hosts.iter().find(|h| h.alias == "other").unwrap();
    assert_eq!(other.resolved.user.as_deref(), Some("own"));
    assert_eq!(other.resolved.port, 2200);
}
