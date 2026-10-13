//! Port of tests/unit/server/database.test.ts, the persistence parts of
//! folder-settings.test.ts and login-sequence-storage.test.ts.

use muxus_core::api::ForwardType;
use muxus_core::api::{
    FolderAuthSettings, HostKeywordHighlightConfig, KeywordHighlightRule, LoginSendStep,
    LoginSequence, LoginSequenceStep, LoginWaitStep, ManagedHostRef, OpenSshMetadataPatch,
    SavedHostProfileInput, SessionHistorySettings, SessionLogFileSettings,
    SessionLoggingPolicyInput, TunnelInput, TunnelSshOptions, WorkspaceMultiExecGroup,
};
use muxus_core::js::Patch;
use muxus_core::management::{ManagementProtocol, SavedManagementRequestInput};
use muxus_core::protocol::{
    GnmiProfile, NetconfProfile, RdpProfile, SavedHostSessionProfile, SerialFlowControl,
    SerialProfile, SshGateway, SshProfile, TelnetProfile, VncProfile,
};
use muxus_store::Database;
use muxus_store::db::{
    CredentialRefInput, DbError, EncryptedCredentialInput, EncryptedCredentialSecret, HostRef,
    NativeConnectionInput, NativeConnectionKind, WorkspaceInput,
};
use rusqlite::Connection;
use serde_json::{Value, json};

fn memory() -> Database {
    Database::open_in_memory().unwrap()
}

fn telnet(host: &str, port: u16) -> SavedHostSessionProfile {
    SavedHostSessionProfile::Telnet(TelnetProfile {
        profile_id: None,
        host: host.into(),
        port,
    })
}

fn serial(path: &str) -> SavedHostSessionProfile {
    SavedHostSessionProfile::Serial(SerialProfile::new(path))
}

fn saved(name: &str, profile: SavedHostSessionProfile) -> SavedHostProfileInput {
    SavedHostProfileInput {
        id: None,
        name: name.into(),
        profile,
    }
}

fn policy(
    enabled: bool,
    capture_input: bool,
    max_part_bytes: u64,
    max_parts: u32,
) -> SessionLoggingPolicyInput {
    SessionLoggingPolicyInput {
        enabled,
        capture_input,
        max_part_bytes,
        max_parts,
        log_to_file: None,
    }
}

fn workspace(id: Option<&str>, name: &str, layout: Value) -> WorkspaceInput {
    WorkspaceInput {
        id: id.map(str::to_owned),
        name: name.into(),
        layout,
        multi_exec_groups: None,
    }
}

fn file_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("muxus.sqlite3");
    (dir, path)
}

mod migrations {
    use super::*;

    #[test]
    fn applies_the_versioned_schema() {
        let names: Vec<(i64, String)> = memory()
            .applied_migrations()
            .unwrap()
            .into_iter()
            .map(|m| (m.version, m.name))
            .collect();
        let expected = [
            "foundation",
            "tunnels",
            "host-sort-order",
            "tunnel-ssh-options",
            "host-keyword-highlights",
            "persistent-session-history",
            "bounded-session-history-settings",
            "named-workspace-session-sets",
            "drop-favorites",
            "terminal-scrollback-snapshots",
            "version-terminal-scrollback-snapshots",
            "password-vault",
            "automatic-password-vault",
            "password-vault-os-keystore",
            "password-vault-key-check",
            "password-vault-key-cleanup",
            "folder-settings",
            "lock-workspaces",
            "host-disable-sftp",
            "host-console-compatibility",
            "host-terminal-appearance",
            "remote-desktop-hosts",
            "session-log-files",
            "host-command-button-group",
            "tunnel-autostart-reconnect",
            "credential-usernames",
            "login-sequences",
            "host-paste-pacing",
            "management-hosts",
        ];
        let expected: Vec<(i64, String)> = expected
            .iter()
            .enumerate()
            .map(|(index, name)| (index as i64 + 1, (*name).to_owned()))
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn keeps_version_22_logging_policies_when_adding_log_files() {
        let (_dir, path) = file_db();
        Database::open(&path).unwrap().close().unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "
        DELETE FROM schema_migrations WHERE version = 23;
        DROP TABLE session_log_file_settings;
        ALTER TABLE session_logging_policies DROP COLUMN log_to_file;
        INSERT INTO session_logging_policies(
          profile_key, enabled, capture_input, max_part_bytes, max_parts
        ) VALUES ('ssh:production', 1, 1, 1048576, 3);
        PRAGMA user_version = 22;
      ",
            )
            .unwrap();
        let db = Database::open(&path).unwrap();
        let policy = db.session_logging_policy("ssh:production").unwrap();
        assert!(policy.enabled && policy.capture_input && policy.overridden && !policy.log_to_file);
        assert_eq!(policy.max_parts, 3);
        assert_eq!(
            db.session_log_file_settings().unwrap(),
            SessionLogFileSettings {
                directory: None,
                filename_pattern: "{host}_{date}_{time}.log".into(),
                timestamps: false
            }
        );
    }

    /// Restore a pre-rebuild connection_profiles: the older kind CHECK.
    fn downgrade_kinds(conn: &Connection, from: &str, to: &str, table: &str, extra: &str) {
        let current: String = conn
            .query_row("SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'connection_profiles'", [], |row| row.get(0))
            .unwrap();
        let indexes: Vec<String> = {
            let mut statement = conn
                .prepare("SELECT sql FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'connection_profiles' AND sql IS NOT NULL")
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let definition = current.replace(from, to).replacen(
            "CREATE TABLE \"connection_profiles\"",
            &format!("CREATE TABLE {table}"),
            1,
        );
        conn.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
        conn.execute_batch(&format!(
            "{definition};
        INSERT INTO {table} SELECT * FROM connection_profiles;
        DROP TABLE connection_profiles;
        ALTER TABLE {table} RENAME TO connection_profiles;
        {extra}"
        ))
        .unwrap();
        for index in indexes {
            conn.execute_batch(&index).unwrap();
        }
    }

    #[test]
    fn rebuilds_version_21_connection_profiles_to_admit_rdp_and_vnc_hosts() {
        let (_dir, path) = file_db();
        let db = Database::open(&path).unwrap();
        let telnet_host = db
            .save_saved_host_profile(&saved("Core switch", telnet("switch.lab", 23)))
            .unwrap();
        db.update_saved_host_metadata(
            &telnet_host.id,
            &OpenSshMetadataPatch {
                group: Patch::Set("Lab".into()),
                color: Patch::Set("#ff0000".into()),
                ..Default::default()
            },
        )
        .unwrap();
        db.record_saved_host_connection(&telnet_host.id).unwrap();
        db.update_open_ssh_metadata(
            "bastion",
            &OpenSshMetadataPatch {
                display_name: Patch::Set("Bastion".into()),
                disable_sftp: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        db.close().unwrap();

        {
            let conn = Connection::open(&path).unwrap();
            downgrade_kinds(
                &conn,
                "'telnet', 'rdp', 'vnc', 'gnmi', 'netconf')",
                "'telnet')",
                "connection_profiles_v21",
                "DROP TABLE remote_desktop_certificates;
        DELETE FROM schema_migrations WHERE version = 22;
        PRAGMA user_version = 21;",
            );
            conn.execute_batch(&format!(
                "INSERT INTO tags(id, name) VALUES ('tag-1', 'core');
        INSERT INTO connection_tags(connection_id, tag_id) VALUES ('{}', 'tag-1');",
                telnet_host.id
            ))
            .unwrap();
            assert!(
                conn.execute_batch(
                    "INSERT INTO connection_profiles(id, kind, name, native_config_json) VALUES ('too-early', 'rdp', 'x', '{}')"
                )
                .unwrap_err()
                .to_string()
                .contains("CHECK")
            );
        }

        let db = Database::open(&path).unwrap();
        assert_eq!(db.applied_migrations().unwrap().last().unwrap().version, 29);
        let restored = db.saved_host_profile(&telnet_host.id).unwrap().unwrap();
        assert_eq!(restored.name, "Core switch");
        assert_eq!(restored.metadata.group.as_deref(), Some("Lab"));
        assert_eq!(restored.metadata.color.as_deref(), Some("#ff0000"));
        assert_eq!(restored.metadata.connect_count, 1);
        let bastion = db.open_ssh_metadata(&["bastion"]).unwrap().remove(0).1;
        assert_eq!(bastion.display_name.as_deref(), Some("Bastion"));
        assert_eq!(bastion.disable_sftp, Some(true));
        let rdp = db
            .save_saved_host_profile(&saved(
                "Build server",
                SavedHostSessionProfile::Rdp(RdpProfile {
                    profile_id: None,
                    host: "win-build".into(),
                    port: 3389,
                    username: Some("ci".into()),
                    domain: None,
                    ssh_gateway: Some(SshGateway {
                        target: "bastion".into(),
                        profile_id: None,
                    }),
                    share_clipboard: None,
                }),
            ))
            .unwrap();
        let vnc = db
            .save_saved_host_profile(&saved(
                "Lab console",
                SavedHostSessionProfile::Vnc(VncProfile {
                    profile_id: None,
                    host: "lab-vm".into(),
                    port: 5901,
                    username: None,
                    ssh_gateway: None,
                    resize_remote: None,
                    view_only: Some(true),
                    share_clipboard: None,
                }),
            ))
            .unwrap();
        let mut kinds: Vec<&str> = db
            .list_saved_host_profiles()
            .unwrap()
            .iter()
            .map(|profile| profile.kind.as_str())
            .collect();
        kinds.sort();
        assert_eq!(kinds, ["rdp", "telnet", "vnc"]);
        let SavedHostSessionProfile::Rdp(stored) =
            db.saved_host_profile(&rdp.id).unwrap().unwrap().profile
        else {
            panic!("rdp")
        };
        assert_eq!(stored.ssh_gateway.unwrap().target, "bastion");
        assert_eq!(stored.profile_id.as_deref(), Some(rdp.id.as_str()));
        assert!(db.delete_saved_host_profile(&vnc.id).unwrap());
        db.close().unwrap();

        let check = Connection::open(&path).unwrap();
        let tags: Vec<(String, String)> = {
            let mut statement = check
                .prepare("SELECT connection_id, tag_id FROM connection_tags")
                .unwrap();
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(tags, vec![(telnet_host.id.clone(), "tag-1".to_owned())]);
        let violations: i64 = check
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
        let indexes: Vec<String> = {
            let mut statement = check
                .prepare("SELECT name FROM sqlite_schema WHERE type = 'index' AND tbl_name = 'connection_profiles'")
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert!(indexes.contains(&"connection_profiles_recent".to_owned()));
        assert!(indexes.contains(&"connection_profiles_group".to_owned()));
    }

    #[test]
    fn rebuilds_version_28_connection_profiles_to_admit_gnmi_and_netconf_hosts() {
        let (_dir, path) = file_db();
        let db = Database::open(&path).unwrap();
        let rdp = db
            .save_saved_host_profile(&saved(
                "Build server",
                SavedHostSessionProfile::Rdp(RdpProfile {
                    profile_id: None,
                    host: "win-build".into(),
                    port: 3389,
                    username: None,
                    domain: None,
                    ssh_gateway: None,
                    share_clipboard: None,
                }),
            ))
            .unwrap();
        db.update_saved_host_metadata(
            &rdp.id,
            &OpenSshMetadataPatch {
                group: Patch::Set("Lab".into()),
                ..Default::default()
            },
        )
        .unwrap();
        db.close().unwrap();

        {
            let conn = Connection::open(&path).unwrap();
            downgrade_kinds(
                &conn,
                "'vnc', 'gnmi', 'netconf')",
                "'vnc')",
                "connection_profiles_v28",
                "DROP TABLE management_requests;
        DELETE FROM schema_migrations WHERE version = 29;
        PRAGMA user_version = 28;",
            );
            conn.execute_batch(&format!(
                "INSERT INTO tags(id, name) VALUES ('tag-1', 'windows');
        INSERT INTO connection_tags(connection_id, tag_id) VALUES ('{}', 'tag-1');",
                rdp.id
            ))
            .unwrap();
            assert!(
                conn.execute_batch(
                    "INSERT INTO connection_profiles(id, kind, name, native_config_json) VALUES ('too-early', 'gnmi', 'x', '{}')"
                )
                .is_err()
            );
        }

        let db = Database::open(&path).unwrap();
        assert_eq!(
            db.applied_migrations().unwrap().last().unwrap().name,
            "management-hosts"
        );
        assert_eq!(
            db.saved_host_profile(&rdp.id)
                .unwrap()
                .unwrap()
                .metadata
                .group
                .as_deref(),
            Some("Lab")
        );
        let gnmi = db
            .save_saved_host_profile(&saved(
                "leaf1",
                SavedHostSessionProfile::Gnmi(GnmiProfile {
                    profile_id: None,
                    host: "leaf1".into(),
                    port: 57400,
                    username: Some("admin".into()),
                    tls: Some(muxus_core::management::GnmiTlsMode::SkipVerify),
                    tls_server_name: None,
                    ca_file: Some("/lab/ca.pem".into()),
                    cert_file: None,
                    key_file: None,
                    ssh_gateway: None,
                    encoding: None,
                }),
            ))
            .unwrap();
        let netconf = db
            .save_saved_host_profile(&saved(
                "leaf1",
                SavedHostSessionProfile::Netconf(NetconfProfile {
                    profile_id: None,
                    host: "leaf1".into(),
                    port: 830,
                    username: None,
                    ssh_gateway: Some(SshGateway {
                        target: "bastion".into(),
                        profile_id: None,
                    }),
                }),
            ))
            .unwrap();
        assert!(matches!(
            db.saved_host_profile(&gnmi.id).unwrap().unwrap().profile,
            SavedHostSessionProfile::Gnmi(_)
        ));
        assert!(matches!(
            db.saved_host_profile(&netconf.id).unwrap().unwrap().profile,
            SavedHostSessionProfile::Netconf(_)
        ));

        let request = |name: &str, profile_id: Option<&str>, protocol, body: Value| {
            SavedManagementRequestInput {
                id: None,
                protocol,
                profile_id: profile_id.map(str::to_owned),
                name: name.into(),
                request: body.as_object().unwrap().clone(),
            }
        };
        let own = db
            .save_management_request(&request(
                "Counters",
                Some(&gnmi.id),
                ManagementProtocol::Gnmi,
                json!({ "operation": "subscribe", "paths": ["/interface[name=*]/statistics"] }),
            ))
            .unwrap();
        db.save_management_request(&request(
            "System",
            None,
            ManagementProtocol::Gnmi,
            json!({ "operation": "get", "paths": ["/system"] }),
        ))
        .unwrap();
        db.save_management_request(&request(
            "Running",
            None,
            ManagementProtocol::Netconf,
            json!({ "operation": "get-config" }),
        ))
        .unwrap();
        let names = |profile: Option<&str>| -> Vec<String> {
            db.list_management_requests(ManagementProtocol::Gnmi, profile)
                .unwrap()
                .into_iter()
                .map(|r| r.name)
                .collect()
        };
        assert_eq!(names(Some(&gnmi.id)), ["Counters", "System"]);
        assert_eq!(names(None), ["System"]);
        let renamed = SavedManagementRequestInput {
            id: Some(own.id.clone()),
            protocol: own.protocol,
            profile_id: own.profile_id.clone(),
            name: "Interface counters".into(),
            request: own.request.clone(),
        };
        assert_eq!(
            db.save_management_request(&renamed).unwrap().name,
            "Interface counters"
        );
        assert!(
            db.save_management_request(&request(
                "Leaky",
                None,
                ManagementProtocol::Gnmi,
                json!({ "password": "secret" })
            ))
            .is_err()
        );
        // A deleted host takes its own requests along, not the shared ones.
        assert!(db.delete_saved_host_profile(&gnmi.id).unwrap());
        assert_eq!(names(Some(&gnmi.id)), ["System"]);
        db.close().unwrap();

        let check = Connection::open(&path).unwrap();
        let tagged: String = check
            .query_row("SELECT connection_id FROM connection_tags", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(tagged, rdp.id);
    }

    #[test]
    fn preserves_version_19_disable_sftp_records_when_adding_console_compatibility() {
        let (_dir, path) = file_db();
        let db = Database::open(&path).unwrap();
        db.update_open_ssh_metadata(
            "legacy",
            &OpenSshMetadataPatch {
                disable_sftp: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
        db.close().unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "DELETE FROM schema_migrations WHERE version = 20;
        ALTER TABLE connection_profiles DROP COLUMN console_compatibility;
        PRAGMA user_version = 19;",
            )
            .unwrap();
        let db = Database::open(&path).unwrap();
        let legacy = db.open_ssh_metadata(&["legacy"]).unwrap().remove(0).1;
        assert_eq!(legacy.disable_sftp, Some(true));
        assert_eq!(legacy.console_compatibility, None);
        assert!(db.sftp_disabled_for_alias("legacy").unwrap());
        assert!(!db.console_compatibility_for_alias("legacy").unwrap());
    }

    #[test]
    fn upgrades_the_draft_version_13_vault_without_deleting_credentials() {
        let (_dir, path) = file_db();
        let db = Database::open(&path).unwrap();
        db.upsert_encrypted_credential(&EncryptedCredentialInput {
            reference: CredentialRefInput {
                provider: "muxus-master-vault".into(),
                service: "muxus/ssh-password/v1".into(),
                account: "legacy-account".into(),
                label: Some("legacy credential".into()),
                username: None,
            },
            secret: EncryptedCredentialSecret {
                format_version: 1,
                nonce: vec![1; 12],
                ciphertext: b"legacy-ciphertext".to_vec(),
                auth_tag: vec![2; 16],
            },
        })
        .unwrap();
        db.close().unwrap();

        {
            let draft = Connection::open(&path).unwrap();
            draft
                .execute_batch(
                    "
        DELETE FROM schema_migrations WHERE version IN (13, 14, 15, 16);
        UPDATE schema_migrations
        SET name = 'master-password-vault'
        WHERE version = 12;
        DROP TABLE password_vault_key_cleanup;
        DROP TABLE password_vault;
        CREATE TABLE password_vault (
          singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
          format_version INTEGER NOT NULL CHECK(format_version = 2),
          kdf_algorithm TEXT NOT NULL CHECK(kdf_algorithm = 'scrypt'),
          kdf_salt BLOB NOT NULL CHECK(length(kdf_salt) = 16),
          kdf_cost INTEGER NOT NULL CHECK(kdf_cost BETWEEN 1024 AND 1048576),
          kdf_block_size INTEGER NOT NULL CHECK(kdf_block_size BETWEEN 1 AND 32),
          kdf_parallelism INTEGER NOT NULL CHECK(kdf_parallelism BETWEEN 1 AND 16),
          master_key_nonce BLOB NOT NULL CHECK(length(master_key_nonce) = 12),
          master_key_ciphertext BLOB NOT NULL CHECK(length(master_key_ciphertext) = 32),
          master_key_tag BLOB NOT NULL CHECK(length(master_key_tag) = 16),
          device_key_nonce BLOB NOT NULL CHECK(length(device_key_nonce) = 12),
          device_key_ciphertext BLOB NOT NULL CHECK(length(device_key_ciphertext) = 32),
          device_key_tag BLOB NOT NULL CHECK(length(device_key_tag) = 16),
          created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
          updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        ) STRICT;
        INSERT INTO schema_migrations(version, name)
        VALUES (13, 'automatic-password-vault');
        PRAGMA user_version = 13;
      ",
                )
                .unwrap();
            draft
                .execute(
                    "
          INSERT INTO password_vault(
            singleton, format_version, kdf_algorithm, kdf_salt,
            kdf_cost, kdf_block_size, kdf_parallelism,
            master_key_nonce, master_key_ciphertext, master_key_tag,
            device_key_nonce, device_key_ciphertext, device_key_tag
          ) VALUES (1, 2, 'scrypt', ?, 1024, 8, 1, ?, ?, ?, ?, ?, ?)
        ",
                    rusqlite::params![
                        vec![3u8; 16],
                        vec![4u8; 12],
                        vec![5u8; 32],
                        vec![6u8; 16],
                        vec![7u8; 12],
                        vec![8u8; 32],
                        vec![9u8; 16]
                    ],
                )
                .unwrap();
        }

        let db = Database::open(&path).unwrap();
        assert_eq!(db.applied_migrations().unwrap().last().unwrap().version, 29);
        let config = db.password_vault_config().unwrap().unwrap().config;
        assert_eq!(config.format_version, 2);
        assert_eq!(
            config.unlock_policy,
            muxus_core::api::PasswordVaultUnlockPolicy::Startup
        );
        assert_eq!(config.vault_id.len(), 21);
        assert_eq!(config.key_check, None);
        assert_eq!(
            db.list_encrypted_credentials("muxus-master-vault", "muxus/ssh-password/v1")
                .unwrap()
                .len(),
            1
        );
    }
}

mod snapshots {
    use super::*;

    fn layout_with(tab_id: &str) -> Value {
        json!({
            "version": 1,
            "root": {
                "id": "pane-1",
                "type": "pane",
                "tabs": [{
                    "id": tab_id, "kind": "terminal", "title": "Router",
                    "profile": { "kind": "ssh", "target": "router" }, "offerReconnect": true
                }]
            }
        })
    }

    #[test]
    fn stores_and_replaces_one_snapshot_per_tab() {
        let db = memory();
        db.save_terminal_snapshot("tab-1", "first", 1).unwrap();
        assert_eq!(
            db.terminal_snapshot("tab-1")
                .unwrap()
                .unwrap()
                .format_version,
            1
        );
        db.save_terminal_snapshot("tab-1", "second", 2).unwrap();
        let snapshot = db.terminal_snapshot("tab-1").unwrap().unwrap();
        assert_eq!(
            (
                snapshot.tab_id.as_str(),
                snapshot.data.as_str(),
                snapshot.format_version
            ),
            ("tab-1", "second", 2)
        );
        assert_eq!(db.terminal_snapshot("tab-2").unwrap(), None);
    }

    #[test]
    fn prunes_snapshots_no_stored_workspace_references() {
        let db = memory();
        db.save_workspace(&workspace(None, "Ops", layout_with("kept-tab")), false)
            .unwrap();
        db.save_terminal_snapshot("kept-tab", "kept", 1).unwrap();
        db.save_terminal_snapshot("orphan-tab", "orphan", 1)
            .unwrap();
        assert_eq!(db.prune_terminal_snapshots(0).unwrap(), 1);
        assert!(db.terminal_snapshot("kept-tab").unwrap().is_some());
        assert!(db.terminal_snapshot("orphan-tab").unwrap().is_none());
    }

    #[test]
    fn spares_fresh_snapshots_that_may_precede_their_first_layout_autosave() {
        let db = memory();
        db.save_terminal_snapshot("brand-new-tab", "early output", 1)
            .unwrap();
        assert_eq!(
            db.prune_terminal_snapshots(muxus_store::db::DEFAULT_SNAPSHOT_GRACE_SECONDS)
                .unwrap(),
            0
        );
        assert!(db.terminal_snapshot("brand-new-tab").unwrap().is_some());
    }
}

mod logging {
    use super::*;

    #[test]
    fn inherits_safe_defaults_and_supports_per_host_logging_policy_overrides() {
        let db = memory();
        let defaults = db.session_logging_policy("ssh:production").unwrap();
        assert!(!defaults.enabled && !defaults.capture_input && !defaults.overridden);
        assert_eq!(
            (defaults.max_part_bytes, defaults.max_parts),
            (5 * 1024 * 1024, 10)
        );

        db.save_session_logging_policy("*", &policy(true, false, 1024 * 1024, 4))
            .unwrap();
        db.save_session_logging_policy("ssh:production", &policy(false, true, 2 * 1024 * 1024, 2))
            .unwrap();
        let exact = db.session_logging_policy("ssh:production").unwrap();
        assert!(!exact.enabled && exact.capture_input && exact.overridden);
        assert_eq!(
            (exact.max_part_bytes, exact.max_parts),
            (2 * 1024 * 1024, 2)
        );
        let inherited = db.session_logging_policy("ssh:staging").unwrap();
        assert_eq!(
            (
                inherited.max_part_bytes,
                inherited.max_parts,
                inherited.overridden
            ),
            (1024 * 1024, 4, false)
        );
        assert!(db.delete_session_logging_policy("ssh:production").unwrap());
        let back = db.session_logging_policy("ssh:production").unwrap();
        assert_eq!((back.max_parts, back.overridden), (4, false));
    }

    #[test]
    fn inherits_writing_log_files_and_validates_the_log_file_settings() {
        let db = memory();
        db.save_session_logging_policy(
            "*",
            &SessionLoggingPolicyInput {
                log_to_file: Some(true),
                ..policy(false, false, 1024 * 1024, 4)
            },
        )
        .unwrap();
        let inherited = db.session_logging_policy("serial:/dev/ttyUSB0").unwrap();
        assert!(!inherited.enabled && inherited.log_to_file && !inherited.overridden);

        let saved = db
            .save_session_log_file_settings(&SessionLogFileSettings {
                directory: Some(" /srv/logs ".into()),
                filename_pattern: " {kind}/{host}_{date}.log ".into(),
                timestamps: true,
            })
            .unwrap();
        assert_eq!(
            saved,
            SessionLogFileSettings {
                directory: Some("/srv/logs".into()),
                filename_pattern: "{kind}/{host}_{date}.log".into(),
                timestamps: true
            }
        );
        let relative = db.save_session_log_file_settings(&SessionLogFileSettings {
            directory: Some("relative/logs".into()),
            filename_pattern: "{host}.log".into(),
            timestamps: false,
        });
        assert!(relative.unwrap_err().to_string().contains("absolute"));
        let unnamed = db.save_session_log_file_settings(&SessionLogFileSettings {
            directory: None,
            filename_pattern: "../{host}.log".into(),
            timestamps: false,
        });
        assert!(unnamed.unwrap_err().to_string().contains("needs a name"));
    }

    #[test]
    fn validates_policy_and_history_limits() {
        let db = memory();
        assert!(
            db.save_session_logging_policy("*", &policy(true, false, 1024, 4))
                .is_err()
        );
        assert!(
            db.save_session_logging_policy("*", &policy(true, false, 1024 * 1024, 0))
                .is_err()
        );
        assert!(
            db.save_session_logging_policy(" ", &policy(true, false, 1024 * 1024, 1))
                .is_err()
        );
        let defaults = db.session_history_settings().unwrap();
        assert_eq!(defaults.max_total_bytes, 5_368_709_120);
        assert_eq!(defaults.min_free_bytes, 2_147_483_648);
        assert_eq!(defaults.min_free_percent, 5.0);
        let settings = |location: Option<&str>, max_total_bytes: u64| SessionHistorySettings {
            storage_location: location.map(str::to_owned),
            max_total_bytes,
            min_free_bytes: 0,
            min_free_percent: 2.5,
            max_age_days: Some(7),
        };
        assert!(
            db.save_session_history_settings(&settings(Some("relative"), 1 << 30))
                .is_err()
        );
        assert!(
            db.save_session_history_settings(&settings(Some("/"), 1 << 30))
                .is_err()
        );
        assert!(
            db.save_session_history_settings(&settings(None, 1 << 20))
                .is_err()
        );
        let saved = db
            .save_session_history_settings(&settings(Some("/srv/history"), 1 << 30))
            .unwrap();
        assert_eq!(saved.storage_location.as_deref(), Some("/srv/history"));
        assert_eq!(saved.max_age_days, Some(7));
    }
}

mod tunnels {
    use super::*;

    fn rule(kind: ForwardType, bind_port: u16) -> TunnelInput {
        TunnelInput {
            id: None,
            name: None,
            target: "web".into(),
            ssh_options: None,
            kind,
            bind_port,
            target_host: None,
            target_port: None,
            auto_start: None,
            auto_reconnect: None,
        }
    }

    #[test]
    fn round_trips_create_update_and_delete() {
        let db = memory();
        let created = db
            .save_tunnel(&TunnelInput {
                name: Some("DB".into()),
                target_host: Some("localhost".into()),
                target_port: Some(5432),
                ..rule(ForwardType::Local, 5432)
            })
            .unwrap();
        assert_eq!(created.name.as_deref(), Some("DB"));
        assert_eq!(
            (created.target_host.as_deref(), created.target_port),
            (Some("localhost"), Some(5432))
        );
        let updated = db
            .save_tunnel(&TunnelInput {
                id: Some(created.id.clone()),
                ..rule(ForwardType::Dynamic, 1080)
            })
            .unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(
            (updated.name, updated.kind, updated.bind_port),
            (None, ForwardType::Dynamic, 1080)
        );
        assert_eq!((updated.target_host, updated.target_port), (None, None));
        assert_eq!(db.list_tunnels().unwrap().len(), 1);
        assert!(db.delete_tunnel(&created.id).unwrap());
        assert!(db.list_tunnels().unwrap().is_empty());
    }

    #[test]
    fn keeps_autostart_and_auto_reconnect_off_unless_they_are_switched_on() {
        let db = memory();
        let created = db.save_tunnel(&rule(ForwardType::Dynamic, 1080)).unwrap();
        assert!(!created.auto_start && !created.auto_reconnect);
        let id = Some(created.id.clone());
        let started = db
            .save_tunnel(&TunnelInput {
                id: id.clone(),
                auto_start: Some(true),
                ..rule(ForwardType::Dynamic, 1080)
            })
            .unwrap();
        assert!(started.auto_start && !started.auto_reconnect);
        let enabled = db
            .save_tunnel(&TunnelInput {
                id: id.clone(),
                auto_start: Some(true),
                auto_reconnect: Some(true),
                ..rule(ForwardType::Dynamic, 1080)
            })
            .unwrap();
        assert!(enabled.auto_start && enabled.auto_reconnect);
        assert_eq!(db.tunnel(&created.id).unwrap().unwrap(), enabled);
        assert_eq!(db.list_tunnels().unwrap(), vec![enabled]);
        // An update without the flags (an older backup, "Save as tunnel") switches them off.
        let off = db
            .save_tunnel(&TunnelInput {
                id,
                ..rule(ForwardType::Dynamic, 1080)
            })
            .unwrap();
        assert!(!off.auto_start && !off.auto_reconnect);
        assert_eq!(db.tunnel("missing").unwrap(), None);
    }

    #[test]
    fn rejects_local_remote_rules_without_a_tunnel_target() {
        let error = memory()
            .save_tunnel(&rule(ForwardType::Local, 8080))
            .unwrap_err();
        assert!(error.to_string().contains("targetHost"));
    }

    #[test]
    fn persists_a_tunnel_owned_ssh_profile_without_credentials() {
        let db = memory();
        let options = TunnelSshOptions {
            user: Some("deploy".into()),
            port: Some(2222),
            identity_files: Some(vec!["~/.ssh/work_ed25519".into()]),
            identities_only: Some(true),
            forward_agent: None,
            proxy_jump: Some(vec!["bastion".into(), "ops@relay.example.com:2200".into()]),
            password_only: None,
        };
        let created = db
            .save_tunnel(&TunnelInput {
                name: Some("Private database".into()),
                target: "db.internal".into(),
                ssh_options: Some(options.clone()),
                target_host: Some("localhost".into()),
                target_port: Some(5432),
                ..rule(ForwardType::Local, 5432)
            })
            .unwrap();
        assert_eq!(created.ssh_options, Some(options));
        let switched = db
            .save_tunnel(&TunnelInput {
                id: Some(created.id),
                target: "database-config-alias".into(),
                ..rule(ForwardType::Dynamic, 1080)
            })
            .unwrap();
        assert_eq!(switched.ssh_options, None);
    }
}

mod metadata {
    use super::*;

    fn highlight() -> HostKeywordHighlightConfig {
        HostKeywordHighlightConfig {
            inherit_global: true,
            profile_id: None,
            rules: vec![KeywordHighlightRule {
                id: "host-error".into(),
                name: None,
                keyword: "ERROR".into(),
                foreground: "#ffffff".into(),
                background: Some("#b91c1c".into()),
                case_sensitive: false,
                whole_word: true,
                regex: None,
            }],
        }
    }

    fn get(db: &Database, alias: &str) -> muxus_core::api::OpenSshProfileMetadata {
        db.open_ssh_metadata(&[alias]).unwrap().remove(0).1
    }

    #[test]
    fn stores_muxus_metadata_without_copying_connection_details() {
        let db = memory();
        let organized = db
            .update_open_ssh_metadata(
                "production",
                &OpenSshMetadataPatch {
                    display_name: Patch::Set("Production".into()),
                    group: Patch::Set("Work".into()),
                    color: Patch::Set("#3b82f6".into()),
                    terminal_scheme: Patch::Set("nord".into()),
                    terminal_font_color: Patch::Set("#f8f8f2".into()),
                    terminal_background_color: Patch::Set("#282a36".into()),
                    disable_sftp: Some(true),
                    console_compatibility: Some(true),
                    keyword_highlights: Patch::Set(highlight()),
                    ..Default::default()
                },
            )
            .unwrap();
        let connected = db.record_open_ssh_connection("production").unwrap();
        assert_eq!(connected.profile_id, organized.profile_id);
        assert_eq!(connected.display_name.as_deref(), Some("Production"));
        assert_eq!(connected.group.as_deref(), Some("Work"));
        assert_eq!(connected.terminal_scheme.as_deref(), Some("nord"));
        assert_eq!(connected.disable_sftp, Some(true));
        assert_eq!(connected.console_compatibility, Some(true));
        assert_eq!(connected.keyword_highlights, Some(highlight()));
        assert_eq!(connected.connect_count, 1);
        assert_eq!(get(&db, "production"), connected);
        assert!(db.sftp_disabled_for_alias("production").unwrap());
        assert!(!db.sftp_disabled_for_alias("missing").unwrap());
        assert!(db.console_compatibility_for_alias("production").unwrap());
        assert!(!db.console_compatibility_for_alias("missing").unwrap());

        db.update_open_ssh_metadata(
            "production",
            &OpenSshMetadataPatch {
                console_compatibility: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!db.console_compatibility_for_alias("production").unwrap());
        assert!(db.sftp_disabled_for_alias("production").unwrap());
        db.update_open_ssh_metadata(
            "production",
            &OpenSshMetadataPatch {
                disable_sftp: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!db.sftp_disabled_for_alias("production").unwrap());

        db.update_open_ssh_metadata(
            "production",
            &OpenSshMetadataPatch {
                terminal_scheme: Patch::Clear,
                terminal_font_color: Patch::Clear,
                terminal_background_color: Patch::Clear,
                ..Default::default()
            },
        )
        .unwrap();
        let cleared = get(&db, "production");
        assert_eq!(
            (
                cleared.terminal_scheme,
                cleared.terminal_font_color,
                cleared.terminal_background_color
            ),
            (None, None, None)
        );
    }

    #[test]
    fn stores_and_clears_the_command_button_group_of_openssh_and_saved_hosts() {
        let db = memory();
        let mut console = SerialProfile::new("/dev/ttyUSB0");
        console.baud_rate = 9_600;
        let serial_host = db
            .save_saved_host_profile(&saved(
                "Rack console",
                SavedHostSessionProfile::Serial(console),
            ))
            .unwrap();
        let group = |value: &str| OpenSshMetadataPatch {
            command_button_group: Patch::Set(value.into()),
            ..Default::default()
        };
        assert_eq!(
            db.update_open_ssh_metadata("core-01", &group("command-group-juniper"))
                .unwrap()
                .command_button_group
                .as_deref(),
            Some("command-group-juniper")
        );
        assert_eq!(
            db.update_saved_host_metadata(&serial_host.id, &group("command-group-huawei"))
                .unwrap()
                .metadata
                .command_button_group
                .as_deref(),
            Some("command-group-huawei")
        );
        // Other metadata edits leave the group alone; null clears it.
        db.update_open_ssh_metadata(
            "core-01",
            &OpenSshMetadataPatch {
                color: Patch::Set("#ef4444".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let core = get(&db, "core-01");
        assert_eq!(
            (core.color.as_deref(), core.command_button_group.as_deref()),
            (Some("#ef4444"), Some("command-group-juniper"))
        );
        let clear = OpenSshMetadataPatch {
            command_button_group: Patch::Clear,
            ..Default::default()
        };
        db.update_open_ssh_metadata("core-01", &clear).unwrap();
        db.update_saved_host_metadata(&serial_host.id, &clear)
            .unwrap();
        assert_eq!(get(&db, "core-01").command_button_group, None);
        assert_eq!(
            db.saved_host_profile(&serial_host.id)
                .unwrap()
                .unwrap()
                .metadata
                .command_button_group,
            None
        );
    }

    #[test]
    fn stores_and_clears_the_paste_delays_of_openssh_and_saved_hosts() {
        let db = memory();
        let host = db
            .save_saved_host_profile(&saved("Core switch", telnet("switch.lab", 23)))
            .unwrap();
        let both = db
            .update_open_ssh_metadata(
                "core-01",
                &OpenSshMetadataPatch {
                    paste_line_delay_ms: Patch::Set(250),
                    paste_char_delay_ms: Patch::Set(0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (both.paste_line_delay_ms, both.paste_char_delay_ms),
            (Some(250), Some(0))
        );
        let saved_host = db
            .update_saved_host_metadata(
                &host.id,
                &OpenSshMetadataPatch {
                    paste_char_delay_ms: Patch::Set(5),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(saved_host.metadata.paste_char_delay_ms, Some(5));
        assert_eq!(saved_host.metadata.paste_line_delay_ms, None);
        db.update_open_ssh_metadata(
            "core-01",
            &OpenSshMetadataPatch {
                color: Patch::Set("#ef4444".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let kept = get(&db, "core-01");
        assert_eq!(
            (kept.paste_line_delay_ms, kept.paste_char_delay_ms),
            (Some(250), Some(0))
        );
        db.update_open_ssh_metadata(
            "core-01",
            &OpenSshMetadataPatch {
                paste_line_delay_ms: Patch::Clear,
                paste_char_delay_ms: Patch::Clear,
                ..Default::default()
            },
        )
        .unwrap();
        db.update_saved_host_metadata(
            &host.id,
            &OpenSshMetadataPatch {
                paste_char_delay_ms: Patch::Clear,
                ..Default::default()
            },
        )
        .unwrap();
        let cleared = get(&db, "core-01");
        assert_eq!(
            (cleared.paste_line_delay_ms, cleared.paste_char_delay_ms),
            (None, None)
        );
        assert_eq!(
            db.saved_host_profile(&host.id)
                .unwrap()
                .unwrap()
                .metadata
                .paste_char_delay_ms,
            None
        );
    }

    #[test]
    fn moves_hosts_between_case_insensitive_groups_and_can_clear_organization() {
        let db = memory();
        let group = |name: &str| OpenSshMetadataPatch {
            group: Patch::Set(name.into()),
            ..Default::default()
        };
        db.update_open_ssh_metadata(
            "one",
            &OpenSshMetadataPatch {
                color: Patch::Set("#ef4444".into()),
                ..group("Production")
            },
        )
        .unwrap();
        let grouped = db
            .update_open_ssh_metadata("two", &group("production"))
            .unwrap();
        let cleared = db
            .update_open_ssh_metadata(
                "one",
                &OpenSshMetadataPatch {
                    group: Patch::Clear,
                    color: Patch::Clear,
                    ..Default::default()
                },
            )
            .unwrap();
        // One group, not two — but the latest spelling wins.
        assert_eq!(grouped.group.as_deref(), Some("production"));
        assert_eq!((cleared.group, cleared.color), (None, None));
    }

    #[test]
    fn applies_a_case_only_group_rename_to_every_host_already_in_it() {
        let db = memory();
        let group = |name: &str| OpenSshMetadataPatch {
            group: Patch::Set(name.into()),
            ..Default::default()
        };
        db.update_open_ssh_metadata("one", &group("prod")).unwrap();
        db.update_open_ssh_metadata("two", &group("prod")).unwrap();
        db.update_open_ssh_metadata("one", &group("Prod")).unwrap();
        assert_eq!(get(&db, "one").group.as_deref(), Some("Prod"));
        assert_eq!(get(&db, "two").group.as_deref(), Some("Prod"));
    }

    #[test]
    fn preserves_the_stable_profile_id_when_an_openssh_alias_is_renamed() {
        let db = memory();
        let before = db
            .update_open_ssh_metadata(
                "old-alias",
                &OpenSshMetadataPatch {
                    group: Patch::Set("Work".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        db.record_open_ssh_connection("old-alias").unwrap();
        db.save_session_logging_policy("ssh:old-alias", &policy(true, false, 2 * 1024 * 1024, 3))
            .unwrap();
        db.rename_open_ssh_alias("old-alias", "new-alias").unwrap();
        assert!(db.open_ssh_metadata(&["old-alias"]).unwrap().is_empty());
        let after = get(&db, "new-alias");
        assert_eq!(
            (
                after.profile_id,
                after.group.as_deref(),
                after.connect_count
            ),
            (before.profile_id, Some("Work"), 1)
        );
        assert!(
            !db.session_logging_policy("ssh:old-alias")
                .unwrap()
                .overridden
        );
        let moved = db.session_logging_policy("ssh:new-alias").unwrap();
        assert!(moved.enabled && moved.overridden);
        assert_eq!(
            (moved.max_part_bytes, moved.max_parts),
            (2 * 1024 * 1024, 3)
        );
    }

    #[test]
    fn merges_connection_counts_when_renaming_onto_a_known_alias() {
        let db = memory();
        db.record_open_ssh_connection("a").unwrap();
        db.record_open_ssh_connection("b").unwrap();
        db.record_open_ssh_connection("b").unwrap();
        db.rename_open_ssh_alias("a", "b").unwrap();
        assert_eq!(get(&db, "b").connect_count, 3);
        assert!(db.open_ssh_metadata(&["a"]).unwrap().is_empty());
    }

    #[test]
    fn persists_one_drag_order_across_openssh_hosts_and_saved_hosts() {
        let db = memory();
        let host = db
            .save_saved_host_profile(&saved("Core router", telnet("router.example.test", 23)))
            .unwrap();
        db.reorder_managed_hosts(&[
            ManagedHostRef::Ssh {
                alias: "gamma".into(),
            },
            ManagedHostRef::Profile {
                id: host.id.clone(),
            },
            ManagedHostRef::Ssh {
                alias: "alpha".into(),
            },
        ])
        .unwrap();
        let order: Vec<(String, Option<i64>)> = db
            .open_ssh_metadata(&["alpha", "gamma"])
            .unwrap()
            .into_iter()
            .map(|(alias, m)| (alias, m.sort_order))
            .collect();
        assert_eq!(
            order,
            vec![("alpha".into(), Some(2)), ("gamma".into(), Some(0))]
        );
        assert_eq!(
            db.saved_host_profile(&host.id)
                .unwrap()
                .unwrap()
                .metadata
                .sort_order,
            Some(1)
        );
        let duplicate = db.reorder_managed_hosts(&[
            ManagedHostRef::Ssh {
                alias: "alpha".into(),
            },
            ManagedHostRef::Ssh {
                alias: "alpha".into(),
            },
        ]);
        assert!(duplicate.unwrap_err().to_string().contains("duplicate"));
        let missing = db.reorder_managed_hosts(&[ManagedHostRef::Profile {
            id: "missing".into(),
        }]);
        assert!(missing.unwrap_err().to_string().contains("not found"));
    }
}

mod saved_hosts {
    use super::*;

    #[test]
    fn round_trips_a_self_contained_ssh_profile_without_an_openssh_alias() {
        let db = memory();
        let profile = SshProfile {
            target: "router.example.test".into(),
            use_config: Some(false),
            user: Some("admin".into()),
            port: Some(2222),
            password_only: Some(true),
            ..Default::default()
        };
        let created = db
            .save_saved_host_profile(&SavedHostProfileInput {
                id: Some("muxus-ssh-router".into()),
                name: "Core router".into(),
                profile: SavedHostSessionProfile::Ssh(profile.clone()),
            })
            .unwrap();
        assert_eq!(created.id, "muxus-ssh-router");
        assert_eq!(created.kind.as_str(), "ssh");
        assert_eq!(
            created.profile,
            SavedHostSessionProfile::Ssh(SshProfile {
                profile_id: Some("muxus-ssh-router".into()),
                ..profile
            })
        );
        assert_eq!(db.list_saved_host_profiles().unwrap().len(), 1);
        let error = db
            .save_saved_host_profile(&saved(
                "Config-dependent profile",
                SavedHostSessionProfile::Ssh(SshProfile::new("router-alias")),
            ))
            .unwrap_err();
        assert!(error.to_string().contains("useConfig"));
    }

    #[test]
    fn upserts_supplied_import_ids_without_crossing_connection_kind_ownership() {
        let db = memory();
        let created = db
            .save_saved_host_profile(&SavedHostProfileInput {
                id: Some("securecrt-serial-console".into()),
                name: "Imported console".into(),
                profile: serial("/dev/ttyUSB0"),
            })
            .unwrap();
        assert_eq!(created.id, "securecrt-serial-console");
        let local_id = db
            .create_native_connection(&NativeConnectionInput {
                kind: NativeConnectionKind::Local,
                name: "Local shell".into(),
                config: json!({}),
                credential_ref_id: None,
            })
            .unwrap();
        let error = db
            .save_saved_host_profile(&SavedHostProfileInput {
                id: Some(local_id),
                name: "Wrong owner".into(),
                profile: telnet("example.test", 23),
            })
            .unwrap_err();
        assert!(error.to_string().contains("different connection type"));
    }

    #[test]
    fn round_trips_connection_settings_organization_recent_use_updates_and_deletion() {
        let db = memory();
        let mut console = SerialProfile::new("/dev/ttyUSB0");
        console.flow_control = SerialFlowControl::Hardware;
        let created = db
            .save_saved_host_profile(&saved(
                "Rack console",
                SavedHostSessionProfile::Serial(console),
            ))
            .unwrap();
        let organized = db
            .update_saved_host_metadata(
                &created.id,
                &OpenSshMetadataPatch {
                    display_name: Patch::Set("Core rack console".into()),
                    group: Patch::Set("Lab".into()),
                    color: Patch::Set("#3b82f6".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        db.record_saved_host_connection(&created.id).unwrap();
        assert_eq!(organized.name, "Core rack console");
        assert_eq!(organized.metadata.group.as_deref(), Some("Lab"));
        let SavedHostSessionProfile::Serial(stored) = &organized.profile else {
            panic!("serial")
        };
        assert_eq!(
            (stored.profile_id.as_deref(), stored.flow_control),
            (Some(created.id.as_str()), SerialFlowControl::Hardware)
        );
        assert_eq!(
            db.saved_host_profile(&created.id)
                .unwrap()
                .unwrap()
                .metadata
                .connect_count,
            1
        );

        let updated = db
            .save_saved_host_profile(&SavedHostProfileInput {
                id: Some(created.id.clone()),
                name: "Console server".into(),
                profile: telnet("console.example.test", 2323),
            })
            .unwrap();
        assert_eq!(
            (updated.kind.as_str(), updated.name.as_str()),
            ("telnet", "Console server")
        );
        assert_eq!(updated.metadata.group.as_deref(), Some("Lab"));
        assert_eq!(updated.metadata.connect_count, 1);
        assert_eq!(db.list_saved_host_profiles().unwrap(), vec![updated]);
        db.save_session_logging_policy(
            &format!("profile:{}", created.id),
            &policy(true, false, 1024 * 1024, 2),
        )
        .unwrap();
        assert!(db.delete_saved_host_profile(&created.id).unwrap());
        assert!(db.list_saved_host_profiles().unwrap().is_empty());
        assert!(
            !db.session_logging_policy(&format!("profile:{}", created.id))
                .unwrap()
                .overridden
        );
    }

    #[test]
    fn rejects_secrets_in_native_host_settings() {
        let db = memory();
        let error = db
            .create_native_connection(&NativeConnectionInput {
                kind: NativeConnectionKind::Telnet,
                name: "Unsafe".into(),
                config: json!({ "host": "router.example.test", "port": 23, "password": "do-not-save" }),
                credential_ref_id: None,
            })
            .unwrap_err();
        assert!(error.to_string().contains("password vault"));
    }

    #[test]
    fn pins_desktop_identities_per_host_port_and_route() {
        let db = memory();
        db.trust_desktop_identity("Win-Build", 3389, "", "AA:BB", "CN=win")
            .unwrap();
        db.trust_desktop_identity("win-build", 3389, "ssh:bastion", "CC:DD", "CN=win")
            .unwrap();
        assert_eq!(
            db.trusted_desktop_identity("WIN-BUILD", 3389, "")
                .unwrap()
                .unwrap()
                .fingerprint,
            "AA:BB"
        );
        assert_eq!(
            db.trusted_desktop_identity("win-build", 3389, "ssh:bastion")
                .unwrap()
                .unwrap()
                .fingerprint,
            "CC:DD"
        );
        db.trust_desktop_identity("win-build", 3389, "", "EE:FF", "CN=win2")
            .unwrap();
        assert_eq!(
            db.trusted_desktop_identity("win-build", 3389, "")
                .unwrap()
                .unwrap()
                .subject,
            "CN=win2"
        );
        assert_eq!(
            db.trusted_desktop_identity("win-build", 3390, "").unwrap(),
            None
        );
    }
}

mod safety {
    use super::*;

    fn input(account: &str) -> CredentialRefInput {
        CredentialRefInput {
            provider: "muxus-master-vault".into(),
            service: "muxus/ssh-password/v1".into(),
            account: account.into(),
            label: Some("Failed encryption".into()),
            username: None,
        }
    }

    #[test]
    fn rolls_back_a_new_credential_reference_when_encryption_fails() {
        let db = memory();
        let mut rolled_back = String::new();
        let result =
            db.upsert_encrypted_credential_atomically(&input("failed-encryption"), |reference| {
                rolled_back = reference.id.clone();
                Err::<EncryptedCredentialSecret, DbError>(DbError::Invalid("sealing failed".into()))
            });
        assert_eq!(result.unwrap_err().to_string(), "sealing failed");
        let next = db
            .upsert_credential_ref(&input("failed-encryption"))
            .unwrap();
        assert_ne!(next.id, rolled_back);
    }

    #[test]
    fn stores_only_a_credential_reference_alongside_native_profile_data() {
        let db = memory();
        let credential = db
            .upsert_credential_ref(&CredentialRefInput {
                provider: "os-keychain".into(),
                service: "muxus/ssh".into(),
                account: "alice@example.com".into(),
                label: Some("Production SSH".into()),
                username: None,
            })
            .unwrap();
        db.create_native_connection(&NativeConnectionInput {
            kind: NativeConnectionKind::Ssh,
            name: "Production".into(),
            config: json!({ "host": "example.com", "identityFile": "/home/alice/.ssh/id_ed25519" }),
            credential_ref_id: Some(credential.id),
        })
        .unwrap();
        let error = db
            .create_native_connection(&NativeConnectionInput {
                kind: NativeConnectionKind::Ssh,
                name: "Unsafe".into(),
                config: json!({ "auth": { "passphrase": "secret" } }),
                credential_ref_id: None,
            })
            .unwrap_err();
        assert!(error.to_string().contains("password vault"));
        let workspace_error = db
            .save_workspace(
                &workspace(None, "Unsafe", json!({ "pane": { "token": "secret" } })),
                false,
            )
            .unwrap_err();
        assert!(workspace_error.to_string().contains("password vault"));
    }

    #[test]
    fn round_trips_a_secret_free_recoverable_workspace_layout() {
        let db = memory();
        let layout = json!({
            "version": 1,
            "root": {
                "type": "split", "direction": "horizontal", "ratio": 0.5,
                "children": [
                    { "type": "terminal", "profileRef": "profile-a", "cwdHint": "/srv/app" },
                    { "type": "sftp", "profileRef": "profile-a", "path": "/var/log" }
                ]
            }
        });
        let groups = vec![WorkspaceMultiExecGroup {
            id: "prod".into(),
            name: "Production".into(),
            tab_ids: vec!["tab-a".into(), "tab-b".into()],
        }];
        let saved = db
            .save_workspace(
                &WorkspaceInput {
                    id: None,
                    name: "Daily work".into(),
                    layout,
                    multi_exec_groups: Some(groups.clone()),
                },
                false,
            )
            .unwrap();
        assert_eq!(db.workspace(&saved.id).unwrap().unwrap(), saved);
        assert!(!saved.is_locked && !saved.is_startup);
        assert_eq!(saved.multi_exec_groups, groups);
    }

    #[test]
    fn renames_opens_and_selects_exactly_one_startup_workspace() {
        let db = memory();
        let empty = json!({ "version": 1, "root": null });
        let first = db
            .save_workspace(&workspace(None, "First", empty.clone()), false)
            .unwrap();
        let second = db
            .save_workspace(&workspace(None, "Second", empty), false)
            .unwrap();
        assert_eq!(
            db.rename_workspace(&first.id, "Daily")
                .unwrap()
                .unwrap()
                .name,
            "Daily"
        );
        assert!(
            db.open_workspace(&first.id)
                .unwrap()
                .unwrap()
                .last_opened_at
                .is_some()
        );
        assert!(
            db.set_startup_workspace(Some(&first.id))
                .unwrap()
                .unwrap()
                .is_startup
        );
        assert!(
            db.set_startup_workspace(Some(&second.id))
                .unwrap()
                .unwrap()
                .is_startup
        );
        assert!(!db.workspace(&first.id).unwrap().unwrap().is_startup);
        assert_eq!(db.startup_workspace().unwrap().unwrap().id, second.id);
        assert_eq!(
            db.list_workspace_summaries()
                .unwrap()
                .iter()
                .filter(|w| w.is_startup)
                .count(),
            1
        );
        assert_eq!(db.set_startup_workspace(None).unwrap(), None);
        assert_eq!(db.startup_workspace().unwrap(), None);
        assert_eq!(db.set_startup_workspace(Some("missing")).unwrap(), None);
    }

    #[test]
    fn locks_and_unlocks_a_workspace_without_changing_its_layout() {
        let db = memory();
        let saved = db
            .save_workspace(
                &workspace(
                    None,
                    "Stable startup",
                    json!({ "version": 1, "root": null }),
                ),
                false,
            )
            .unwrap();
        let locked = db.set_workspace_locked(&saved.id, true).unwrap().unwrap();
        assert!(locked.is_locked);
        assert_eq!(locked.layout, saved.layout);
        assert!(db.list_workspace_summaries().unwrap()[0].is_locked);
        let attempt = db.save_workspace(
            &workspace(
                Some(&saved.id),
                &saved.name,
                json!({ "version": 1, "root": { "id": "unexpected" } }),
            ),
            false,
        );
        assert!(matches!(attempt, Err(DbError::WorkspaceLocked(_))));
        assert_eq!(
            db.workspace(&saved.id).unwrap().unwrap().layout,
            saved.layout
        );
        let explicit = db
            .save_workspace(
                &workspace(
                    Some(&saved.id),
                    &saved.name,
                    json!({ "version": 1, "root": { "id": "explicit" } }),
                ),
                true,
            )
            .unwrap();
        assert!(explicit.is_locked);
        assert_eq!(
            explicit.layout,
            json!({ "version": 1, "root": { "id": "explicit" } })
        );
        assert!(
            !db.set_workspace_locked(&saved.id, false)
                .unwrap()
                .unwrap()
                .is_locked
        );
    }

    #[test]
    fn allocates_the_first_free_default_workspace_name() {
        let db = memory();
        db.save_workspace(
            &workspace(None, " workspace 1 ", json!({ "version": 1, "root": null })),
            false,
        )
        .unwrap();
        let next = db
            .save_workspace_with_default_name(json!({ "version": 1, "root": null }), None)
            .unwrap();
        assert_eq!(next.name, "Workspace 2");
    }
}

mod folders {
    use super::*;

    fn auth(user: Option<&str>, port: Option<u16>) -> FolderAuthSettings {
        FolderAuthSettings {
            user: user.map(str::to_owned),
            port,
            ..Default::default()
        }
    }

    #[test]
    fn upserts_by_case_folded_path_and_keeps_the_row_id_stable() {
        let db = memory();
        let created = db
            .upsert_folder_settings("Prod/EU", &auth(Some("admin"), None))
            .unwrap();
        let updated = db
            .upsert_folder_settings("prod/eu", &auth(Some("root"), Some(2222)))
            .unwrap();
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.path, "prod/eu");
        assert_eq!(updated.auth, auth(Some("root"), Some(2222)));
        assert_eq!(db.list_folder_settings().unwrap().len(), 1);
        assert_eq!(
            db.folder_settings_for_path(" Prod / EU ")
                .unwrap()
                .unwrap()
                .id,
            created.id
        );
    }

    #[test]
    fn moves_a_folder_and_its_descendants_keeping_ids() {
        let db = memory();
        let prod = db
            .upsert_folder_settings("Prod", &auth(Some("root"), None))
            .unwrap();
        let eu = db
            .upsert_folder_settings("Prod/EU", &auth(None, Some(2222)))
            .unwrap();
        db.upsert_folder_settings("Staging", &auth(Some("stage"), None))
            .unwrap();
        let moved = db.move_folder_settings("Prod", "Production").unwrap();
        assert_eq!((moved.moved, moved.dropped.len()), (2, 0));
        assert_eq!(
            db.folder_settings_for_path("Production")
                .unwrap()
                .unwrap()
                .id,
            prod.id
        );
        assert_eq!(
            db.folder_settings_for_path("Production/EU")
                .unwrap()
                .unwrap()
                .id,
            eu.id
        );
        assert!(db.folder_settings_for_path("Staging").unwrap().is_some());
        assert!(db.folder_settings_for_path("Prod").unwrap().is_none());
    }

    #[test]
    fn keeps_the_destination_settings_when_a_move_merges_folders() {
        let db = memory();
        let source = db
            .upsert_folder_settings("Prod", &auth(Some("source"), None))
            .unwrap();
        let destination = db
            .upsert_folder_settings("Production", &auth(Some("dest"), None))
            .unwrap();
        let moved = db.move_folder_settings("Prod", "Production").unwrap();
        assert_eq!(
            moved
                .dropped
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            vec![source.id]
        );
        let kept = db.folder_settings_for_path("Production").unwrap().unwrap();
        assert_eq!(
            (kept.id, kept.auth),
            (destination.id, auth(Some("dest"), None))
        );
    }

    #[test]
    fn updates_only_the_display_path_on_a_recapitalization() {
        let db = memory();
        let row = db
            .upsert_folder_settings("prod", &auth(Some("root"), None))
            .unwrap();
        db.move_folder_settings("prod", "Prod").unwrap();
        let after = db.folder_settings_for_path("prod").unwrap().unwrap();
        assert_eq!((after.id, after.path.as_str()), (row.id, "Prod"));
    }

    #[test]
    fn deleting_a_folder_promotes_descendant_settings_a_level_up() {
        let db = memory();
        let own = db
            .upsert_folder_settings("Prod/EU", &auth(Some("eu"), None))
            .unwrap();
        let edge = db
            .upsert_folder_settings("Prod/EU/Edge", &auth(None, Some(2222)))
            .unwrap();
        let removed = db.delete_folder_settings("Prod/EU").unwrap();
        assert_eq!(
            removed.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
            vec![own.id]
        );
        assert_eq!(
            db.folder_settings_for_path("Prod/Edge")
                .unwrap()
                .unwrap()
                .id,
            edge.id
        );
        assert!(db.folder_settings_for_path("Prod/EU").unwrap().is_none());
    }

    #[test]
    fn merges_folder_chains_nearest_first() {
        let db = memory();
        db.upsert_folder_settings(
            "Prod",
            &FolderAuthSettings {
                user: Some("root".into()),
                port: Some(22),
                ..Default::default()
            },
        )
        .unwrap();
        db.upsert_folder_settings("Prod/EU", &auth(None, Some(2222)))
            .unwrap();
        let chain = db.folder_settings_chain(Some("prod/eu/edge")).unwrap();
        assert_eq!(
            chain
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["Prod/EU", "Prod"]
        );
        let merged = muxus_store::db::merge_folder_auth(chain.iter().map(|row| &row.auth));
        assert_eq!(merged, auth(Some("root"), Some(2222)));
    }
}

mod login_sequences {
    use super::*;

    fn wait(id: &str, pattern: &str, timeout_seconds: u32) -> LoginSequenceStep {
        LoginSequenceStep::Wait(LoginWaitStep {
            id: id.into(),
            pattern: pattern.into(),
            regex: None,
            timeout_seconds,
        })
    }

    fn send(id: &str, text: &str) -> LoginSequenceStep {
        LoginSequenceStep::Send(LoginSendStep {
            id: id.into(),
            text: text.into(),
            enter: true,
        })
    }

    fn enable() -> LoginSequence {
        LoginSequence {
            steps: vec![
                wait("a", ">", 10),
                send("b", "enable"),
                LoginSequenceStep::Secret(muxus_core::api::LoginSecretStep {
                    id: "c".into(),
                    secret_id: "secret-1".into(),
                    enter: true,
                }),
            ],
        }
    }

    fn banner() -> LoginSequence {
        LoginSequence {
            steps: vec![wait("x", "Press RETURN", 30), send("y", "")],
        }
    }

    fn sequence(value: LoginSequence) -> OpenSshMetadataPatch {
        OpenSshMetadataPatch {
            login_sequence: Patch::Set(value),
            ..Default::default()
        }
    }

    #[test]
    fn stores_a_host_sequence_in_the_metadata_of_openssh_and_muxus_owned_hosts() {
        let db = memory();
        assert_eq!(
            db.update_open_ssh_metadata("core-sw", &sequence(enable()))
                .unwrap()
                .login_sequence,
            Some(enable())
        );
        assert_eq!(
            db.open_ssh_metadata(&["core-sw"]).unwrap()[0]
                .1
                .login_sequence,
            Some(enable())
        );
        let recolored = db
            .update_open_ssh_metadata(
                "core-sw",
                &OpenSshMetadataPatch {
                    color: Patch::Set("#ff0000".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(recolored.login_sequence, Some(enable()));
        let cleared = db
            .update_open_ssh_metadata(
                "core-sw",
                &OpenSshMetadataPatch {
                    login_sequence: Patch::Clear,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared.login_sequence, None);

        let host = db
            .save_saved_host_profile(&saved("Console server", telnet("console.lab", 23)))
            .unwrap();
        let updated = db
            .update_saved_host_metadata(
                &host.id,
                &OpenSshMetadataPatch {
                    group: Patch::Set("Lab".into()),
                    ..sequence(banner())
                },
            )
            .unwrap();
        assert_eq!(updated.metadata.login_sequence, Some(banner()));
        assert_eq!(
            db.list_saved_host_profiles().unwrap()[0]
                .metadata
                .login_sequence,
            Some(banner())
        );
        let entry = db
            .host_login_sequence(HostRef::ProfileId(&host.id))
            .unwrap()
            .unwrap();
        assert_eq!(
            (entry.login_sequence, entry.group.as_deref()),
            (Some(banner()), Some("Lab"))
        );
        assert_eq!(
            db.host_login_sequence(HostRef::ProfileId("missing"))
                .unwrap(),
            None
        );
        assert_eq!(
            db.host_login_sequence(HostRef::Alias("unknown")).unwrap(),
            None
        );
    }

    #[test]
    fn keeps_the_sequence_when_the_alias_is_renamed() {
        let db = memory();
        db.update_open_ssh_metadata(
            "router",
            &OpenSshMetadataPatch {
                group: Patch::Set("Core".into()),
                ..sequence(enable())
            },
        )
        .unwrap();
        db.rename_open_ssh_alias("router", "router-1").unwrap();
        let entry = db
            .host_login_sequence(HostRef::Alias("router-1"))
            .unwrap()
            .unwrap();
        assert_eq!(
            (entry.login_sequence, entry.group.as_deref()),
            (Some(enable()), Some("Core"))
        );
    }

    #[test]
    fn stores_folder_sequences_that_follow_the_folder_when_it_moves() {
        let db = memory();
        let row = db
            .upsert_folder_settings("Network", &FolderAuthSettings::default())
            .unwrap();
        db.set_folder_login_sequence(&row.id, Some(&enable()))
            .unwrap();
        assert_eq!(
            db.folder_settings_for_path("network")
                .unwrap()
                .unwrap()
                .login_sequence,
            Some(enable())
        );
        db.upsert_folder_settings(
            "Network",
            &FolderAuthSettings {
                user: Some("admin".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            db.folder_settings_for_path("Network")
                .unwrap()
                .unwrap()
                .login_sequence,
            Some(enable())
        );
        db.move_folder_settings("Network", "Infra/Network").unwrap();
        assert_eq!(
            db.folder_settings_for_path("Infra/Network")
                .unwrap()
                .unwrap()
                .login_sequence,
            Some(enable())
        );
        db.set_folder_login_sequence(&row.id, None).unwrap();
        assert_eq!(
            db.folder_settings_for_path("Infra/Network")
                .unwrap()
                .unwrap()
                .login_sequence,
            None
        );
    }

    fn inheritance() -> Database {
        let db = memory();
        for (path, value) in [
            ("Network", enable()),
            ("Network/Consoles", banner()),
            ("Network/Lab", LoginSequence::default()),
        ] {
            let row = db
                .upsert_folder_settings(path, &FolderAuthSettings::default())
                .unwrap();
            db.set_folder_login_sequence(&row.id, Some(&value)).unwrap();
        }
        db
    }

    fn group(name: &str) -> OpenSshMetadataPatch {
        OpenSshMetadataPatch {
            group: Patch::Set(name.into()),
            ..Default::default()
        }
    }

    #[test]
    fn uses_the_nearest_folder_that_sets_one() {
        let db = inheritance();
        db.update_open_ssh_metadata("core", &group("Network"))
            .unwrap();
        db.update_open_ssh_metadata("edge", &group("Network/EU/Edge"))
            .unwrap();
        db.update_open_ssh_metadata("ts1", &group("network/consoles"))
            .unwrap();
        assert_eq!(
            db.resolve_login_sequence(HostRef::Alias("core")).unwrap(),
            Some(enable())
        );
        assert_eq!(
            db.resolve_login_sequence(HostRef::Alias("edge")).unwrap(),
            Some(enable())
        );
        assert_eq!(
            db.resolve_login_sequence(HostRef::Alias("ts1")).unwrap(),
            Some(banner())
        );
    }

    #[test]
    fn lets_a_host_or_a_subfolder_override_or_switch_it_off() {
        let db = inheritance();
        db.update_open_ssh_metadata(
            "own",
            &OpenSshMetadataPatch {
                group: Patch::Set("Network/Consoles".into()),
                ..sequence(enable())
            },
        )
        .unwrap();
        db.update_open_ssh_metadata(
            "off",
            &OpenSshMetadataPatch {
                group: Patch::Set("Network".into()),
                ..sequence(LoginSequence::default())
            },
        )
        .unwrap();
        db.update_open_ssh_metadata("lab", &group("Network/Lab/Bench"))
            .unwrap();
        assert_eq!(
            db.resolve_login_sequence(HostRef::Alias("own")).unwrap(),
            Some(enable())
        );
        assert_eq!(
            db.resolve_login_sequence(HostRef::Alias("off")).unwrap(),
            None
        );
        assert_eq!(
            db.resolve_login_sequence(HostRef::Alias("lab")).unwrap(),
            None
        );
    }

    #[test]
    fn runs_nothing_for_hosts_outside_any_folder_with_a_sequence() {
        let db = inheritance();
        db.update_open_ssh_metadata(
            "loose",
            &OpenSshMetadataPatch {
                color: Patch::Set("#00ff00".into()),
                ..Default::default()
            },
        )
        .unwrap();
        db.update_open_ssh_metadata("elsewhere", &group("Servers"))
            .unwrap();
        for alias in ["loose", "elsewhere", "never-seen"] {
            assert_eq!(
                db.resolve_login_sequence(HostRef::Alias(alias)).unwrap(),
                None
            );
        }
    }

    #[test]
    fn resolves_muxus_owned_hosts_by_their_profile_id() {
        let db = inheritance();
        let mut console = SerialProfile::new("/dev/ttyUSB0");
        console.baud_rate = 9600;
        let host = db
            .save_saved_host_profile(&saved(
                "Bench console",
                SavedHostSessionProfile::Serial(console),
            ))
            .unwrap();
        db.update_saved_host_metadata(&host.id, &group("Network/Consoles"))
            .unwrap();
        assert_eq!(
            db.resolve_login_sequence(HostRef::ProfileId(&host.id))
                .unwrap(),
            Some(banner())
        );
    }

    #[test]
    fn rejects_secret_values_at_the_persistence_boundary() {
        // A typed step cannot carry extra fields; the persistence check still
        // guards the JSON it writes.
        let leaky = json!({ "steps": [{ "id": "z", "kind": "send", "text": "x", "enter": true, "password": "hunter2" }] });
        assert!(muxus_store::db::assert_secret_free(&leaky, "loginSequence").is_err());
    }
}

/// The storage side of tests/unit/server/workspace-routes.test.ts.
mod workspace_requests {
    use muxus_core::api::WorkspaceSaveRequest;
    use muxus_core::validate::parse_value;

    use super::*;

    fn layout() -> Value {
        json!({
            "version": 1,
            "root": {
                "id": "split-1", "type": "split", "direction": "horizontal", "ratio": 0.5,
                "children": [
                    { "id": "pane-1", "type": "pane", "activeTabId": "tab-1", "tabs": [
                        { "id": "tab-1", "kind": "terminal", "title": "Local",
                          "profile": { "kind": "local", "cwd": "/tmp" }, "cwdHint": "/tmp", "offerReconnect": true }
                    ]},
                    { "id": "pane-2", "type": "pane", "activeTabId": "tab-2", "tabs": [
                        { "id": "tab-2", "kind": "sftp", "title": "Files",
                          "connection": { "source": "openssh", "id": "router" }, "path": "/var/log" }
                    ]}
                ]
            },
            "activePaneId": "pane-1"
        })
    }

    fn request(body: Value) -> WorkspaceSaveRequest {
        parse_value(body).unwrap()
    }

    #[test]
    fn round_trips_a_versioned_pane_tree_without_claiming_to_resume_sessions() {
        let db = memory();
        let saved = db
            .put_workspace(&request(
                json!({ "name": "Daily work", "layout": layout() }),
            ))
            .unwrap();
        let read = db.workspace(&saved.id).unwrap().unwrap();
        assert_eq!(
            (read.name.as_str(), &read.layout),
            ("Daily work", &layout())
        );
        assert!(read.multi_exec_groups.is_empty() && !read.is_locked && !read.is_startup);
        assert_eq!(db.list_workspace_summaries().unwrap()[0].id, saved.id);
        assert_eq!(db.latest_workspace().unwrap().unwrap().layout, layout());
        assert!(read.to_record().is_ok());
    }

    #[test]
    fn allocates_implicit_default_workspace_names() {
        let db = memory();
        db.put_workspace(&request(
            json!({ "name": "Workspace 1", "layout": layout() }),
        ))
        .unwrap();
        let body =
            json!({ "name": "Workspace 2", "allocateDefaultName": true, "layout": layout() });
        let second = db.put_workspace(&request(body.clone())).unwrap();
        let third = db.put_workspace(&request(body)).unwrap();
        assert_eq!(
            (second.name.as_str(), third.name.as_str()),
            ("Workspace 2", "Workspace 3")
        );
    }

    #[test]
    fn deletes_a_workspace_and_clears_its_startup_selection() {
        let db = memory();
        let saved = db
            .put_workspace(&request(json!({ "name": "Temporary", "layout": layout() })))
            .unwrap();
        db.set_startup_workspace(Some(&saved.id)).unwrap();
        assert!(db.delete_workspace(&saved.id).unwrap());
        assert_eq!(db.workspace(&saved.id).unwrap(), None);
        assert_eq!(db.startup_workspace().unwrap(), None);
    }

    #[test]
    fn prevents_stale_windows_from_renaming_or_recreating_a_deleted_workspace() {
        let db = memory();
        let id = db
            .put_workspace(&request(
                json!({ "name": "Authoritative name", "layout": layout() }),
            ))
            .unwrap()
            .id;
        let empty = json!({ "version": 1, "root": null });
        let stale = db
            .put_workspace(&request(
                json!({ "id": id, "name": "Stale window name", "layout": empty }),
            ))
            .unwrap();
        assert_eq!(
            (stale.name.as_str(), &stale.layout),
            ("Authoritative name", &empty)
        );
        db.delete_workspace(&id).unwrap();
        let after_delete = db.put_workspace(&request(
            json!({ "id": id, "name": "Stale window name", "layout": empty }),
        ));
        assert!(
            matches!(after_delete, Err(DbError::NotFound(message)) if message == "workspace not found")
        );
        let recreated = db
            .put_workspace(&request(json!({ "id": id, "createIfMissing": true, "name": "Recreated intentionally", "layout": empty })))
            .unwrap();
        assert_eq!(recreated.name, "Recreated intentionally");
        let repeated = db
            .put_workspace(&request(json!({
                "id": id, "createIfMissing": true, "name": "Must not replace the existing workspace", "layout": layout()
            })))
            .unwrap();
        assert_eq!(
            (repeated.name.as_str(), &repeated.layout),
            ("Recreated intentionally", &empty)
        );
    }

    #[test]
    fn locked_workspaces_change_only_through_an_explicit_save() {
        let db = memory();
        let id = db
            .put_workspace(&request(json!({ "name": "Stable", "layout": layout() })))
            .unwrap()
            .id;
        db.set_workspace_locked(&id, true).unwrap();
        let empty = json!({ "version": 1, "root": null });
        let autosave = db.put_workspace(&request(
            json!({ "id": id, "name": "Stable", "layout": empty }),
        ));
        assert!(matches!(autosave, Err(DbError::WorkspaceLocked(_))));
        let explicit = db
            .put_workspace(&request(
                json!({ "id": id, "name": "Stable", "layout": empty, "overwriteLocked": true }),
            ))
            .unwrap();
        assert!(explicit.is_locked);
        assert_eq!(explicit.layout, empty);
    }
}
