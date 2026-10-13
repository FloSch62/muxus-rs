# Parity: session (`muxus_session::Backend`)

The in-process replacement for the Node server: `server/src/app.ts`, `server/src/routes/*`,
`server/src/ws/{terminal-socket,terminal-inputs,paste-pacer,sftp-lease-socket}.ts`, the data
folder choice of `electron/src/{main,development-database}.ts`, and the backend-facing parts of
`client/src/connection-recovery.ts`. The transport-side modules (`logging`, `login_sequence`,
`diagnostics`, `stats`) are tracked in `transports.md`.

Run the tests with:

```bash
CARGO_TARGET_DIR=target-session cargo test -p muxus-session
```

Every backend test runs against a sandbox (`BackendOptions::sandbox`): its own data folder and
home, an in-memory vault key store, no SSH agent, local shells with a scratch `HOME`. SSH tests
use the in-process server of `muxus_ssh::testing` (feature `testing`); Telnet tests a loopback
console; nothing touches the real user environment or a real host.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Backend, runtime, threading model | `server/src/app.ts` (`buildApp`), `server.ts` (`startServer`) | done | Own multi-thread runtime (or a caller's `Handle`); every method spawns onto it and returns an executor-agnostic future. Start-up order as `buildApp`: data folder, database, vault `initialize`, X11, connection manager (store-backed lookups), forward manager, history open, `finalize_session_history_separation`, snapshot pruning, tunnel autostart. `shutdown` = `onClose`. |
| Update check off without a feed | `routes/app.ts` (`/api/app/update-check`) | done | `BackendOptions::from_process` reads the manifest from `$MUXUS_UPDATE_FEED/latest.json` only; without the variable the check answers `not-available` with reason `disabled` (About: "No updates are published for this build."), because `UPDATE_MANIFEST_URL` lists the Electron releases. |
| Data folder (release, development, override) | `electron/src/main.ts`, `development-database.ts` | done | `data_dir::DataLocation`: release uses `AppPaths::installed` (shared with the TypeScript build); debug builds and `MUXUS_DEV=1` use `<appData>/Muxus-rs-development`, seeded with `seed_development_database` (SQLite backup API, so a running Electron Muxus's WAL is included) and `client-state.json` (read whole, renamed into place) on every start, history inside it; `MUXUS_DATA_DIR` overrides and never seeds; `MUXUS_HISTORY_PATH` still forces the history root. `tests/development_seed.rs`: an installed backend keeps running while a development backend seeds from it; hosts, folder passwords (vault with an automatic key copied under its `development:` id) and preferences come over. |
| Preferences and other `client-state.json` items | `electron/src/main.ts`, `store-bridge.ts` | done | The backend owns the `PrefsStore` of its data folder: `prefs`, `set_prefs`, `reload_prefs`, `client_state_item`, `set_client_state_item`, `paths`. Window placement files stay with the app (`AppPaths` tells it where). |
| App info, X11 status/settings | `routes/app.ts` | done | |
| Update check (manifest, release URL trust, caching, force) | `routes/app.ts` | done | hyper + rustls (webpki roots) GET with redirects; one check per run shared by every caller, `force` starts a fresh one with `?t=`. Tested against a loopback manifest server. |
| Log viewer (entries, debug switch, clear) | `routes/logs.ts`, `app.ts` `baseLogLevel` | done | `logging::level`: process-wide level with `LOG_LEVEL` as base; the app installs `logging::subscriber::install_app_subscriber` (`main.log` and the viewer; Muxus records at the level, other crates' errors only). SFTP failures, terminal session ends and remote desktop sessions are logged. |
| WSL distributions, serial ports | `routes/local-shells.ts`, `routes/serial.ts` | done | Delegates to `muxus-local` / `muxus-serial` (port sorting tested there). |
| Connection diagnostics | `routes/diagnostics.ts` | done | Dial plan → `DiagnosticHop` (ProxyCommand expanded); 422 for a broken plan. Probes use the backend's environment and home, so a sandbox never asks the real agent. |
| Host stats (local, SSH, shared readings) | `routes/host-stats.ts` | done | Remote reading over `ConnectionCommand` (`RemoteCommand` on a `ManagedConnection` exec channel). |
| Folder settings and folder passwords | `routes/folders.ts` | done | All six routes; vault relabel/delete follow moves and deletes. |
| Forwards, connections | `routes/forwards.ts` | done | Forward list returned as a `Vec` (the `{ forwards }` wrapper is dropped); live updates through `Backend::subscribe_forwards`. |
| Saved tunnels | `routes/tunnels.ts` | done | `tunnel_changed` after save/delete. |
| Saved hosts, host order, metadata schema | `routes/profiles.ts`, `host-order.ts`, `metadata-schema.ts` | done | Schema checks are core's `Validate` impls. |
| Terminal snapshots | `routes/terminal-snapshots.ts` | done | Body-size limit has no in-process meaning; the 512 000-character cap stays. |
| Workspaces | `routes/workspaces.ts` | done | `put_workspace` from the store; `workspace-not-found` / `workspace-locked` codes. |
| Password vault (status, create, repair, unlock, policy, master password, credentials, secrets, send secret, reset) | `routes/password-vault.ts` | done | `sendVaultError` mapping in `ApiError::from(VaultError)`. `send_vault_secret` types into sessions through `TerminalInputs`, never through the recorder. |
| Session history (search, storage, log files, suggest, policies, detail, raw/clean/replay exports, pin, label, delete) | `routes/session-history.ts` | done | Exports return `SessionLogExport { file_name, content_type, body }` instead of HTTP headers. |
| SSH config (list with folder defaults + metadata, upsert/rename, bulk patch, preview, delete, bulk delete, host metadata, keys) | `routes/ssh.ts` | done | `localeCompare` order via the new `muxus_core::collate`. |
| SFTP routes and the detached window lease | `routes/sftp.ts`, `ws/sftp-lease-socket.ts` | done | Thin wrappers over `muxus_ssh::sftp::SftpService` run on the backend runtime; downloads stream through `SftpDownloadStream` (chunks over a channel); `sftp_upload_drop` runs the client's drop orchestration. No backend-level SFTP test (the service is tested in `muxus-ssh`). |
| Local file editor and clipboard images | `routes/local-files.ts` | done | |
| Saved management requests | `routes/management-requests.ts` | missing | By design: the management area owns it. Hook: `muxus_session::management` (storage is `Backend::database()`). |
| Terminal sessions: connect, dial, attach | `ws/terminal-socket.ts` | done | `Backend::open_session(first message) -> SessionHandle`. Message order documented in `muxus_session::terminal`. |
| SSH sessions (prompts over the session, summary with started config forwards, health, transport loss, forwards stop with the last terminal) | `terminal-socket.ts` | done | `ConnectIo` on the session's control channel. Output only flows after `ready`. |
| Local, Telnet, serial sessions | `terminal-socket.ts` (`attachTerminalTransport`) | done | Serial session tested end to end over a pty pair. |
| Dial mode (ready after post-auth password saving, lease handed to forwards) | `terminal-socket.ts` | done | Found and fixed a race in `muxus-ssh` where `wait_for_post_auth` could return before the vault save finished. |
| Transferable sessions (prepare/cancel transfer, attach replay, held output tail, reattach grace, explicit close) | `TransferableTerminalSocket` | done | Dropping a `SessionHandle` is the unexpected renderer close (grace); `close()` ends at once. |
| Output backpressure | `terminal-socket.ts` (`BACKPRESSURE_HIGH`) | done | Unread output per handle + held tail; the transport is paused above 4 MiB until below 2 MiB (50 ms poll, as TS). |
| Paced paste with drain | `ws/paste-pacer.ts`, `drainedWriter` | done | `TransportFeedback::until_written` replaces `drain()`. All of `paste-pacer.test.ts` ported (paused tokio clock). |
| BREAK with its result | `answerBreak`, `channel-break.ts` | partial | Telnet (IAC BRK) and serial report through `TransportFeedback`. SSH answers `ok: false, "Could not send BREAK: the SSH engine cannot send BREAK requests yet"`: russh 0.64 has no `break` channel request (see ssh.md). |
| Terminal inputs registry | `ws/terminal-inputs.ts` | done | |
| Session logging and login sequences per session | `terminal-socket.ts` | done | Recorder from the host's policy; `set-logging`/`set-title`; login sequences (host, then folder) start before `ready`, now reported synchronously like the TypeScript `async` call. |
| Connection recovery decisions | `client/src/connection-recovery.ts` | done | `muxus_session::recovery`; all of `connection-recovery.test.ts` ported. The redial itself is the app's (it reopens with `freshTransport`). |
| Store and SSH adapters | `app.ts` wiring | done | `adapters`: `StoreVault` (`muxus_ssh` `PasswordVault` + `SecretVault`), `StoreHistory` (`HistorySink`), `StoreLoginSequences` (`LoginSequenceSource`), `StoreFolderAuth` (`FolderAuthSource`), `ConnectionCommand` (`RemoteCommand`), `diagnostic_hops`. |
| WebSocket keepalive pings, 30 s connect timeout, bearer token, CSP headers, static files | `app.ts`, `terminal-socket.ts` | n/a | Only meaningful for an HTTP server. |

### Ported tests

`app-routes`, `diagnostics-routes`, `folder-settings` (route half), `host-order-routes`,
`local-files`, `logs-routes`, `password-vault-routes`, `profile-routes`, `session-history-routes`,
`ssh-routes`, `terminal-snapshot-routes`, `tunnel-routes`, `workspace-routes`, `paste-pacer`,
`terminal-socket-break`, `terminal-socket-dial`, `terminal-socket-paste`, `login-sequence-ssh`,
`connection-recovery`; the backend part of `tunnel-reconnect` (a saved tunnel starts with the
backend from a vault password); end-to-end SSH, local, Telnet and serial sessions. `serial-routes`
is covered in `muxus-serial`, `sftp-lease-socket` and the SFTP routes in `muxus-ssh`, the rest of
`connection-session` in `muxus-ssh`. Bearer-token cases have no in-process equivalent.

## Follow-ups made in finished crates

- `muxus-core` (additive): `stream::TransportFeedback` on both stream ends (counted written input
  for drains, `BreakOutcome` per BREAK); `shell_integration` (the zsh/bash shims and
  `SHELL_CWD_REPORT`, byte-identical); `platform::Platform` (with Node's names);
  `collate::locale_compare` (the app's `localeCompare` approximation, shared).
- `muxus-telnet`, `muxus-serial`, `muxus-local`, `muxus-ssh` report drains and BREAK outcomes
  (serial drains with `tcdrain` after each write; `BreakObserver` still works).
- `muxus-local` and `muxus-ssh` use the core shims (the SSH install hash is unchanged);
  `muxus-local::Platform` and the diagnostics `Platform` are the core enum.
- `muxus-ssh`: `testing` feature exposes the test SSH/SFTP servers; `shell::pump` declares its
  feedback before it runs; fixed the post-auth race above.
- The vault account encoders were not moved: the session uses the store's copies, the SSH crate
  keeps its own (identical, tested in both).

## Attribution

No third-party code was adapted. `muxus_core::collate` is the app's own sorting code.
