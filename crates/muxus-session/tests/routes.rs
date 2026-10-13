//! Ports of `tests/unit/server/*-routes.test.ts` (and the route halves of `folder-settings`,
//! `local-files` and `app-routes`): every route is a `Backend` method; the HTTP status and code
//! of a failure are on `ApiError`. Bearer-token checks have no in-process equivalent.

mod support;

use std::time::Duration;

use muxus_core::api::{
    ConnectionCheck, ConnectionCheckStatus, EditorFileSaveRequest, HostBulkDeleteRequest,
    HostBulkUpdateRequest, HostOrderRequest, HostUpsertRequest, OpenSshMetadataPatch,
    PasswordVaultUnlockPolicy, SavedHostProfileInput, SessionEndStatus, SessionHistorySettings,
    SessionLogCreateInput, SessionLogDirection, SessionLogFileSettings, SessionLoggingPolicyInput,
    TunnelInput, UpdateCheckResult, VaultSecretInput, WorkspaceSaveRequest, X11SettingsUpdate,
};
use muxus_core::protocol::{ProfileKind, TerminalProfile};
use muxus_session::routes::folders::FolderSettingsUpsert;
use muxus_session::routes::workspaces::WorkspacePatch;
use muxus_store::history::{HistoryEvent, PartPolicy, SessionHistoryQuery};
use serde_json::json;
use support::{Sandbox, body, start};

// --- hosts, order, snapshots -----------------------------------------------------------------

#[tokio::test]
async fn host_order_persists_one_order_across_openssh_hosts_and_saved_hosts() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let created = backend
        .save_saved_host(body(json!({
            "name": "Core router",
            "profile": { "kind": "telnet", "host": "router.example.test", "port": 23 },
        })))
        .await
        .unwrap();
    backend
        .put_host_order(body(json!({
            "hosts": [
                { "kind": "profile", "id": created.id },
                { "kind": "ssh", "alias": "web-prod" },
            ],
        })))
        .await
        .unwrap();
    let list = backend.saved_hosts().await.unwrap();
    assert_eq!(list.profiles[0].metadata.sort_order, Some(0));

    let duplicate = backend
        .put_host_order(body::<HostOrderRequest>(json!({
            "hosts": [{ "kind": "ssh", "alias": "same" }, { "kind": "ssh", "alias": "same" }],
        })))
        .await
        .unwrap_err();
    assert_eq!(duplicate.status, 400);
    assert!(duplicate.message.contains("duplicate"));
    let missing = backend
        .put_host_order(body(
            json!({ "hosts": [{ "kind": "profile", "id": "missing" }] }),
        ))
        .await
        .unwrap_err();
    assert_eq!(missing.status, 500);
    assert!(missing.message.contains("not found"), "{}", missing.message);
    sandbox.stop().await;
}

#[tokio::test]
async fn terminal_snapshots_round_trip_and_reject_empty_ones() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    backend
        .save_terminal_snapshot(
            "tab-1",
            "deploy@web-01:~$ uptime\r\n 09:15 up 42 days",
            Some(2),
        )
        .await
        .unwrap();
    let snapshot = backend.terminal_snapshot("tab-1").await.unwrap().unwrap();
    assert_eq!(snapshot.tab_id, "tab-1");
    assert_eq!(
        snapshot.data,
        "deploy@web-01:~$ uptime\r\n 09:15 up 42 days"
    );
    assert_eq!(snapshot.format_version, 2);
    assert_eq!(backend.terminal_snapshot("never-seen").await.unwrap(), None);
    let empty = backend
        .save_terminal_snapshot("tab-1", "", None)
        .await
        .unwrap_err();
    assert_eq!(empty.status, 400);
    sandbox.stop().await;
}

#[tokio::test]
async fn saved_hosts_store_ssh_settings_without_touching_ssh_config() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let created = backend
        .save_saved_host(body(json!({
            "id": "muxus-ssh-core-router",
            "name": "Core router",
            "profile": {
                "kind": "ssh", "target": "router.example.test", "useConfig": false,
                "user": "admin", "port": 2222, "identityFiles": ["~/.ssh/router"],
                "identitiesOnly": true, "proxyJump": ["bastion"],
            },
        })))
        .await
        .unwrap();
    let created = serde_json::to_value(&created).unwrap();
    assert_eq!(created["id"], "muxus-ssh-core-router");
    assert_eq!(created["profile"]["port"], 2222);
    let config = backend.ssh_config().await.unwrap();
    assert!(
        !config
            .hosts
            .iter()
            .any(|host| host.resolved.hostname == "router.example.test")
    );

    let pinned = backend
        .save_saved_host(body(json!({
            "name": "Pinned",
            "profile": {
                "kind": "ssh", "target": "router.example.test", "useConfig": false,
                "hostKeyFingerprint": "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8",
            },
        })))
        .await
        .unwrap();
    assert!(
        serde_json::to_value(&pinned).unwrap()["profile"]
            .get("hostKeyFingerprint")
            .is_none()
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn saved_hosts_keep_desktop_options_and_refuse_bad_ports() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let rdp = backend
        .save_saved_host(body(json!({
            "name": "Build server",
            "profile": {
                "kind": "rdp", "host": "win-build.example.test", "username": "CORP\\ci",
                "sshGateway": { "target": "bastion" }, "shareClipboard": false,
                "password": "hunter2",
            },
        })))
        .await
        .unwrap();
    let rdp = serde_json::to_value(&rdp).unwrap();
    assert_eq!(rdp["profile"]["port"], 3389);
    assert_eq!(rdp["profile"]["sshGateway"]["target"], "bastion");
    assert!(!rdp.to_string().contains("hunter2"));
    let bad_port = serde_json::from_value::<SavedHostProfileInput>(json!({
        "name": "Bad",
        "profile": { "kind": "vnc", "host": "lab", "port": 70000 },
    }));
    assert!(
        bad_port.is_err(),
        "a port past 65535 is refused while parsing"
    );
    let invalid = backend
        .save_saved_host(body(json!({
            "name": "Broken serial host",
            "profile": { "kind": "serial", "path": "", "baudRate": 0 },
        })))
        .await
        .unwrap_err();
    assert_eq!(invalid.status, 400);
    sandbox.stop().await;
}

#[tokio::test]
async fn saved_hosts_update_metadata_and_delete() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let created = backend
        .save_saved_host(body(json!({
            "name": "Console server",
            "profile": { "kind": "telnet", "host": "console.example.test", "port": 2323 },
        })))
        .await
        .unwrap();
    let organized = backend
        .patch_saved_host_metadata(
            &created.id,
            body(json!({
                "group": "Network lab", "color": "#22c55e",
                "terminalScheme": "solarized-dark",
            })),
        )
        .await
        .unwrap();
    assert_eq!(organized.metadata.group.as_deref(), Some("Network lab"));
    let empty = backend
        .patch_saved_host_metadata(&created.id, OpenSshMetadataPatch::default())
        .await
        .unwrap_err();
    assert_eq!(empty.status, 400);
    let update = backend
        .save_saved_host(body(json!({
            "id": created.id,
            "name": "USB console",
            "profile": {
                "kind": "serial", "path": "COM3", "baudRate": 9600, "dataBits": 8,
                "stopBits": 1, "parity": "none", "flowControl": "none",
            },
        })))
        .await
        .unwrap();
    assert_eq!(update.metadata.group.as_deref(), Some("Network lab"));
    assert_eq!(backend.saved_hosts().await.unwrap().profiles, vec![update]);
    assert!(backend.delete_saved_host(&created.id).await.unwrap());
    sandbox.stop().await;
}

// --- tunnels -----------------------------------------------------------------------------------

#[tokio::test]
async fn saved_tunnels_round_trip_and_refuse_credentials() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let saved = backend
        .save_tunnel(body(json!({
            "name": "Database", "target": "db.internal",
            "sshOptions": {
                "user": "deploy", "port": 2222, "identityFiles": ["~/.ssh/work_ed25519"],
                "identitiesOnly": true, "proxyJump": ["bastion"],
            },
            "type": "local", "bindPort": 5432, "targetHost": "localhost", "targetPort": 5432,
        })))
        .await
        .unwrap();
    assert_eq!(saved.name.as_deref(), Some("Database"));
    assert_eq!(
        backend.tunnels().await.unwrap().tunnels,
        vec![saved.clone()]
    );
    let leaky = serde_json::from_value::<TunnelInput>(json!({
        "target": "db.internal",
        "sshOptions": { "user": "deploy", "password": "do-not-store-this" },
        "type": "dynamic", "bindPort": 1080,
    }));
    assert!(
        leaky.is_err(),
        "the strict sshOptions schema refuses a password"
    );

    let switches = json!({
        "target": "db", "type": "dynamic", "bindPort": 1080,
        "autoStart": true, "autoReconnect": true,
    });
    let created = backend.save_tunnel(body(switches.clone())).await.unwrap();
    assert_eq!((created.auto_start, created.auto_reconnect), (true, true));
    let mut off = switches;
    off["id"] = json!(created.id);
    off["autoStart"] = json!(false);
    let switched = backend.save_tunnel(body(off)).await.unwrap();
    assert_eq!(switched.id, created.id);
    assert_eq!(
        (switched.auto_start, switched.auto_reconnect),
        (false, true)
    );
    assert!(backend.delete_tunnel(&created.id).await.unwrap());
    let missing_target = backend
        .save_tunnel(body(
            json!({ "target": "db", "type": "local", "bindPort": 1 }),
        ))
        .await
        .unwrap_err();
    assert_eq!(missing_target.status, 400);
    sandbox.stop().await;
}

#[tokio::test]
async fn forwards_refuse_bad_requests_and_unknown_ids() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let missing = backend
        .start_forward(body(
            json!({ "connId": "gone", "type": "dynamic", "bindPort": 1080 }),
        ))
        .await
        .unwrap_err();
    assert_eq!(missing.status, 404);
    let bad = backend
        .start_forward(body(
            json!({ "connId": "", "type": "dynamic", "bindPort": 1080 }),
        ))
        .await
        .unwrap_err();
    assert_eq!(bad.status, 400);
    let unknown = backend
        .assign_forward_tunnel("nope", "tunnel")
        .await
        .unwrap_err();
    assert_eq!(
        (unknown.status, unknown.message.as_str()),
        (404, "forward not found")
    );
    let no_tunnel = backend.assign_forward_tunnel("nope", "").await.unwrap_err();
    assert_eq!(
        (no_tunnel.status, no_tunnel.message.as_str()),
        (400, "tunnelId is required")
    );
    assert!(backend.forwards(None).await.unwrap().is_empty());
    assert!(
        backend
            .connections_list()
            .await
            .unwrap()
            .connections
            .is_empty()
    );
    sandbox.stop().await;
}

// --- folders -----------------------------------------------------------------------------------

const MASTER: &str = "master-pass-12";

#[tokio::test]
async fn folder_settings_round_trip_and_drop_rows_that_hold_nothing() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let saved = backend
        .put_folder_settings(body(json!({
            "path": "Prod/EU",
            "auth": { "user": " ops ", "port": 2222, "identityFiles": [" ~/.ssh/eu ", ""] },
        })))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.path, "Prod/EU");
    assert_eq!(saved.auth.user.as_deref(), Some("ops"));
    assert_eq!(
        saved.auth.identity_files,
        Some(vec!["~/.ssh/eu".to_string()])
    );
    assert!(!saved.has_password);
    assert_eq!(backend.folder_settings().await.unwrap().folders.len(), 1);
    let cleared = backend
        .put_folder_settings(body(json!({ "path": "Prod/EU", "auth": { "user": "" } })))
        .await
        .unwrap();
    assert_eq!(cleared, None);
    assert!(backend.folder_settings().await.unwrap().folders.is_empty());
    let invalid = backend
        .put_folder_settings(FolderSettingsUpsert {
            path: "/".into(),
            ..FolderSettingsUpsert::default()
        })
        .await
        .unwrap_err();
    assert_eq!(
        (invalid.status, invalid.message.as_str()),
        (400, "a folder path is required")
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn folder_passwords_go_through_the_vault() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let unconfigured = backend
        .put_folder_password("Prod", "folder-secret", None)
        .await
        .unwrap_err();
    assert_eq!(unconfigured.code.as_deref(), Some("vault-not-configured"));
    assert!(backend.folder_settings().await.unwrap().folders.is_empty());

    backend.create_vault(MASTER, None).await.unwrap();
    let folder = backend
        .put_folder_password("Prod", "folder-secret", None)
        .await
        .unwrap();
    assert!(folder.has_password);
    let vault = backend.vault_status().await.unwrap();
    assert_eq!(vault.credentials.len(), 1);
    assert_eq!(vault.credentials[0].label, "Folder Prod");

    let moved = backend
        .move_folder_settings("Prod", "Production")
        .await
        .unwrap();
    assert_eq!((moved.moved, moved.destination_preserved), (1, false));
    assert_eq!(
        backend.vault_status().await.unwrap().credentials[0].label,
        "Folder Production"
    );
    assert!(backend.delete_folder_password("Production").await.unwrap());
    assert!(backend.folder_settings().await.unwrap().folders.is_empty());
    assert_eq!(backend.vault_status().await.unwrap().credential_count, 0);
    sandbox.stop().await;
}

#[tokio::test]
async fn folder_moves_report_a_preserved_destination_and_deletes_take_the_password() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    backend.create_vault(MASTER, None).await.unwrap();
    for (path, user) in [("A", "a-user"), ("B", "b-user")] {
        backend
            .put_folder_settings(body(json!({ "path": path, "auth": { "user": user } })))
            .await
            .unwrap();
    }
    let moved = backend.move_folder_settings("A", "B").await.unwrap();
    assert!(moved.destination_preserved);
    let folders = backend.folder_settings().await.unwrap().folders;
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].auth.user.as_deref(), Some("b-user"));

    backend.put_folder_password("B", "pw", None).await.unwrap();
    assert_eq!(backend.delete_folder_settings("B").await.unwrap(), 1);
    assert_eq!(backend.vault_status().await.unwrap().credential_count, 0);
    sandbox.stop().await;
}

#[tokio::test]
async fn folder_defaults_feed_the_resolved_host_list() {
    let sandbox = start().await;
    sandbox.write_ssh_config(&["Host web", "  HostName web.internal"]);
    let backend = &sandbox.backend;
    backend
        .patch_ssh_host_metadata("web", body(json!({ "group": "Prod" })))
        .await
        .unwrap();
    backend
        .put_folder_settings(body(
            json!({ "path": "Prod", "auth": { "user": "deploy", "port": 2200 } }),
        ))
        .await
        .unwrap();
    let config = backend.ssh_config().await.unwrap();
    let web = config
        .hosts
        .iter()
        .find(|host| host.alias == "web")
        .unwrap();
    assert_eq!(web.resolved.user.as_deref(), Some("deploy"));
    assert_eq!(web.resolved.port, 2200);
    assert_eq!(
        web.metadata.as_ref().unwrap().group.as_deref(),
        Some("Prod")
    );
    sandbox.stop().await;
}

// --- password vault ----------------------------------------------------------------------------

#[tokio::test]
async fn the_vault_gates_reveal_and_edit_with_the_master_password() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let initial = backend.vault_status().await.unwrap();
    assert!(!initial.configured && initial.locked && initial.credential_count == 0);
    assert_eq!(
        backend
            .create_vault("1234567", None)
            .await
            .unwrap_err()
            .status,
        400
    );
    let created = backend.create_vault(MASTER, None).await.unwrap();
    assert_eq!(
        created.unlock_policy,
        Some(PasswordVaultUnlockPolicy::Never)
    );
    assert!(created.configured && !created.locked);

    let account = muxus_store::vault::ssh_password_account("alice", "router.example", 22);
    backend
        .vault()
        .remember_ssh_password(&account, "alice@router.example:22", "remote-password", None)
        .unwrap();
    let credential = backend.vault_status().await.unwrap().credentials[0].clone();
    let wrong = backend
        .reveal_vault_credential(&credential.id, "incorrect-pass")
        .await
        .unwrap_err();
    assert_eq!(
        (wrong.status, wrong.code.as_deref()),
        (401, Some("invalid-master-password"))
    );
    assert_eq!(
        backend
            .reveal_vault_credential(&credential.id, MASTER)
            .await
            .unwrap(),
        "remote-password"
    );
    backend
        .update_vault_credential(&credential.id, MASTER, "changed-password")
        .await
        .unwrap();
    assert_eq!(
        backend
            .vault()
            .ssh_password(&account, None)
            .unwrap()
            .unwrap()
            .as_str(),
        "changed-password"
    );
    let per_credential = backend
        .change_vault_unlock_policy(MASTER, PasswordVaultUnlockPolicy::Credential)
        .await
        .unwrap();
    assert_eq!(
        per_credential.unlock_policy,
        Some(PasswordVaultUnlockPolicy::Credential)
    );
    assert!(per_credential.locked);
    backend
        .change_vault_unlock_policy(MASTER, PasswordVaultUnlockPolicy::Startup)
        .await
        .unwrap();
    backend.vault().lock();
    assert!(!backend.unlock_vault(MASTER).await.unwrap().locked);
    let reset = backend.reset_vault().await.unwrap();
    assert!(!reset.configured && reset.locked && reset.credential_count == 0);
    sandbox.stop().await;
}

fn secret(name: &str, value: Option<&str>, master: &str) -> VaultSecretInput {
    VaultSecretInput {
        name: name.into(),
        username: None,
        value: value.map(str::to_string),
        master_password: master.into(),
    }
}

#[tokio::test]
async fn named_secrets_are_saved_renamed_revealed_and_forgotten() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let unconfigured = backend
        .create_vault_secret(secret("Core enable", Some("enable-secret"), MASTER))
        .await
        .unwrap_err();
    assert_eq!(
        (unconfigured.status, unconfigured.code.as_deref()),
        (409, Some("vault-not-configured"))
    );
    backend.create_vault(MASTER, None).await.unwrap();
    assert_eq!(
        backend
            .create_vault_secret(secret("Core enable", None, MASTER))
            .await
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(
        backend
            .create_vault_secret(secret(
                "Core enable",
                Some("enable-secret"),
                "incorrect-pass"
            ))
            .await
            .unwrap_err()
            .status,
        401
    );
    let created = backend
        .create_vault_secret(VaultSecretInput {
            username: Some("admin".into()),
            ..secret("Core enable", Some("enable-secret"), MASTER)
        })
        .await
        .unwrap();
    assert_eq!(created.secret.name, "Core enable");
    assert_eq!(created.secret.username.as_deref(), Some("admin"));
    assert_eq!(created.status.credential_count, 0);
    assert_eq!(created.status.secrets, vec![created.secret.clone()]);
    let duplicate = backend
        .create_vault_secret(secret("core enable", Some("other"), MASTER))
        .await
        .unwrap_err();
    assert_eq!(
        (duplicate.status, duplicate.code.as_deref()),
        (409, Some("vault-secret-name-taken"))
    );
    let id = created.secret.id.clone();
    let renamed = backend
        .update_vault_secret(&id, secret("Edge enable", None, MASTER))
        .await
        .unwrap();
    assert_eq!(renamed.secret.name, "Edge enable");
    assert_eq!(renamed.secret.username, None);
    assert_eq!(
        backend
            .vault()
            .secret_value(&id, None)
            .unwrap()
            .unwrap()
            .as_str(),
        "enable-secret"
    );
    assert_eq!(
        backend.reveal_vault_credential(&id, MASTER).await.unwrap(),
        "enable-secret"
    );
    assert!(backend.delete_vault_credential(&id).await.unwrap());
    let gone = backend
        .update_vault_secret(&id, secret("Edge enable", None, MASTER))
        .await
        .unwrap_err();
    assert_eq!(
        (gone.status, gone.code.as_deref()),
        (404, Some("vault-secret-missing"))
    );
    sandbox.stop().await;
}

// --- ssh config --------------------------------------------------------------------------------

#[tokio::test]
async fn openssh_host_metadata_is_validated_and_kept_out_of_ssh_config() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let saved = backend
        .patch_ssh_host_metadata(
            "production",
            body(json!({
                "terminalScheme": "catppuccin-mocha", "terminalFontColor": "#cdd6f4",
                "terminalBackgroundColor": "#1e1e2e", "disableSftp": true,
                "consoleCompatibility": true,
            })),
        )
        .await
        .unwrap();
    assert_eq!(saved.disable_sftp, Some(true));
    assert_eq!(saved.terminal_scheme.as_deref(), Some("catppuccin-mocha"));
    let bad_color = backend
        .patch_ssh_host_metadata("production", body(json!({ "terminalFontColor": "red" })))
        .await
        .unwrap_err();
    assert_eq!(bad_color.status, 400);
    assert!(!sandbox.home().join(".ssh").join("config").exists());
    sandbox.stop().await;
}

#[tokio::test]
async fn bulk_host_updates_patch_every_alias_or_nothing() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    sandbox.write_ssh_config(&[
        "Host a",
        "  User old",
        "",
        "Host b",
        "  HostName b.internal",
    ]);
    let updated = backend
        .patch_ssh_hosts(body(
            json!({ "aliases": ["a", "b"], "options": { "user": "ops", "port": null } }),
        ))
        .await
        .unwrap();
    assert_eq!(updated.updated, 2);
    assert_eq!(
        sandbox.ssh_config(),
        [
            "Host a",
            "  User ops",
            "",
            "Host b",
            "  HostName b.internal",
            "  User ops",
            ""
        ]
        .join("\n")
    );
    let empty = backend
        .patch_ssh_hosts(body::<HostBulkUpdateRequest>(
            json!({ "aliases": ["a"], "options": {} }),
        ))
        .await
        .unwrap_err();
    assert_eq!(empty.status, 400);
    let unknown = backend
        .patch_ssh_hosts(body(
            json!({ "aliases": ["a", "gone"], "options": { "user": "x" } }),
        ))
        .await
        .unwrap_err();
    assert_eq!(unknown.status, 404);
    assert!(sandbox.ssh_config().contains("User ops"));
    sandbox.stop().await;
}

#[tokio::test]
async fn bulk_host_deletes_remove_every_alias_or_nothing() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    sandbox.write_ssh_config(&[
        "Host a",
        "  User old",
        "",
        "Host b",
        "  HostName b.internal",
        "",
        "Host c",
        "  User keep",
    ]);
    let empty = backend
        .delete_ssh_hosts(HostBulkDeleteRequest { aliases: vec![] })
        .await
        .unwrap_err();
    assert_eq!(empty.status, 400);
    let unknown = backend
        .delete_ssh_hosts(body(json!({ "aliases": ["a", "gone"] })))
        .await
        .unwrap_err();
    assert_eq!(unknown.status, 404);
    assert!(sandbox.ssh_config().contains("Host a"));
    let deleted = backend
        .delete_ssh_hosts(body(json!({ "aliases": ["a", "b"] })))
        .await
        .unwrap();
    assert_eq!(deleted.deleted, 2);
    assert_eq!(
        sandbox.ssh_config(),
        ["Host c", "  User keep", ""].join("\n")
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn host_upserts_write_identity_agent_and_carry_metadata_on_rename() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let request: HostUpsertRequest = body(json!({
        "aliases": ["muxus-identity-agent-route-test"],
        "options": { "hostname": "example.test", "identityAgent": "${ONEPASSWORD_SSH_AUTH_SOCK}" },
    }));
    let preview = backend.preview_ssh_host(request.clone()).await.unwrap();
    assert!(
        preview
            .text
            .contains("IdentityAgent ${ONEPASSWORD_SSH_AUTH_SOCK}")
    );
    backend.upsert_ssh_host(request).await.unwrap();
    assert!(
        sandbox
            .ssh_config()
            .contains("IdentityAgent ${ONEPASSWORD_SSH_AUTH_SOCK}")
    );

    backend
        .patch_ssh_host_metadata(
            "muxus-identity-agent-route-test",
            body(json!({ "group": "Lab" })),
        )
        .await
        .unwrap();
    backend
        .upsert_ssh_host(body(json!({
            "aliases": ["renamed"],
            "previousAlias": "muxus-identity-agent-route-test",
            "options": { "hostname": "example.test" },
        })))
        .await
        .unwrap();
    let config = backend.ssh_config().await.unwrap();
    let renamed = config
        .hosts
        .iter()
        .find(|host| host.alias == "renamed")
        .unwrap();
    assert_eq!(
        renamed.metadata.as_ref().unwrap().group.as_deref(),
        Some("Lab")
    );
    let no_alias = backend
        .upsert_ssh_host(body(json!({ "aliases": [], "options": {} })))
        .await
        .unwrap_err();
    assert_eq!(no_alias.status, 400);
    backend.delete_ssh_host("renamed").await.unwrap();
    assert!(!sandbox.ssh_config().contains("Host renamed"));
    sandbox.stop().await;
}

#[tokio::test]
async fn key_detection_uses_the_requested_agent_source() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let none = backend.ssh_keys(Some("none")).await.unwrap();
    assert!(!none.agent_available && none.agent_keys.is_empty());
    let missing = sandbox.home().join("missing-agent.sock");
    let unreachable = backend
        .ssh_keys(Some(missing.to_str().unwrap()))
        .await
        .unwrap();
    assert!(!unreachable.agent_available && unreachable.agent_keys.is_empty());
    sandbox.stop().await;
}

// --- session history ---------------------------------------------------------------------------

fn seed_session(sandbox: &Sandbox) -> String {
    let history = sandbox.backend.history();
    let policy = PartPolicy {
        max_part_bytes: 1024 * 1024,
        max_parts: 2,
    };
    let id = history.begin_session(
        &SessionLogCreateInput {
            profile_key: "ssh:production".into(),
            title: "Production".into(),
            kind: ProfileKind::Ssh,
            host: "production".into(),
            started_at: "2026-07-24T12:00:00.000Z".into(),
            capture_input: false,
        },
        policy,
    );
    assert!(history.append(
        &id,
        vec![HistoryEvent {
            sequence: 1,
            recorded_at: "2026-07-24T12:00:01.000Z".into(),
            elapsed_ms: 1_000,
            direction: SessionLogDirection::Output,
            raw: b"\x1b[32mdeploy complete\x1b[0m\r\n".to_vec(),
            text: "deploy complete\n".into(),
            line_timestamps: None,
        }],
        policy,
    ));
    history.finish_session(&id, SessionEndStatus::Completed, "2026-07-24T12:00:02.000Z");
    id
}

#[tokio::test]
async fn session_history_searches_and_exports_transcripts() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let id = seed_session(&sandbox);
    let found = backend
        .session_history(SessionHistoryQuery {
            query: Some("deploy".into()),
            ..SessionHistoryQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(found.sessions.len(), 1);
    assert_eq!(found.sessions[0].id, id);
    let detail = backend.session_log(&id, Some("deploy")).await.unwrap();
    assert_eq!(detail.events.len(), 1);

    let clean = backend.export_session_log_clean(&id, false).await.unwrap();
    assert!(clean.content_type.starts_with("text/plain"));
    assert!(clean.file_name.ends_with("-clean.txt"));
    let text = String::from_utf8(clean.body).unwrap();
    assert!(text.contains("deploy complete") && !text.contains('\x1b'));
    backend.export_session_log_clean(&id, true).await.unwrap();
    let replay = backend.export_session_log_replay(&id).await.unwrap();
    let html = String::from_utf8(replay.body).unwrap();
    assert!(html.contains("Muxus replay") && html.contains("deploy complete"));
    let raw = backend.export_session_log_raw(&id).await.unwrap();
    assert_eq!(raw.content_type, "application/x-ndjson");
    let line: serde_json::Value =
        serde_json::from_str(String::from_utf8(raw.body).unwrap().trim()).unwrap();
    assert_eq!(line["sequence"], 1);
    assert_eq!(line["direction"], "output");
    assert_eq!(line["encoding"], "base64");
    assert_eq!(
        backend
            .session_log("missing", None)
            .await
            .unwrap_err()
            .status,
        404
    );
    sandbox.stop().await;
}

#[tokio::test]
async fn session_history_round_trips_host_policies_with_safe_bounds() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let policy = |enabled, capture_input, max_part_bytes, max_parts, log_to_file| {
        SessionLoggingPolicyInput {
            enabled,
            capture_input,
            max_part_bytes,
            max_parts,
            log_to_file,
        }
    };
    let saved = backend
        .put_session_logging_policy(
            "ssh:production",
            policy(true, true, 2 * 1024 * 1024, 6, None),
        )
        .await
        .unwrap();
    assert_eq!(saved.profile_key, "ssh:production");
    assert!(saved.capture_input && !saved.log_to_file && saved.overridden);
    assert_eq!(saved.max_parts, 6);
    let with_files = backend
        .put_session_logging_policy(
            "ssh:production",
            policy(false, false, 2 * 1024 * 1024, 6, Some(true)),
        )
        .await
        .unwrap();
    assert!(!with_files.enabled && with_files.log_to_file);
    let invalid = backend
        .put_session_logging_policy("ssh:production", policy(true, false, 1, 0, None))
        .await
        .unwrap_err();
    assert_eq!(invalid.status, 400);
    assert!(
        backend
            .delete_session_logging_policy("ssh:production")
            .await
            .unwrap()
    );
    let inherited = backend
        .session_logging_policy("ssh:production")
        .await
        .unwrap();
    assert!(!inherited.enabled && !inherited.capture_input && !inherited.overridden);
    sandbox.stop().await;
}

#[tokio::test]
async fn session_history_reports_storage_and_validates_retention() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    seed_session(&sandbox);
    let current = backend.session_history_storage().await.unwrap();
    assert_eq!(current.settings.max_total_bytes, 5 * 1024 * 1024 * 1024);
    assert_eq!(current.settings.min_free_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(current.settings.min_free_percent, 5.0);
    let elsewhere = sandbox.dir.path().join("history-on-another-disk");
    let saved = backend
        .put_session_history_storage(SessionHistorySettings {
            storage_location: Some(elsewhere.to_string_lossy().into_owned()),
            max_total_bytes: 64 * 1024 * 1024,
            min_free_bytes: 0,
            min_free_percent: 0.0,
            max_age_days: Some(30),
        })
        .await
        .unwrap();
    assert_eq!(saved.settings.max_age_days, Some(30));
    assert!(saved.restart_required);
    let relative = backend
        .put_session_history_storage(SessionHistorySettings {
            storage_location: Some("relative/history".into()),
            max_total_bytes: 64 * 1024 * 1024,
            min_free_bytes: 0,
            min_free_percent: 0.0,
            max_age_days: None,
        })
        .await
        .unwrap_err();
    assert_eq!(relative.status, 400);
    sandbox.stop().await;
}

#[tokio::test]
async fn session_history_pins_and_names_sessions() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let id = seed_session(&sandbox);
    assert!(backend.set_session_log_pinned(&id, true).await.unwrap());
    let listed = backend
        .session_history(SessionHistoryQuery::default())
        .await
        .unwrap();
    assert!(listed.sessions[0].pinned);
    assert!(
        backend
            .set_session_log_label(&id, "  Pre-change config ")
            .await
            .unwrap()
    );
    let found = backend
        .session_history(SessionHistoryQuery {
            query: Some("pre-change".into()),
            ..SessionHistoryQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(
        found.sessions[0].label.as_deref(),
        Some("Pre-change config")
    );
    let clean = backend.export_session_log_clean(&id, false).await.unwrap();
    assert_eq!(clean.file_name, "pre-change-config-clean.txt");
    let replay =
        String::from_utf8(backend.export_session_log_replay(&id).await.unwrap().body).unwrap();
    assert!(replay.contains("<title>Pre-change config — Muxus replay</title>"));
    let too_long = backend
        .set_session_log_label(&id, &"x".repeat(201))
        .await
        .unwrap_err();
    assert_eq!(too_long.status, 400);
    assert!(backend.set_session_log_label(&id, "").await.unwrap());
    sandbox.stop().await;
}

#[tokio::test]
async fn log_file_settings_are_saved_and_suggest_a_free_name() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let current = backend.session_log_files().await.unwrap();
    assert_eq!(
        current.settings.filename_pattern,
        "{host}_{date}_{time}.log"
    );
    assert!(current.active_directory.ends_with("Logs"));
    let directory = sandbox.dir.path().join("logs");
    std::fs::create_dir_all(&directory).unwrap();
    let directory_text = directory.to_string_lossy().into_owned();
    let saved = backend
        .put_session_log_files(SessionLogFileSettings {
            directory: Some(directory_text.clone()),
            filename_pattern: "{kind}/{title}.log".into(),
            timestamps: true,
        })
        .await
        .unwrap();
    assert_eq!(saved.active_directory, directory_text);
    for settings in [
        SessionLogFileSettings {
            directory: Some("relative".into()),
            filename_pattern: "{host}.log".into(),
            timestamps: false,
        },
        SessionLogFileSettings {
            directory: None,
            filename_pattern: "{hostname}.log".into(),
            timestamps: false,
        },
    ] {
        assert_eq!(
            backend
                .put_session_log_files(settings)
                .await
                .unwrap_err()
                .status,
            400
        );
    }
    let profile: TerminalProfile = body(json!({ "kind": "telnet", "host": "switch", "port": 23 }));
    let file = directory.join("telnet").join("Lab_switch.log");
    let first = backend
        .suggest_session_log_file(profile.clone(), Some("Lab/switch"))
        .await
        .unwrap();
    assert_eq!(first, file.to_string_lossy());
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "an earlier log").unwrap();
    let second = backend
        .suggest_session_log_file(profile, Some("Lab/switch"))
        .await
        .unwrap();
    assert_eq!(
        second,
        directory
            .join("telnet")
            .join("Lab_switch-1.log")
            .to_string_lossy()
    );
    sandbox.stop().await;
}

// --- workspaces --------------------------------------------------------------------------------

fn layout() -> serde_json::Value {
    json!({
        "version": 1,
        "root": {
            "id": "split-1", "type": "split", "direction": "horizontal", "ratio": 0.55,
            "children": [
                {
                    "id": "pane-1", "type": "pane", "activeTabId": "terminal-1",
                    "tabs": [{
                        "id": "terminal-1", "kind": "terminal", "title": "Production",
                        "profile": { "kind": "ssh", "target": "production" },
                        "cwdHint": "/srv/app", "pinned": true, "offerReconnect": true,
                    }],
                },
                {
                    "id": "pane-2", "type": "pane", "activeTabId": "sftp-1",
                    "tabs": [{
                        "id": "sftp-1", "kind": "sftp", "title": "Logs",
                        "connection": { "source": "openssh", "id": "production" },
                        "path": "/var/log",
                    }],
                },
            ],
        },
        "activePaneId": "pane-1",
    })
}

fn save_request(value: serde_json::Value) -> WorkspaceSaveRequest {
    body(value)
}

#[tokio::test]
async fn workspaces_lock_rename_open_and_delete() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let saved = backend
        .save_workspace(save_request(
            json!({ "name": "Stable startup", "layout": layout() }),
        ))
        .await
        .unwrap();
    let id = saved.id.clone();
    let read = backend.workspace(&id).await.unwrap();
    assert_eq!(serde_json::to_value(&read.layout).unwrap(), layout());

    let locked = backend
        .patch_workspace(&id, WorkspacePatch::SetLocked(true))
        .await
        .unwrap();
    assert!(locked.is_locked);
    let blocked = backend
        .save_workspace(save_request(json!({
            "id": id, "name": "Stable startup", "layout": { "version": 1, "root": null },
        })))
        .await
        .unwrap_err();
    assert_eq!(
        (blocked.status, blocked.code.as_deref()),
        (409, Some("workspace-locked"))
    );
    assert!(blocked.message.contains("is locked"));
    let explicit = backend
        .save_workspace(save_request(json!({
            "id": id, "name": "Stable startup", "layout": { "version": 1, "root": null },
            "overwriteLocked": true,
        })))
        .await
        .unwrap();
    assert!(explicit.is_locked && explicit.layout.root.is_none());
    backend
        .patch_workspace(&id, WorkspacePatch::SetLocked(false))
        .await
        .unwrap();
    let renamed = backend
        .patch_workspace(&id, WorkspacePatch::Rename("  Renamed ".into()))
        .await
        .unwrap();
    assert_eq!(renamed.name, "Renamed");
    let startup = backend
        .set_startup_workspace(Some(&id))
        .await
        .unwrap()
        .unwrap();
    assert!(startup.is_startup);
    assert!(
        backend
            .open_workspace(&id)
            .await
            .unwrap()
            .last_opened_at
            .is_some()
    );
    assert!(backend.delete_workspace(&id).await.unwrap());
    assert_eq!(backend.workspace(&id).await.unwrap_err().status, 404);
    assert_eq!(backend.startup_workspace().await.unwrap(), None);
    assert_eq!(backend.latest_workspace().await.unwrap(), None);
    sandbox.stop().await;
}

#[tokio::test]
async fn workspaces_resist_stale_windows() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let saved = backend
        .save_workspace(save_request(
            json!({ "name": "Authoritative name", "layout": layout() }),
        ))
        .await
        .unwrap();
    let id = saved.id.clone();
    let stale = backend
        .save_workspace(save_request(json!({
            "id": id, "name": "Stale window name", "layout": { "version": 1, "root": null },
        })))
        .await
        .unwrap();
    assert_eq!(stale.name, "Authoritative name");
    backend.delete_workspace(&id).await.unwrap();
    let after_delete = backend
        .save_workspace(save_request(json!({
            "id": id, "name": "Stale window name", "layout": { "version": 1, "root": null },
        })))
        .await
        .unwrap_err();
    assert_eq!(
        (
            after_delete.status,
            after_delete.code.as_deref(),
            after_delete.message.as_str()
        ),
        (404, Some("workspace-not-found"), "workspace not found")
    );
    let created = backend
        .save_workspace(save_request(json!({
            "id": id, "createIfMissing": true, "name": "Recreated intentionally",
            "layout": { "version": 1, "root": null },
        })))
        .await
        .unwrap();
    assert_eq!(created.name, "Recreated intentionally");
    let repeated = backend
        .save_workspace(save_request(json!({
            "id": id, "createIfMissing": true,
            "name": "Must not replace the existing workspace", "layout": layout(),
        })))
        .await
        .unwrap();
    assert_eq!(repeated.name, "Recreated intentionally");
    assert!(repeated.layout.root.is_none());
    let invalid = backend
        .save_workspace(save_request(json!({
            "name": "Bad split",
            "layout": {
                "version": 1,
                "root": {
                    "id": "s", "type": "split", "direction": "vertical", "ratio": 0.95,
                    "children": [
                        { "id": "a", "type": "pane", "tabs": [] },
                        { "id": "b", "type": "pane", "tabs": [] },
                    ],
                },
            },
        })))
        .await
        .unwrap_err();
    assert_eq!(invalid.status, 400);
    sandbox.stop().await;
}

// --- diagnostics -------------------------------------------------------------------------------

async fn closed_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn diagnostics_find_a_telnet_port_nothing_listens_on() {
    let sandbox = start().await;
    let port = closed_port().await;
    let report = sandbox
        .backend
        .diagnose_connection(body(
            json!({ "kind": "telnet", "host": "127.0.0.1", "port": port }),
        ))
        .await
        .unwrap();
    assert_eq!(report.checked, format!("127.0.0.1:{port}"));
    assert!(report.checks.contains(&ConnectionCheck {
        label: "TCP".into(),
        status: ConnectionCheckStatus::Fail,
        detail: format!("127.0.0.1:{port} refused the connection"),
    }));
    assert!(report.conclusion.contains("nothing accepts connections"));
    sandbox.stop().await;
}

#[tokio::test]
async fn diagnostics_read_the_greeting_of_an_ad_hoc_ssh_target() {
    let sandbox = start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        while let Ok((mut socket, _)) = listener.accept().await {
            let _ = socket.write_all(b"SSH-2.0-RouteTest\r\n").await;
        }
    });
    let report = sandbox
        .backend
        .diagnose_connection(body(
            json!({ "kind": "ssh", "target": format!("alice@127.0.0.1:{port}") }),
        ))
        .await
        .unwrap();
    assert_eq!(
        report.checks[0],
        ConnectionCheck {
            label: "Route".into(),
            status: ConnectionCheckStatus::Info,
            detail: format!("alice@127.0.0.1:{port}"),
        }
    );
    assert!(report.checks.contains(&ConnectionCheck {
        label: "SSH".into(),
        status: ConnectionCheckStatus::Ok,
        detail: "SSH-2.0-RouteTest".into(),
    }));
    sandbox.stop().await;
}

#[tokio::test]
async fn diagnostics_answer_a_broken_dial_plan_and_only_network_sessions() {
    let sandbox = start().await;
    let broken = sandbox
        .backend
        .diagnose_connection(body(
            json!({ "kind": "ssh", "target": "gone", "profileId": "missing-profile" }),
        ))
        .await
        .unwrap_err();
    assert_eq!(
        (broken.status, broken.message.as_str()),
        (422, "saved SSH profile \"missing-profile\" was not found")
    );
    let local = sandbox
        .backend
        .diagnose_connection(body(json!({ "kind": "local" })))
        .await
        .unwrap_err();
    assert_eq!(local.status, 400);
    sandbox.stop().await;
}

// --- local files -------------------------------------------------------------------------------

#[tokio::test]
async fn local_files_read_and_save_utf8_text_atomically() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let file = sandbox.dir.path().join("notes.txt");
    std::fs::write(&file, "first line\nsecond line\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let path = file.to_str().unwrap();
    let opened = backend.read_local_file(path).await.unwrap();
    assert_eq!(opened.content, "first line\nsecond line\n");
    #[cfg(unix)]
    assert_eq!(opened.mode, Some(0o640));
    let saved = backend
        .save_local_file(
            path,
            EditorFileSaveRequest {
                content: "changed\r\n".into(),
                expected_mtime_ms: opened.mtime_ms,
                force: None,
            },
        )
        .await
        .unwrap();
    assert!(saved.ok);
    assert_eq!(std::fs::read(&file).unwrap(), b"changed\r\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    let stale = backend
        .save_local_file(
            path,
            EditorFileSaveRequest {
                content: "lost".into(),
                expected_mtime_ms: Some(1),
                force: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (stale.status, stale.code.as_deref()),
        (409, Some("LOCAL_FILE_CHANGED"))
    );

    let binary = sandbox.dir.path().join("blob.bin");
    std::fs::write(&binary, [0u8, 1, 2]).unwrap();
    assert_eq!(
        backend
            .read_local_file(binary.to_str().unwrap())
            .await
            .unwrap_err()
            .status,
        415
    );
    assert_eq!(
        backend
            .read_local_file(sandbox.dir.path().to_str().unwrap())
            .await
            .unwrap_err()
            .status,
        400
    );
    assert_eq!(
        backend
            .read_local_file("relative.txt")
            .await
            .unwrap_err()
            .status,
        400
    );
    let missing = sandbox.dir.path().join("missing.txt");
    assert_eq!(
        backend
            .read_local_file(missing.to_str().unwrap())
            .await
            .unwrap_err()
            .status,
        404
    );
    sandbox.stop().await;
}

#[test]
fn clipboard_images_are_owner_only_and_size_limited() {
    let dir = tempfile::tempdir().unwrap();
    let file =
        muxus_session::routes::local_files::write_clipboard_image(dir.path(), b"\x89PNG").unwrap();
    assert!(
        file.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("muxus-paste-")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let huge = vec![0u8; 18 * 1024 * 1024 + 1];
    let refused =
        muxus_session::routes::local_files::write_clipboard_image(dir.path(), &huge).unwrap_err();
    assert_eq!(refused.status, 413);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

// --- app ---------------------------------------------------------------------------------------

/// Serves one canned HTTP answer per request on loopback; records the request lines.
async fn manifest_server(
    status: u16,
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/muxus/latest.json",
        listener.local_addr().unwrap()
    );
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = vec![0u8; 8192];
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buffer[..read]).into_owned());
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (url, requests)
}

#[tokio::test]
async fn update_checks_validate_manifests_and_trust_only_muxus_releases() {
    let (url, requests) = manifest_server(
        200,
        r#"{"version":"v0.12.0","releaseName":"Muxus 0.12","releaseUrl":"https://github.com/FloSch62/muxus/releases/tag/v0.12.0","publishedAt":"2026-07-26T08:00:00Z"}"#,
    )
    .await;
    let sandbox = support::start_with(|options| {
        options.update_manifest_url = url;
        options.app_version = "0.11.1".into();
    })
    .await;
    let result = sandbox.backend.update_check(true).await.unwrap();
    assert_eq!(
        result,
        UpdateCheckResult::Available {
            current_version: "0.11.1".into(),
            latest_version: "0.12.0".into(),
            release_name: Some("Muxus 0.12".into()),
            release_url: "https://github.com/FloSch62/muxus/releases/tag/v0.12.0".into(),
            published_at: Some("2026-07-26T08:00:00Z".into()),
        }
    );
    let request = requests.lock().unwrap()[0].clone();
    let request_line = request.lines().next().unwrap();
    assert!(
        regex::Regex::new(r"^GET /muxus/latest\.json\?t=\d+ HTTP/1\.1$")
            .unwrap()
            .is_match(request_line),
        "{request_line}"
    );
    let lower = request.to_lowercase();
    assert!(lower.contains("accept: application/json"));
    assert!(lower.contains("user-agent: muxus/0.11.1"));
    // Without force the run's first answer is reused.
    assert_eq!(sandbox.backend.update_check(false).await.unwrap(), result);
    assert_eq!(requests.lock().unwrap().len(), 1);
    sandbox.stop().await;

    let (url, _) = manifest_server(
        200,
        r#"{"version":"0.12.0","releaseUrl":"https://attacker.example/FloSch62/muxus/releases/tag/v0.12.0"}"#,
    )
    .await;
    let sandbox = support::start_with(|options| {
        options.update_manifest_url = url;
        options.app_version = "0.11.1".into();
    })
    .await;
    assert_eq!(
        sandbox.backend.update_check(true).await.unwrap(),
        UpdateCheckResult::NotAvailable {
            current_version: "0.11.1".into(),
            latest_version: Some("0.12.0".into()),
            reason: Some("missing-release-url".into()),
        }
    );
    sandbox.stop().await;

    let (url, _) = manifest_server(404, "{}").await;
    let sandbox = support::start_with(|options| options.update_manifest_url = url).await;
    let no_release = sandbox.backend.update_check(false).await.unwrap();
    assert!(matches!(
        no_release,
        UpdateCheckResult::NotAvailable { reason: Some(ref reason), .. } if reason == "no-release"
    ));
    sandbox.stop().await;
}

#[tokio::test]
async fn x11_status_applies_settings_over_the_platform_defaults() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let initial = backend.x11_status().await.unwrap();
    assert!(!initial.clipboard);
    let accepted = backend
        .put_x11_settings(X11SettingsUpdate {
            enabled: Some(false),
            forward_by_default: Some(true),
            clipboard: true,
        })
        .await
        .unwrap();
    assert!(!accepted.enabled && accepted.forward_by_default && accepted.clipboard);
    let reverted = backend
        .put_x11_settings(X11SettingsUpdate {
            enabled: None,
            forward_by_default: None,
            clipboard: false,
        })
        .await
        .unwrap();
    assert_eq!(reverted.enabled, reverted.defaults.enabled);
    assert_eq!(
        reverted.forward_by_default,
        reverted.defaults.forward_by_default
    );
    assert!(serde_json::from_value::<X11SettingsUpdate>(json!({ "clipboard": "yes" })).is_err());
    sandbox.stop().await;
}

#[tokio::test]
async fn app_info_lists_no_wsl_distributions_off_windows() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    let info = backend.app_info().await.unwrap();
    assert_eq!(info.name, "Muxus");
    assert_eq!(info.home_dir, sandbox.home().to_string_lossy());
    assert!(info.ssh_algorithms.contains_key("Ciphers"));
    if !cfg!(windows) {
        assert!(
            backend
                .wsl_distributions()
                .await
                .unwrap()
                .distributions
                .is_empty()
        );
    }
    let _ = Duration::from_secs(0);
    sandbox.stop().await;
}

#[tokio::test]
async fn shutdown_closes_history_and_refuses_later_calls() {
    let sandbox = start().await;
    let backend = sandbox.backend.clone();
    backend.shutdown().await;
    let refused = backend.tunnels().await.unwrap_err();
    assert_eq!(
        (refused.status, refused.message.as_str()),
        (500, "the backend has shut down")
    );
    assert!(
        backend
            .history()
            .session_history(SessionHistoryQuery::default())
            .await
            .is_err(),
        "session history was closed"
    );
    // Shutting down twice is harmless.
    sandbox.stop().await;
}

#[tokio::test]
async fn preferences_live_in_client_state_json_of_the_data_folder() {
    let sandbox = start().await;
    let backend = &sandbox.backend;
    assert_eq!(
        backend.paths().client_state_file(),
        sandbox.data().join("client-state.json")
    );
    let prefs = backend.prefs().await.unwrap();
    backend.set_prefs(prefs.clone()).await.unwrap();
    let file = std::fs::read_to_string(sandbox.data().join("client-state.json")).unwrap();
    assert!(file.contains("muxus-prefs"));
    assert_eq!(backend.reload_prefs().await.unwrap(), prefs);
    backend
        .set_client_state_item("muxus-dismissed-update", Some("0.12.0"))
        .await
        .unwrap();
    assert_eq!(
        backend
            .client_state_item("muxus-dismissed-update")
            .await
            .unwrap()
            .as_deref(),
        Some("0.12.0")
    );
    backend
        .set_client_state_item("muxus-dismissed-update", None)
        .await
        .unwrap();
    assert_eq!(
        backend
            .client_state_item("muxus-dismissed-update")
            .await
            .unwrap(),
        None
    );
    sandbox.stop().await;
}
