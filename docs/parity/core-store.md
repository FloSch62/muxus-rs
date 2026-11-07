# Parity: core-store (`muxus-core`, `muxus-store`)

`muxus-core` ports `shared/src` (domain types, the session protocol, pure helpers).
`muxus-store` ports the persistence of the TypeScript build: the SQLite database
(`server/src/persistence`), the password vault (`server/src/security`), the
session history storage (`server/src/session-logging/history-*`), the desktop
preferences (`client-state.json`, zustand persist) and the data-folder rules.

Both crates are the foundation of the next wave; their public API is listed at
the end of this file. Run their tests with:

```bash
CARGO_TARGET_DIR=target-core cargo test -p muxus-core -p muxus-store
CARGO_TARGET_DIR=target-core cargo test -p muxus-store --test vault -- --ignored   # default-KDF vault (8 s unoptimized)
```

## Status

### muxus-core

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Domain and REST types | shared/src/api-types.ts | done | `muxus_core::api`; camelCase, absent optionals skipped, `T \| null` patches as `js::Patch` |
| Session profiles (8 kinds) + defaults | shared/src/ws-protocol.ts | done | `protocol::{SessionProfile, TerminalProfile, DesktopProfile, ManagementProfile, SavedHostSessionProfile}` |
| Web page hosts | shared/src/ws-protocol.ts (`webProfileSchema`) | done | `protocol::WebProfile`, `SavedHostSessionProfile::Web`, `ProfileKind::Web`; no `SessionProfile` (`TryFrom` returns the web profile as the error) |
| Fallback jump routes and gateways | shared/src/ws-protocol.ts, routes/metadata-schema.ts | done | `SshProfile::proxy_jump_fallbacks`, `ssh_gateway_fallbacks` on RDP/VNC/gNMI/NETCONF, `MAX_JUMP_FALLBACKS`/`MAX_JUMP_HOPS` |
| Environment targets on local/SSH sessions | shared/src/ws-protocol.ts | done | `LocalProfile::environment`, `SshProfile::environment` |
| Environments (containers, pods, labs, VMs) | shared/src/environments.ts, routes/environments.ts (`actionSchema`, `portSchema`) | done | `environments`: name checks, argv, quoting, titles, discovery/response types, `EnvironmentAction`, `EnvironmentPortRequest`; tests `tests/environments.rs` |
| Script library and runs | shared/src/api-types.ts, routes/scripts.ts | done | `api::{SavedScript, SavedScriptInput, ScriptRunRequest, ScriptRunStatus, ScriptRunState}` with the route limits |
| Anti-idle, terminal keys and echo, system OpenSSH, environment discovery override | shared/src/api-types.ts, routes/metadata-schema.ts | done | `OpenSshProfileMetadata`/`OpenSshMetadataPatch` fields with the route's validation; `TERMINAL_BACKSPACE_KEYS`/`TERMINAL_ENTER_KEYS` |
| Anti-idle drafts | client/src/anti-idle.ts | done | `anti_idle` (shared by the host editors, bulk edit and backups) |
| Prompt waiting drafts | client/src/terminal/paste-prompt.ts | done | `paste_prompt` |
| Terminal input modes | client/src/terminal/input-modes.ts | done | `input_modes` |
| AppImage environment cleanup | shared/src/appimage-env.ts | done | `appimage_env` |
| Host batch create, `listedHostFor`, `new-window` launch, independent forwards, paste stop progress, `systemOpenSsh` ready flag | shared/src/api-types.ts, ws-protocol.ts | done | types only |
| Terminal / desktop / management messages | shared/src/ws-protocol.ts | done | client and server enums tagged by `op`; desktop certificate challenge flattened |
| zod validation and limits | shared/src/ws-protocol.ts | done | `validate::Validate` + `parse_value`; zod 4 messages; UTF-16 lengths, JS `trim` |
| Route schemas (metadata patch, login sequence, keyword rules, tunnel, saved host, workspace save) | server/src/routes/{metadata-schema,tunnels,profiles,workspaces}.ts | done | `Validate` impls in `api`; JS regex validity uses an Annex B checker, not V8 |
| Golden JSON vs zod output | ws-protocol.ts + route schemas | done | 118 TypeScript-generated cases, byte-identical (`tests/golden.rs`) |
| ssh:// and telnet:// links | shared/src/connection-links.ts | done | identical error messages |
| gNMI path strings | shared/src/gnmi-path.ts | done | key order kept, UTF-16 error positions |
| Management types | shared/src/management.ts | done | |
| Paste pacing | shared/src/paste-pacing.ts | done | prompt waiting (`PastePromptWait`, `PastePromptWaitSetting`, `paste_pattern_problem`) and the host's Enter key included; the client-only helpers in client/src/terminal/paste-pacing.ts are not ported (terminal view) |
| Session transcript | shared/src/session-transcript.ts | done | UTF-16 offsets |
| Update version precedence | shared/src/update-version.ts | done | |
| Session log file names | shared/src/api-types.ts | done | `session_log_file` |
| Folder paths | server/src/util/folder-paths.ts | done | |
| JavaScript semantics | (implicit) | done | `js`: UTF-16 length/prefix, `trim`, `JSON.stringify` numbers, ISO timestamps |

### muxus-store

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Data folder rules | electron/src/main.ts, server/src/config.ts, session-logging/history-store.ts | done | `paths`: Electron `userData` (`$XDG_CONFIG_HOME/Muxus`, macOS/Windows equivalents), `-development`, server default `$XDG_DATA_HOME/muxus`, `MUXUS_HISTORY_PATH`, history root precedence; macOS/Windows rules unit-tested, not run there |
| Schema + migrations 1–37 | server/src/persistence/database.ts | done | SQL copied verbatim; a fresh file has the TypeScript schema byte for byte (`tests/db_schema.rs`); table rebuilds of v22/v29/v36 (`addWebHosts`, skipped when the table already admits `web`) ported |
| Newer database refused | database.ts `migrate()` | done | same error text, file untouched |
| Current TypeScript database opens unchanged | — | done | populated TypeScript fixture (v0.15, schema 37): schema unchanged, all 40 recorded reads equal the TypeScript return values (`tests/db_ts_fixture.rs`) |
| OpenSSH metadata, groups, host order, alias rename | database.ts | done | includes prompt waiting, anti-idle, keys/echo, system OpenSSH, discovery override and (aliases only) fallback jump routes; `system_open_ssh_for_alias`/`_for_saved_host`, `jump_fallbacks_for_alias`, `host_anti_idle`, `host_input_keys` |
| Saved hosts (all 8 kinds, web included), native connections | database.ts | done | stored config and metadata JSON round-trip byte for byte |
| Scripts | database.ts (`listScripts`, `saveScript`, `deleteScript`) | done | `list_scripts`, `save_script`, `delete_script` |
| Workspaces, startup/lock, default names, snapshot pruning | database.ts, routes/workspaces.ts | done | `put_workspace` is the storage side of `PUT /api/workspaces` |
| Terminal snapshots | database.ts, routes/terminal-snapshots.ts | done | |
| Tunnels | database.ts, routes/tunnels.ts | done | |
| Logging policies, history limits, log file settings | database.ts, routes/session-history.ts | done | |
| Folder settings, moves/merges/deletes | database.ts, routes/folders.ts | done | `merge_folder_auth`, `folder_settings_chain`; `folderAuthOptionLines` belongs to muxus-ssh |
| Login sequences + inheritance | database.ts, login-sequence/resolve.ts | done | `resolve_login_sequence` |
| Management requests, desktop identity pins | database.ts | done | |
| Credential refs, vault header, sealed values | database.ts | done | WAL checkpoint / VACUUM after deletes, as in TypeScript |
| `assertSecretFree` | database.ts | done | |
| Password vault (crypto, policies, secrets) | server/src/security/password-vault.ts | done | scrypt + AES-256-GCM + HMAC key check; TypeScript vaults open in Rust and Rust vaults open in TypeScript (fixtures + `verify.mts`) |
| OS credential store | server/src/security/vault-key-store.ts | partial | same service/account and the same store crates as `@napi-rs/keyring` (Secret Service, then keyutils); the real-keyring smoke test is opt-in (`MUXUS_OS_KEYRING_SMOKE=1`) and was not run; macOS/Windows backends not compiled here |
| Typing vault secrets | server/src/security/vault-secret-input.ts | done | writers are closures; `TerminalInputs` lives in muxus-session |
| Development database seeding | electron/src/development-database.ts | done | backup API, vault id namespacing, key copy |
| Session history store (async facade, worker thread) | session-logging/history-store.ts | done | async-channel facade, failure listeners, 8 MiB backpressure |
| History root storage (schema, .muxlog segments, zstd, trash) | session-logging/history-worker.js | done | byte-compatible both ways, checked against the TypeScript build |
| History search, preview window, cursors, raw export | history-worker.js | done | FTS prefix, snippet markers, match counts, context anchoring |
| History retention, quota and maintenance | history-worker.js | done | same thresholds and warning text; Windows disk space not built here |
| Crash recovery and v6 legacy import | history-worker.js | done | then `Database::finalize_session_history_separation` |
| `client-state.json` store | electron/src/main.ts | done | key order, 0600, tmp + rename |
| Prefs envelope + rehydrate | client/src/state/prefs.ts, zustand persist | done | byte-equal golden cases (v0, v8, v15, v17, v18, none, bad JSON, empty) |
| Prefs migration v0–17 + merge | client/src/state/prefs.ts | done | v0.15 keys: `pasteWarnLookalikes`, `closeTabOnCleanExit`, `showProgramStatus`, `discoverEnvironments`, `environmentRefreshSeconds` (`ENVIRONMENT_REFRESH_SECONDS`), `environmentsPanelWidth`; `webglRenderer` defaults to on |
| Command buttons | client/src/command-buttons.ts | partial | `shortcut` kept and de-duplicated; dropping a shortcut that is not a bindable chord needs the chord parser in muxus-app (`keymap::chords`) and is left to the shortcuts feature |
| Highlight profiles + built-ins | client/src/highlight-profiles.ts, builtin-highlight-profiles.ts | partial | regex check injected by the caller; matching belongs to the terminal view |
| Custom terminal schemes | client/src/terminal/custom-schemes.ts | partial | theme conversion left to the terminal view |
| Layout helpers | host-stats.ts, sidebar-width.ts, sftp-panel-width.ts, interface-zoom.ts | done | |
| Window placement files | electron/src/main.ts, window-placement.ts | done | |
| HTTP routes | server/src/routes/* | missing | by design: route logic becomes `muxus_session::Backend` methods over this store |
| Plain-text session log files, recorder, normalizer | session-logging/{session-log-file,session-recorder,terminal-text-normalizer}.ts | missing | owned by muxus-session |

## Data compatibility evidence

- **Schema:** `tests/fixtures/db/ts-fresh-schema.json` and `ts-finalized-schema.json` were written by the
  TypeScript `MuxusDatabase`; a Rust-created file matches them object for object, SQL text included.
- **Reads:** `ts-populated.sqlite3.zst` was populated through the TypeScript API; every read method returns
  the JSON the TypeScript returned (`ts-populated.expected.json`), and re-saving every JSON column writes
  identical text.
- **Vault:** `tests/fixtures/vault/` holds a startup-policy vault and a default-KDF never-policy vault
  created by the TypeScript `PasswordVault`; Rust unlocks both and reads every value. A vault exported by
  this build (`exports_a_vault_for_the_typescript_build`) was opened by the TypeScript build with
  `verify.mts`: every value matched.
- **History:** `tests/fixtures/history/ts-root/` is a TypeScript-written history root; a Rust-written root
  was read by the TypeScript store (see `check-rust-root.mts`).
- **Prefs:** `tests/fixtures/prefs/rehydrate-cases.json` was recorded from the real zustand store.
- The generators are kept beside the fixtures (`generate.mts`, `generate/`); they expect the TypeScript
  checkout at `../muxus` (or `MUXUS_TS_ROOT`) and a scratch `HOME`.

## Known differences and decisions

- serde writes an internally tagged enum's tag first. Stored JSON whose TypeScript key order differs
  (login sequence steps, workspace tabs and nodes) has hand-written serializers, so stored bytes match;
  in-process message types keep serde's order, which no file depends on.
- `serde_json` is built with `preserve_order` (feature unification applies to the whole workspace) so
  arbitrary stored JSON keeps the key order the TypeScript wrote.
- Saved-host rows whose stored profile this build cannot parse are left out of `list_saved_host_profiles`
  instead of failing the whole list.
- A stored prompt-wait timeout or anti-idle interval that is not a whole number is read as unset; the
  TypeScript keeps any number, but only ever writes whole ones.
- The store is synchronous (`Database` is `Send + Sync`, one connection behind a mutex); the vault blocks
  on scrypt and the keyring. Async callers use `spawn_blocking`. The history store is async.
- The OS keyring is chosen like `@napi-rs/keyring` 2.1: Secret Service through the same keyring-rs store
  family (zbus flavour, no libdbus), else kernel keyutils.

## Public API (for other crates)

- `muxus_core::api` — every domain/REST type (hosts, saved hosts, metadata patches, folders, login
  sequences, keyword highlighting, workspaces + `WorkspaceSaveRequest`, tunnels/forwards, vault status,
  session history, diagnostics, SFTP/editor, app/window/update types).
- `muxus_core::protocol` — profiles, `TerminalClientMessage`/`TerminalServerMessage`,
  `DesktopClientMessage`/`DesktopServerMessage`, `ManagementClientMessage`/`ManagementServerMessage`,
  `AuthPromptInfo`, `HostKeyChallenge`, constants, `parse_session_profile`, `parse_terminal_client_message`.
- `muxus_core::validate` — `Validate`, `parse`, `parse_value`, `ValidationError`.
- `muxus_core::{js, stream, management, connection_links, gnmi_path, paste_pacing, paste_prompt,
  anti_idle, input_modes, environments, appimage_env, session_transcript, session_log_file,
  update_version, folder_paths}`.
- `muxus_store::Database` (`open`, `open_in_memory`, every repository method), `muxus_store::db::*` row and
  input types, `DbError`.
- `muxus_store::vault::{PasswordVault, VaultOptions, VaultError, SecretInput, VaultKeyStore,
  SystemVaultKeyStore, MemoryVaultKeyStore, ssh_password_account, folder_password_account,
  desktop_password_account, …}`, `vault::secret_input`.
- `muxus_store::history::{SessionHistoryStore, HistoryOpenOptions, HistoryEvent, PartPolicy,
  SessionHistoryQuery, SessionStatePatch, HistoryError}`.
- `muxus_store::prefs::{PrefsStore, PrefsState, ClientStateFile, PrefsDocument, migrate_prefs_state, …}`.
- `muxus_store::paths::{Environment, AppPaths, server_default_database_path, history_root, …}`,
  `muxus_store::development::seed_development_database`, `muxus_store::ids`.

## Attribution

No third-party code was adapted. Everything here is ported from Muxus itself (MIT); the built-in
highlight profiles and scheme metadata are data from the TypeScript client.
