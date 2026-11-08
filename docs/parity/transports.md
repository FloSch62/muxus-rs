# Parity: transports

Local shells, Telnet, serial, and the session-level helpers that sit on top of
every terminal transport: session logging, login sequences, connection
diagnostics and host statistics.

Crates: `muxus-local`, `muxus-telnet`, `muxus-serial`, the `machine` module of `muxus-os`,
and the `logging`, `login_sequence`, `diagnostics` and `stats` modules of `muxus-session`.

All terminal transports speak `muxus_core::stream` (`TransportChannels`) and take
the profile types of `muxus_core::protocol` (`LocalProfile`, `TelnetProfile`,
`SerialProfile`). Tests: `cargo test -p muxus-local -p muxus-telnet -p muxus-serial -p muxus-os -p muxus-session`.

Windows and macOS: `hack/cross-check.sh` runs clippy (`-D warnings`, all targets) for
`x86_64-pc-windows-gnu` and `x86_64-apple-darwin` on `muxus-local`, `muxus-telnet`,
`muxus-serial` and `muxus-os`, and on `muxus-session`'s `stats` and `diagnostics` modules
through `hack/cross-check`. That proves the platform code compiles; none of it has run on Windows or
macOS.

## Status

| Feature | TypeScript source | State | Notes |
|---|---|---|---|
| Local pty session (spawn, env cleanup, TERM/COLORTERM/TERM_PROGRAM, resize, close, exit code) | `server/src/local/pty-manager.ts`, local branch of `server/src/ws/terminal-socket.ts` | done | `portable-pty`; live bash and zsh tests in a scratch HOME without ZDOTDIR (Unix only). Runs on threads, no runtime needed. Windows (ConPTY through `portable-pty`) is clippy-clean for `x86_64-pc-windows-gnu`, tests included, but has not run. |
| Startup command after the first prompt | `pty-manager.ts` (`localStartupInput`, `localShellPromptReady`), `terminal-socket.ts` | done | Prompt detection on the last 8192 characters, as in TS. |
| Default shell | `pty-manager.ts` (`defaultShell`) | done | |
| Shell integration shims (zsh `.zshenv`/`.zshrc`, bash init file, cmd `PROMPT`) | `server/src/local/shell-integration.ts` | done | Byte-identical to the TS output (golden files generated from the TS). fish emits OSC 133 itself, as in TS; there is no local pwsh shim in TS either. |
| Login shell environment (PATH, macOS SSH_AUTH_SOCK) | `electron/src/login-shell-environment.ts`, `shell-env` package | done | `shell_env` adds a 10 s timeout the npm package lacks. The app applies the result to its own environment at startup. |
| Saved shell profiles, argument editing, WSL launch profiles | `client/src/local-shell-profile.ts`, `client/src/local-shell-launchers.ts` | done | `LocalShellProfileConfig` lives in `muxus-local` until the prefs port has it. The React hooks are UI work. |
| WSL distribution discovery | `server/src/local/wsl.ts`, `routes/local-shells.ts` | partial | Registry parsing, icons and listing logic done and tested; the `reg.exe` query is `cfg(windows)`: clippy-clean for `x86_64-pc-windows-gnu`, never run. |
| Telnet codec (negotiation, TTYPE, NAWS, NVT newlines, CR NUL, IAC escaping) | `server/src/telnet/telnet-transport.ts` | done | |
| Telnet transport (connect timeout, friendly errors, keepalive, BREAK as IAC BRK, close) | `telnet-transport.ts` | done | Drained-input counts and the BREAK outcome are reported through `muxus_core::stream::TransportFeedback`. Input typed before a close is still flushed (2 s cap). |
| Serial open (baud, data bits, parity, stop bits, flow control, exclusive lock, busy retry, friendly errors) | `server/src/serial/serial-transport.ts` | done | `serialport` crate. Mark/space parity and 1.5 stop bits, which it lacks, are written into the port's DCB on Windows (`GetCommState`/`SetCommState`, as node-serialport's Windows binding does) and refused elsewhere, as node-serialport refuses them on Unix. The Windows path is checked with clippy for `x86_64-pc-windows-gnu` but not run on Windows. Uses TIOCEXCL plus flock (TS: flock only). |
| Serial BREAK with configurable duration | `serial-transport.ts` | done | Held for the profile's `breakDurationMs`, else the requested duration; second press ignored; released before close; DTR/RTS asserted like node-serialport's `set()`. Outcome reported through `TransportFeedback` (`BreakOutcome`). |
| Serial data path and device loss | `serial-transport.ts` | done | Tested over a pty pair. Drained input is reported through `TransportFeedback` so paced paste can wait. |
| Serial port listing | `server/src/routes/serial.ts`, bindings-cpp `linux-list.js`, `serialport_win.cpp`, `win32.js`, `win32-sn-parser.js`, `darwin_list.cpp` | done (Linux), partial (Windows, macOS: implemented, never run) | Linux: same `udevadm info -e` parsing as node-serialport. Windows: the Ports class through SetupAPI as `serialport_win.cpp` lists it (`ports/windows.rs`): `PortName` containing COM, `pnpId` = device instance ID, `locationId` = `SPDRP_LOCATION_INFORMATION`, manufacturer = `SPDRP_MFG`, VID/PID cut from the instance ID (upper case, as there), the serial number from the USB container (`DEVPKEY_Device_ContainerId` matched against `Enum\USB\VID_x&PID_y\*\ContainerID`, case-sensitive like its `wcscmp`) else `win32-sn-parser.js`, whose two regular expressions are ported and checked against 17 values printed by the real parser under Node. macOS: the `serialport` crate's IOKit enumeration, keeping only the dial-in `/dev/tty.*` device `darwin_list.cpp` reports (the crate also lists `/dev/cu.*`), `locationId` = the USB `locationID` as `%08x` (rebuilt from the crate's `usbportinfo-location` bus and port chain); manufacturer and serial number are the crate's, which reads them from the `IOUSBHostInterface` parent where `darwin_list.cpp` reads the `IOUSBDevice`. The pure parts are unit-tested on Linux; the SetupAPI and IOKit calls are clippy-clean for both targets and have not run. |
| Text normalizer | `server/src/session-logging/terminal-text-normalizer.ts` | done | Line offsets in UTF-16 units, as stored by TS. CSI parameters clamped to 65535 (guards a runaway allocation TS would attempt). |
| Session log files (paths, numbering, append, timestamps, failure handling) | `server/src/session-logging/session-log-file.ts` | done | Writer thread flushes within 250 ms; 16 MB backlog limit. |
| Session recorder | `server/src/session-logging/session-recorder.ts` | done | History goes through the `HistorySink` trait; `MemoryHistory` implements it for tests. 250 ms batch coalescing with a tokio timer when a runtime is present. |
| History store | `server/src/session-logging/history-store.ts`, `history-worker.js` | done | `muxus_store::history` (tracked in `core-store.md`); `muxus_session::adapters::StoreHistory` implements `HistorySink` over it. |
| App log buffer | `server/src/logging/log-buffer.ts` | done | Plus `AppLogLayer`, a `tracing` layer feeding the same buffer. |
| Login sequence output matcher | `server/src/login-sequence/output-matcher.ts` | done | `fancy-regex` with a backtrack limit replaces the 100 ms vm timeout. Patterns that need no backtracking run on a linear-time engine and never time out. |
| Login sequence runner | `server/src/login-sequence/runner.ts` | done | |
| Login sequence session (progress, cancel, master-password prompt) | `server/src/login-sequence/session.ts`, `security/vault-secret-input.ts` | done | Vault via the `SecretVault` trait. Tested end to end over Telnet against a fake console, with recorder and log file. Reattach and "announce before ready" are session-layer wiring. |
| Login sequence resolution (host, folder chain, "none") | `server/src/login-sequence/resolve.ts` | done | Via the `LoginSequenceSource` trait. |
| Login sequence editor helpers | `client/src/login-sequence.ts` | done | In `muxus_session::login_sequence::editor` so the UI and the backend share them. |
| Connection diagnostics (DNS, ping, TCP, SSH greeting, agent, key files, conclusions) | `server/src/diagnostics/connection-diagnostics.ts` | done | SSH plans arrive as `DiagnosticHop`s; the SSH layer maps its dial plan to them. The Windows ping and agent pipe probes are clippy-clean for `x86_64-pc-windows-gnu` (`hack/cross-check.sh` compiles `diagnostics`), never run. |
| Host stats script and parser | `server/src/stats/host-stats.ts` | done | Script byte-identical to TS (golden). |
| Remote host stats over an exec channel | `host-stats.ts` | done | Via the `RemoteCommand` trait (stdout as `AsyncRead`); 5 s and 256 KB limits. |
| Local host stats | `host-stats.ts` (`readLocalHostStats`, `readWindowsHostStats`, `nodeCpuTime`) | done (Linux), partial (Windows, macOS: implemented, never run) | Linux/BSD: the script (live test). macOS: the CPU-time fallback from `os.cpus()` is `muxus_os::machine::core_times` (`host_processor_info` ticks × 1000/`_SC_CLK_TCK` ms, as libuv). Windows has no `sh`: `muxus_os::machine::windows_host_stats` makes the calls libuv makes for Node's `os` module (DNS host name, `RtlGetVersion` + registry `ProductName` with the Windows 11 rename and service pack, `Windows_NT <release>`, `GetTickCount64`, `GlobalMemoryStatusEx`, per-processor `NtQuerySystemInformation` times, `GetDiskFreeSpaceW` on `%SystemDrive%\`). Conversions and the sample are unit-tested on Linux; the system calls are clippy-clean for both targets and have not run. BSD gets no CPU-time fallback (Node's `kern.cp_times`), as before. |
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
  (MIT, Copyright 2010 Christopher Williams). `ports::windows` (`ListBaton::Execute`,
  `getSerialNumber` of `serialport_win.cpp`), `ports::win32_serial_number`
  (`win32-sn-parser.js`) and the macOS dial-in and `locationId` rules (`darwin_list.cpp`)
  come from the same package.
- `muxus-os::machine`: the system calls and conversions of libuv's `uv_cpu_info`,
  `uv_os_uname`, `uv_uptime` and `uv_fs_statfs` (MIT, Copyright Joyent, Inc. and other Node
  contributors), reimplemented, not copied.
