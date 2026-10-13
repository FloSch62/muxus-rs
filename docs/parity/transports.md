# Parity: transports

Local shells, Telnet, serial, and the session-level helpers that sit on top of
every terminal transport: session logging, login sequences, connection
diagnostics and host statistics.

Crates: `muxus-local`, `muxus-telnet`, `muxus-serial`, and the `logging`,
`login_sequence`, `diagnostics` and `stats` modules of `muxus-session`.

All terminal transports speak `muxus_core::stream` (`TransportChannels`) and take
the profile types of `muxus_core::protocol` (`LocalProfile`, `TelnetProfile`,
`SerialProfile`). Tests: `cargo test -p muxus-local -p muxus-telnet -p muxus-serial -p muxus-session`
(196 tests).

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Local pty session (spawn, env cleanup, TERM/COLORTERM/TERM_PROGRAM, resize, close, exit code) | `server/src/local/pty-manager.ts`, local branch of `server/src/ws/terminal-socket.ts` | done | `portable-pty`; live bash and zsh tests in a scratch HOME without ZDOTDIR. Runs on threads, no runtime needed. Windows (ConPTY) compiles through `portable-pty` but is untested. |
| Startup command after the first prompt | `pty-manager.ts` (`localStartupInput`, `localShellPromptReady`), `terminal-socket.ts` | done | Prompt detection on the last 8192 characters, as in TS. |
| Default shell | `pty-manager.ts` (`defaultShell`) | done | |
| Shell integration shims (zsh `.zshenv`/`.zshrc`, bash init file, cmd `PROMPT`) | `server/src/local/shell-integration.ts` | done | Byte-identical to the TS output (golden files generated from the TS). fish emits OSC 133 itself, as in TS; there is no local pwsh shim in TS either. |
| Login shell environment (PATH, macOS SSH_AUTH_SOCK) | `electron/src/login-shell-environment.ts`, `shell-env` package | done | `shell_env` adds a 10 s timeout the npm package lacks. The app applies the result to its own environment at startup. |
| Saved shell profiles, argument editing, WSL launch profiles | `client/src/local-shell-profile.ts`, `client/src/local-shell-launchers.ts` | done | `LocalShellProfileConfig` lives in `muxus-local` until the prefs port has it. The React hooks are UI work. |
| WSL distribution discovery | `server/src/local/wsl.ts`, `routes/local-shells.ts` | partial | Registry parsing, icons and listing logic done and tested; the `reg.exe` query is `cfg(windows)` and untested (no Windows here). |
| Telnet codec (negotiation, TTYPE, NAWS, NVT newlines, CR NUL, IAC escaping) | `server/src/telnet/telnet-transport.ts` | done | |
| Telnet transport (connect timeout, friendly errors, keepalive, BREAK as IAC BRK, close) | `telnet-transport.ts` | done | Drained-input counts and the BREAK outcome are reported through `muxus_core::stream::TransportFeedback`. Input typed before a close is still flushed (2 s cap). |
| Serial open (baud, data bits, parity, stop bits, flow control, exclusive lock, busy retry, friendly errors) | `server/src/serial/serial-transport.ts` | partial | `serialport` crate. Mark/space parity and 1.5 stop bits are refused on every platform; node-serialport refuses them on Unix too but supports them on Windows. Uses TIOCEXCL plus flock (TS: flock only). |
| Serial BREAK with configurable duration | `serial-transport.ts` | done | Held for the profile's `breakDurationMs`, else the requested duration; second press ignored; released before close; DTR/RTS asserted like node-serialport's `set()`. Outcome reported through `TransportFeedback` (`BreakOutcome`). |
| Serial data path and device loss | `serial-transport.ts` | done | Tested over a pty pair. Drained input is reported through `TransportFeedback` so paced paste can wait. |
| Serial port listing | `server/src/routes/serial.ts`, bindings-cpp `linux-list.js` | partial | Linux: same `udevadm info -e` parsing as node-serialport. macOS/Windows: `serialport` enumeration supplies manufacturer, serial number and USB ids only (no `pnpId`/`locationId`). |
| Text normalizer | `server/src/session-logging/terminal-text-normalizer.ts` | done | Line offsets in UTF-16 units, as stored by TS. CSI parameters clamped to 65535 (guards a runaway allocation TS would attempt). |
| Session log files (paths, numbering, append, timestamps, failure handling) | `server/src/session-logging/session-log-file.ts` | done | Writer thread flushes within 250 ms; 16 MB backlog limit. |
| Session recorder | `server/src/session-logging/session-recorder.ts` | done | History goes through the `HistorySink` trait; `MemoryHistory` implements it for tests. 250 ms batch coalescing with a tokio timer when a runtime is present. |
| History store | `server/src/session-logging/history-store.ts`, `history-worker.js` | missing | Belongs to `muxus-store`; it must implement `muxus_session::logging::HistorySink`. |
| App log buffer | `server/src/logging/log-buffer.ts` | done | Plus `AppLogLayer`, a `tracing` layer feeding the same buffer. |
| Login sequence output matcher | `server/src/login-sequence/output-matcher.ts` | done | `fancy-regex` with a backtrack limit replaces the 100 ms vm timeout. Patterns that need no backtracking run on a linear-time engine and never time out. |
| Login sequence runner | `server/src/login-sequence/runner.ts` | done | |
| Login sequence session (progress, cancel, master-password prompt) | `server/src/login-sequence/session.ts`, `security/vault-secret-input.ts` | done | Vault via the `SecretVault` trait. Tested end to end over Telnet against a fake console, with recorder and log file. Reattach and "announce before ready" are session-layer wiring. |
| Login sequence resolution (host, folder chain, "none") | `server/src/login-sequence/resolve.ts` | done | Via the `LoginSequenceSource` trait. |
| Login sequence editor helpers | `client/src/login-sequence.ts` | done | In `muxus_session::login_sequence::editor` so the UI and the backend share them. |
| Connection diagnostics (DNS, ping, TCP, SSH greeting, agent, key files, conclusions) | `server/src/diagnostics/connection-diagnostics.ts` | done | SSH plans arrive as `DiagnosticHop`s; the SSH layer maps its dial plan to them. Windows agent pipe probe untested. |
| Host stats script and parser | `server/src/stats/host-stats.ts` | done | Script byte-identical to TS (golden). |
| Remote host stats over an exec channel | `host-stats.ts` | done | Via the `RemoteCommand` trait (stdout as `AsyncRead`); 5 s and 256 KB limits. |
| Local host stats | `host-stats.ts` (`readLocalHostStats`) | partial | Linux/BSD done (live test). macOS lacks the CPU-time fallback TS takes from `os.cpus()`; Windows (Node `os` module path) is missing. |
| Shared readings per connection | `server/src/routes/host-stats.ts` | done | `SharedReadings`; the route itself becomes a `Backend` method. |
| Status bar rates and formatting | `client/src/host-stats.ts` | done | `js_to_fixed` keeps JavaScript's round-half-up. |

## Needs from other crates

All resolved:

- `muxus_core::stream::TransportFeedback` carries BREAK outcomes and drained input.
- The shell integration scripts live in `muxus_core::shell_integration`, and the shared
  `Platform` enum in `muxus_core::platform` replaced the copies in `muxus-local` and
  `muxus_session::diagnostics`.
- `muxus-session` implements `HistorySink`, `SecretVault` and `LoginSequenceSource` over
  the store, and `RemoteCommand` plus the dial plan to `DiagnosticHop` mapping over
  `muxus-ssh`.

## Attribution

- `muxus-local::login_env::strip_ansi`: regular expression adapted from
  [ansi-regex](https://github.com/chalk/ansi-regex) 6 (MIT, Sindre Sorhus).
- `muxus-local::login_env::shell_env`: behaviour of
  [shell-env](https://github.com/sindresorhus/shell-env) 4 (MIT, Sindre Sorhus).
- `muxus-serial::ports::parse_udevadm_ports`: port of `linux-list.js` from
  [@serialport/bindings-cpp](https://github.com/serialport/bindings-cpp)
  (MIT, Copyright 2010 Christopher Williams).
