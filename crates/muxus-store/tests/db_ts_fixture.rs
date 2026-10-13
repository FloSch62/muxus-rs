//! Reads of a database the TypeScript build populated must return exactly
//! what the TypeScript `MuxusDatabase` returned for it (recorded in
//! `ts-populated.expected.json` by `fixtures/db/generate.mts`).

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use muxus_core::management::ManagementProtocol;
use muxus_store::Database;
use muxus_store::db::{EncryptedCredentialRecord, HostRef};
use serde::Serialize;
use serde_json::{Map, Value, json};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/db")
        .join(name)
}

fn open_fixture(dir: &Path) -> Database {
    let compressed = std::fs::read(fixture("ts-populated.sqlite3.zst")).unwrap();
    let path = dir.join("muxus.sqlite3");
    std::fs::write(&path, zstd::decode_all(compressed.as_slice()).unwrap()).unwrap();
    Database::open(&path).unwrap()
}

fn expected() -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture("ts-populated.expected.json")).unwrap())
        .unwrap()
}

fn value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}

fn b64(bytes: &[u8]) -> Value {
    Value::from(STANDARD.encode(bytes))
}

fn credential(record: &EncryptedCredentialRecord) -> Value {
    let mut map = Map::new();
    map.insert("id".into(), json!(record.id));
    map.insert("provider".into(), json!(record.provider));
    map.insert("service".into(), json!(record.service));
    map.insert("account".into(), json!(record.account));
    if let Some(label) = &record.label {
        map.insert("label".into(), json!(label));
    }
    if let Some(username) = &record.username {
        map.insert("username".into(), json!(username));
    }
    map.insert("formatVersion".into(), json!(record.format_version));
    map.insert("nonce".into(), b64(&record.nonce));
    map.insert("ciphertext".into(), b64(&record.ciphertext));
    map.insert("authTag".into(), b64(&record.auth_tag));
    map.insert("createdAt".into(), json!(record.created_at));
    map.insert("updatedAt".into(), json!(record.updated_at));
    Value::Object(map)
}

#[test]
fn reads_match_the_typescript_database_layer() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_fixture(dir.path());
    let expected = expected();
    let ids = &expected["ids"];
    let id = |key: &str| ids[key].as_str().unwrap().to_owned();
    let check = |key: &str, actual: Value| {
        assert_eq!(actual, expected[key], "{key} differs");
    };

    check("appliedMigrations", value(db.applied_migrations().unwrap()));
    let metadata: Map<String, Value> = db
        .open_ssh_metadata(&["production", "bastion", "new-name", "old-name", "missing"])
        .unwrap()
        .into_iter()
        .map(|(alias, metadata)| (alias, value(metadata)))
        .collect();
    check("openSshMetadata", Value::Object(metadata));
    check(
        "groupForAlias",
        value(db.group_for_alias("production").unwrap()),
    );
    check(
        "groupForSavedHost",
        value(db.group_for_saved_host(&id("ssh")).unwrap()),
    );
    check(
        "sftpDisabledForAlias",
        value(db.sftp_disabled_for_alias("production").unwrap()),
    );
    check(
        "consoleCompatibilityForSavedHost",
        value(db.console_compatibility_for_saved_host(&id("ssh")).unwrap()),
    );
    check("savedHosts", value(db.list_saved_host_profiles().unwrap()));
    check(
        "savedHost",
        value(db.saved_host_profile(&id("vnc")).unwrap()),
    );
    check(
        "workspaceSummaries",
        value(db.list_workspace_summaries().unwrap()),
    );
    check("workspaces", value(db.list_workspaces().unwrap()));
    check("latestWorkspace", value(db.latest_workspace().unwrap()));
    check("startupWorkspace", value(db.startup_workspace().unwrap()));
    check("snapshot", value(db.terminal_snapshot("tab-1").unwrap()));
    check("tunnels", value(db.list_tunnels().unwrap()));
    check("tunnel", value(db.tunnel("tunnel-socks").unwrap()));
    check(
        "policyExact",
        value(db.session_logging_policy("ssh:production").unwrap()),
    );
    check(
        "policyInherited",
        value(db.session_logging_policy("ssh:other").unwrap()),
    );
    check(
        "policyDefaults",
        value(db.session_logging_policy("*").unwrap()),
    );
    check(
        "historySettings",
        value(db.session_history_settings().unwrap()),
    );
    check(
        "logFileSettings",
        value(db.session_log_file_settings().unwrap()),
    );
    check("folders", value(db.list_folder_settings().unwrap()));
    check(
        "folder",
        value(db.folder_settings_for_path("work/eu").unwrap()),
    );
    check(
        "hostLoginSequenceAlias",
        value(
            db.host_login_sequence(HostRef::Alias("production"))
                .unwrap(),
        ),
    );
    check(
        "hostLoginSequenceSaved",
        value(
            db.host_login_sequence(HostRef::ProfileId(&id("ssh")))
                .unwrap(),
        ),
    );

    let vault = db.password_vault_config().unwrap().unwrap();
    let config = &vault.config;
    check(
        "vaultConfig",
        json!({
            "formatVersion": config.format_version,
            "vaultId": config.vault_id,
            "unlockPolicy": config.unlock_policy,
            "kdfAlgorithm": config.kdf_algorithm,
            "kdfSalt": b64(&config.kdf_salt),
            "kdfCost": config.kdf_cost,
            "kdfBlockSize": config.kdf_block_size,
            "kdfParallelism": config.kdf_parallelism,
            "masterKeyNonce": b64(&config.master_key_nonce),
            "masterKeyCiphertext": b64(&config.master_key_ciphertext),
            "masterKeyTag": b64(&config.master_key_tag),
            "keyCheck": b64(config.key_check.as_deref().unwrap()),
            "createdAt": vault.created_at,
            "updatedAt": vault.updated_at,
        }),
    );
    let ssh_passwords = db
        .list_encrypted_credentials("muxus-master-vault", "muxus/ssh-password/v1")
        .unwrap();
    check(
        "sshPasswords",
        Value::Array(ssh_passwords.iter().map(credential).collect()),
    );
    let secrets = db
        .list_encrypted_credentials("muxus-master-vault", "muxus/named-secret/v1")
        .unwrap();
    check(
        "namedSecrets",
        Value::Array(secrets.iter().map(credential).collect()),
    );
    check(
        "pendingCleanup",
        value(db.pending_password_vault_key_cleanup().unwrap()),
    );
    check(
        "desktopIdentity",
        value(
            db.trusted_desktop_identity("win-build", 3389, "ssh:bastion")
                .unwrap(),
        ),
    );
    check(
        "managementRequests",
        value(
            db.list_management_requests(ManagementProtocol::Gnmi, Some(&id("gnmi")))
                .unwrap(),
        ),
    );
    check(
        "managementShared",
        value(
            db.list_management_requests(ManagementProtocol::Gnmi, None)
                .unwrap(),
        ),
    );
}

#[test]
fn stored_json_columns_round_trip_byte_for_byte() {
    // Re-saving what the TypeScript stored must write the very same JSON text.
    let dir = tempfile::tempdir().unwrap();
    let db = open_fixture(dir.path());
    let raw = |sql: &str| -> Vec<String> {
        let conn = rusqlite::Connection::open(dir.path().join("muxus.sqlite3")).unwrap();
        let mut statement = conn.prepare(sql).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let configs_before = raw(
        "SELECT native_config_json FROM connection_profiles WHERE kind <> 'openssh' ORDER BY id",
    );
    let layouts_before =
        raw("SELECT layout_json || multi_exec_groups_json FROM workspaces ORDER BY id");
    let metadata_before = raw(
        "SELECT COALESCE(keyword_highlights_json, '') || COALESCE(login_sequence_json, '') FROM connection_profiles ORDER BY id",
    );
    let tunnels_before = raw("SELECT COALESCE(ssh_options_json, '') FROM tunnels ORDER BY id");
    let folders_before = raw(
        "SELECT auth_json || COALESCE(login_sequence_json, '') FROM folder_settings ORDER BY id",
    );

    for host in db.list_saved_host_profiles().unwrap() {
        let input = muxus_core::api::SavedHostProfileInput {
            id: Some(host.id.clone()),
            name: host.name.clone(),
            profile: host.profile,
        };
        db.save_saved_host_profile(&input).unwrap();
        let patch = muxus_core::api::OpenSshMetadataPatch {
            keyword_highlights: host.metadata.keyword_highlights.into(),
            login_sequence: host.metadata.login_sequence.into(),
            ..Default::default()
        };
        db.update_saved_host_metadata(&host.id, &patch).unwrap();
    }
    for (alias, metadata) in db
        .open_ssh_metadata(&["production", "bastion", "new-name"])
        .unwrap()
    {
        let patch = muxus_core::api::OpenSshMetadataPatch {
            keyword_highlights: metadata.keyword_highlights.into(),
            login_sequence: metadata.login_sequence.into(),
            ..Default::default()
        };
        db.update_open_ssh_metadata(&alias, &patch).unwrap();
    }
    for workspace in db.list_workspaces().unwrap() {
        // Through the typed layout, the way the app saves.
        let record = workspace.to_record().unwrap();
        let input = muxus_store::db::WorkspaceInput::new(
            Some(record.id.clone()),
            record.name.clone(),
            &record.layout,
            record.multi_exec_groups.clone(),
        )
        .unwrap();
        db.save_workspace(&input, true).unwrap();
    }
    for tunnel in db.list_tunnels().unwrap() {
        let input = muxus_core::api::TunnelInput {
            id: Some(tunnel.id),
            name: tunnel.name,
            target: tunnel.target,
            ssh_options: tunnel.ssh_options,
            kind: tunnel.kind,
            bind_port: tunnel.bind_port,
            target_host: tunnel.target_host,
            target_port: tunnel.target_port,
            auto_start: Some(tunnel.auto_start),
            auto_reconnect: Some(tunnel.auto_reconnect),
        };
        db.save_tunnel(&input).unwrap();
    }
    for folder in db.list_folder_settings().unwrap() {
        db.upsert_folder_settings(&folder.path, &folder.auth)
            .unwrap();
        db.set_folder_login_sequence(&folder.id, folder.login_sequence.as_ref())
            .unwrap();
    }

    assert_eq!(
        raw(
            "SELECT native_config_json FROM connection_profiles WHERE kind <> 'openssh' ORDER BY id"
        ),
        configs_before
    );
    assert_eq!(
        raw("SELECT layout_json || multi_exec_groups_json FROM workspaces ORDER BY id"),
        layouts_before
    );
    assert_eq!(
        raw(
            "SELECT COALESCE(keyword_highlights_json, '') || COALESCE(login_sequence_json, '') FROM connection_profiles ORDER BY id"
        ),
        metadata_before
    );
    assert_eq!(
        raw("SELECT COALESCE(ssh_options_json, '') FROM tunnels ORDER BY id"),
        tunnels_before
    );
    assert_eq!(
        raw(
            "SELECT auth_json || COALESCE(login_sequence_json, '') FROM folder_settings ORDER BY id"
        ),
        folders_before
    );
}
