# SSH parity (`crates/muxus-ssh`)

Port of `server/src/ssh`, `server/src/forwards`, `server/src/x11` and the SFTP routes. The crate
depends on `muxus-core` only; settings come in as arguments (`ManagerOptions`, `LocalEnv`) and every
user interaction goes through the async traits `ConnectIo` (status, auth prompts, host keys),
`PasswordVault` and `SecurityKeyAsk`.

Tests run against an in-process russh server (`src/testing/server.rs`, 127.0.0.1:0) and an
in-process ssh-agent; nothing touches the real `~/.ssh`, the real agent or a real host.

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| ssh_config parse, Include, first-obtained-wins resolution, tokens, listing | `ssh/ssh-config.ts` | done | Match blocks skipped and Include inside Host kept verbatim, as in the TS. Indexed resolution (4000 hosts < 1 s). All of `ssh-config.test.ts` ported. |
| Line-preserving Host block editor (upsert, bulk patch, delete, preview, atomic write, `.muxus.bak`, symlinks) | `ssh/ssh-config-edit.ts` | done | All of `ssh-config-edit.test.ts` ported. Errors carry the TS HTTP status (`Problem`). |
| Folder credentials as the lowest-priority config layer | `ssh/folder-auth.ts` | done | `folder-auth.test.ts` and the resolver case of `folder-settings.test.ts` ported. Vault account/label encoding matches `password-vault.ts` byte for byte. |
| Folder path helpers | `util/folder-paths.ts` | done | Duplicate of `muxus_core::folder_paths`; can switch to the core copy. |
| Server config (`resolveConfig`) | `config.ts` | n/a | HTTP server binding/token; the native app has no server. |
| Folder settings persistence and routes | `persistence/database.ts`, `routes/folders.ts` | n/a | Store/session layer. |
| known_hosts (plain, hashed, wildcard, `@revoked`, `[host]:port`, `.old` backup, `/dev/null`) | `ssh/known-hosts.ts` | done | `known-hosts.test.ts` ported. |
| Key picker scan, agent socket resolution | `ssh/key-scan.ts` | done | `agent-socket.test.ts` ported; PuTTY keys parse through russh. |
| OpenSSH user certificates | `ssh/certificates.ts` | done | Signing goes through the SSH engine; RSA certificates offer rsa-sha2-512/256/ssh-rsa variants. |
| Algorithm lists (`+`/`^`/`-`, patterns, unsupported-entry notes, Compression) | `ssh/algorithms.ts` | done | Defaults mirror the TS engine (ECDH-NIST, SHA-1 MACs, ssh-rsa kept); diffie-hellman-group1-sha1, group14-sha1, gex-sha1, ssh-dss, CBC/3DES, hmac-sha1 available on request, verified end to end against restricted servers. `^` moves an offered algorithm to the head (OpenSSH semantics). mlkem768x25519 added to the defaults. |
| Security key helpers | `ssh/security-keys.ts` | done | |
| Security keys held by the user's ssh-agent (listing, sk certificates, touch beyond ConnectTimeout) | `security-keys.test.ts` agent cases | partial | Implemented through the agent path; the agent-held sk tests need an agent that signs with FIDO keys and are not ported. |
| Private ssh-agent for security key IdentityFiles | `ssh/security-key-agent.ts` | done | Askpass relays over a socketpair on the helper's stdin instead of a Node script. Tested with fake and (when installed) real OpenSSH tools. |
| Agent wrapper that never stalls the dial | `ssh/responsive-agent.ts` | done | Deadline pause/resume, wait notice, timeouts, unavailable latch. |
| RFC 4335 `break` | `ssh/channel-break.ts` | missing | russh 0.64 has no client API for the `break` channel request (and no raw channel request). Needs an upstream addition or a patched russh; `TransportInput::Break` is answered with `BreakOutcome::Failed` meanwhile, so the UI shows the failure. |
| Dial plan (ProxyJump chains, saved Muxus jump hosts, profile overlay, cycles) | `ssh/connection-manager.ts` (`buildChain`, `muxKey`, ...) | done | `chain.test.ts` and the buildChain cases of `folder-auth.test.ts` ported. |
| Auth ladder (none, agent, certificates, identity files with passphrases, security keys, keyboard-interactive, password with retries, saved/folder passwords, vault unlock/create/remember) | `ssh/connection-manager.ts` (`AuthLadder`) | done | `agent-auth`, `folder-password-auth` and the password-memory cases of `connection-session` ported. A failed agent signature answers with an unusable signature so the login moves on (the engine waits for a signature once a key is accepted). A keyboard-interactive round without prompts (PAM's after a password) is answered without asking. Forward start, stop and failure are logged. |
| Host key verification (pinned link fingerprints, known_hosts, StrictHostKeyChecking, TOFU prompt) | `ssh/connection-manager.ts` (`verifyHostKey`) | done | `host-key-pin` behaviour ported (fingerprint plumbing and connection cases). |
| Pausable ConnectTimeout, keepalive (ServerAlive*), passive health | `ssh/connection-manager.ts` | done | |
| ProxyCommand transport | `ssh/connection-manager.ts` (`openProxyCommand`) | done | |
| Connection sharing, concurrent-dial collapse, MaxSessions overflow, force reconnect, console mode, automatic plain-console redial, SFTP-disabled hosts | `ssh/connection-manager.ts` | done | `connection-mux.test.ts` ported. |
| Shared jump hops | `ssh/jump-hops.ts` | done | `jump-hops.test.ts` ported. |
| Lease registry | `ssh/connection-leases.ts` | done | Leases also release on drop. |
| Session summary | `ssh/connection-manager.ts` (`opened`) | done | `forwards` is left empty for the caller, as in the TS. |
| Remote shell integration (probe, content-addressed install, zsh/bash shims, cwd restore, transport-loss signal) | `ssh/remote-shell-integration.ts` | done | Script bytes generated from the TS sources; the content hash equals the TS build's (`5fd060055802`), so both apps share remote installs. |
| Interactive session (pty with VERASE, env, RemoteCommand/RequestTTY, x11-req, agent forwarding) as `TransportChannels` | `ssh/connection-manager.ts`, `ws/terminal-socket.ts` | done | `TerminalShell::into_stream`. |
| X11 forwarding (display detection, Xauthority, setup rewrite, local server, bundled VcXsrv) | `x11/*` | done | VcXsrv compiled on Windows (and in tests). `x11.test.ts` and `x11-forwarding.test.ts` ported, including x11-req and a server-opened X11 channel relayed to a fake local X server with the real cookie. An X11 channel is accepted before the local server is reached (the TS refused it when the local server was down). |
| Home, list (realpath, `.`/`..` filtered, type, size, mtime, mode bits, owner/group from long names) | `routes/sftp.ts` (`/home`, `/list`) | done | DTOs are `muxus_core::api` (`SftpEntry`, `SftpListResponse`), re-exported from `sftp::types`. |
| Drag-out download tickets (2 min, per connection, mismatch burns the ticket) | `routes/sftp.ts` (`/list`, `/drag-download`) | done | |
| Streaming download that keeps its lease until read through or dropped | `routes/sftp.ts` (`/download`) | done | `SftpDownload`: `AsyncRead`, `next_chunk`, `copy_to` with `ByteProgress`; pipelined 32 KiB reads. `name`/`size`/`mtime_ms` replace the HTTP headers. |
| Remote editor read (dir 400, symlink 409, 8 MiB 413, binary/UTF-8 415, BOM dropped) | `routes/sftp.ts` (`GET /file`) | done | |
| Remote editor save (mtime check, force, atomic `.tmp` + posix-rename, mode kept) | `routes/sftp.ts` (`PUT /file`) | done | |
| Upload (conflict 409 codes, expectedMtimeMs, symlink/dir refusal, staged `.upload`, posix-rename, v3 rename fallback, cleanup on failure) | `routes/sftp.ts` (`POST /upload`) | done | Also removes the staged file when the upload future is dropped. Progress through `UploadStep` callbacks. |
| Clipboard image to `/tmp/muxus-paste-*.png`, mode 600 | `routes/sftp.ts` (`POST /clipboard-image`) | done | |
| mkdir (plain, recursive), rename, delete (recursive, 10 000-entry guard, links not followed) | `routes/sftp.ts` (`/mkdir`, `/rename`, `/delete`) | done | |
| Window lease (hold while open, release on close or when the connection closes, stale id 404) | `ws/sftp-lease-socket.ts` | done | `SftpWindowLease` guard; `closed()` replaces the 1011 socket close. |
| Upload a drop: folders shallowest first, replace confirmation, cancel, progress phases, summary | `client/src/sftp-upload.ts` | done | `upload::upload_to_remote` over the `UploadDeps` trait; `SftpUploadDeps` runs it on `SftpService`. `collect_drop` walks local paths instead of browser entries. |
| Progress smoothing/throttling | `client/src/api/transfers.ts` (`progressReporter`) | done | `ProgressMeter`. |
| Session reopen after the SFTP channel ended | `connection-manager.ts` (`sftp.once('close')`) | done | Dead sessions are dropped on the error that reveals them; an operation whose first request finds the session gone is retried on a fresh one. |
| Terminal upload store (one upload per connection, toasts, browser counts) | `client/src/terminal-upload.ts`, `state/sftp-uploads.ts` | missing | UI state; belongs to the app layer. |
| chmod, directory download | — | n/a | The TypeScript app has neither. |
| Port forwards (local, remote, dynamic SOCKS5, config forwards, saved tunnels with autostart and reconnect backoff, status) | `forwards/forward-manager.ts`, `routes/forwards.ts`, `routes/tunnels.ts` | done | `forward-manager.test.ts` (21) and `tunnel-reconnect.test.ts` (3) ported plus traffic tests per forward type. Status through `ForwardManager::subscribe()`. |

## Attribution

No third-party code was copied. russh, russh-sftp and ssh-key are dependencies (Apache-2.0).

SFTP: `ManagedConnection::sftp()` hands out the raw russh-sftp session (`SftpChannel`), because the
high-level client cannot send `posix-rename@openssh.com` and drops directory long names; its
request timeout is 60 s so pipelined transfers on slow links do not time out. The SFTP tests run
against `src/testing/sftp_server.rs`, a russh-sftp server over a scratch directory.
