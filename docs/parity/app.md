# Parity: app (`muxus-app`: the window, its sessions and the feature screens in it)

The main window with live sessions: the startup sequence, the data folder, a
`muxus_term_view::TerminalView` per terminal tab and pane, remote desktop and
management views for the other tabs, how a session ends, reconnects and is
diagnosed, the prompts sessions raise, the status bar, and how every feature
screen (`features/*`) is mounted, opened and answered. The window chrome (top
bar, sidebar, tab strip, panes) is tracked in `ui-shell.md`; each feature's
own screens in its parity file.

```bash
CARGO_TARGET_DIR=target cargo run -p muxus-app                        # the app (debug: development data folder)
MUXUS_SCENE=overview CARGO_TARGET_DIR=target cargo run -p muxus-app   # docs demo catalog, no backend
hack/app-shots.sh                                     # screenshot set against the docs screenshots
hack/regress.py capture <dir> --bin <examples> --app <muxus>   # every scene of every feature, light and dark
hack/regress.py compare <before> <after>              # share of differing pixels per scene
hack/perf.sh --binary target-release/release/muxus    # throughput, frame pacing, UI blocking, memory
hack/e2e/run.py all --binary target-release/release/muxus   # the real app against real servers (below)
```

## Running on the connections of an installed Muxus

The Rust build reads the Electron build's data: its database
(`$XDG_CONFIG_HOME/Muxus/muxus.sqlite3`), its preferences
(`client-state.json`), `~/.ssh/config` and `known_hosts`, and the vault key
in the OS keyring (service `io.github.flosch62.muxus.password-vault`, the
same entries `@napi-rs/keyring` writes; both use the keyring-core Secret
Service store). To try it without touching the installed data, run it on the
**development data folder**, `$XDG_CONFIG_HOME/Muxus-rs-development`:

```bash
cd muxus-rs
CARGO_TARGET_DIR=target-release cargo build --release -p muxus-app
MUXUS_DEV=1 target-release/release/muxus          # release build on the development folder
# or simply the debug build, which always uses the development folder:
CARGO_TARGET_DIR=target-ui-b cargo run -p muxus-app
```

On every start the development folder is refreshed from the installed one,
the way the Electron development build refreshes `Muxus-development`
(`electron/src/development-database.ts`):

- the database is copied with SQLite's online backup API from a read-only
  connection, so committed transactions still in the installed app's WAL are
  included and a running Electron Muxus is never disturbed; the copy is made
  in a temporary folder and renamed into place;
- session history storage is pointed back into the development folder, the
  vault gets a `development:<sha256>` id, and an automatic ("never prompt")
  vault's key is copied in the keyring under that id (a master-password vault
  just asks for its password, "Unlock password vault" at startup);
- `client-state.json` (folder order, colors, collapsed folders, every
  preference) is copied as a whole file and renamed into place.

Changes made in the Rust build stay in the development folder and are
replaced by the installed data on the next start, as with the Electron
development build. A release build *without* `MUXUS_DEV=1` uses the
installed folder itself, shared with the Electron build. `MUXUS_DATA_DIR=<dir>`
puts the data anywhere and never seeds (tests and sandboxes).

Checked with `examples/installed_fixture.rs` (an installed folder with the
documentation's demo hosts, a vault and preferences under a scratch HOME,
written by the backend) and the app started headless on it: the sidebar lists
the hosts with their folders and colors, the vault asks for its password.
`muxus-session/tests/development_seed.rs` seeds while the installed backend
keeps running (WAL included, automatic vault key copied, preferences copied).

## Startup (`ui::run`, `electron/src/main.ts`)

1. The data folder is chosen (`DataLocation::from_process`) and created
   (0700); the shell's log starts in `<data>/logs/main.log` with panic
   capture. One tracing subscriber
   (`muxus_session::logging::subscriber::install_app_subscriber`) feeds the
   file and the log viewer: Muxus records at the configured level, other
   crates' errors only. The first line names the version and platform;
   connects, session ends, tunnel starts and failures, SFTP failures and
   remote desktop sessions are logged.
2. One instance per data folder (`single_instance::acquire`): a second
   launch hands its command line to the running one and exits before it
   could seed or open the database. Later launches arrive as desktop events:
   the window comes forward and `launch::plan_launch` resolves the request
   (connect target, link, host, folder, workspace) against the backend's
   catalogs. The launch this process started with runs once the window is up
   (a workspace launch becomes the window's initial workspace).
3. The backend starts (seeding the development folder first, see above).
4. `SettingsPrefs::init`: one preferences entity for every window; each
   window subscribes to `PrefsChanged` and applies theme, fonts, terminal
   schemes, sidebar and status bar changes live. The window's own changes go
   through it (one writer, newest state wins).
5. App-wide feature services: toasts, SFTP services and its question host,
   remote desktop key bindings, settings hooks (log viewer, session import),
   the vault's "New secret…" dialog, command hooks.
6. **Desktop updates are off.** The release feed still names the Electron
   releases, so a Rust build must neither offer nor install them: the in-app
   updater starts only when `MUXUS_UPDATE_FEED=<folder URL>` is set, and the
   plain update check then reads `<folder>/latest.json`. Without the variable
   About says "No updates are published for this build." and the update
   notice never shows.
7. The main window opens (app id `muxus`); further windows (a host or tab in
   a new window, a workspace in a new window) open on the same backend.

## Status

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Startup sequence, single instance, launch hand-over | `electron/src/main.ts`, `CommandLineLaunchHandler.tsx` | done | see above; `ui/desktop.rs` performs `PlannedAction`s |
| Development data folder seeded from the installed app | `development-database.ts` | done | database by backup API, `client-state.json`, vault id remap and key copy |
| Updater off by default | `updater.ts` | done | only with `MUXUS_UPDATE_FEED`; neutral "no updates" otherwise |
| Data source | `api/*.ts` | done | `data_source::DataSource`: `BackendSource`, `DemoSource`, `LocalSource` |
| Terminal per tab and pane | `components/TerminalViewImpl.tsx` | done | |
| Session link and output pump | `ws/terminal-socket.ts`, xterm.js write queue | done | streaming output is gathered over an event-loop turn or two, so a `seq` flood is ~1 k updates/s instead of ~35 k; 16 KiB pieces, the UI thread yields every 12 ms |
| Remote desktop tabs | `RemoteDesktopView.tsx` | done | `ui/session_views.rs`: one `RemoteDesktopView` per tab; status, prompts, toasts, reconnects, titles, focus |
| Management tabs (gNMI, NETCONF) | `ManagementView.tsx` | done | same, `ManagementView` |
| Shared auth, host-key and certificate dialogs | `AuthPromptDialog`, `HostKeyDialog`, `DesktopCertificateDialog` | done | one queue per window for terminals, desktops, management sessions and tunnel dials |
| Session end, reconnect, auto-reconnect, tmux/screen reattach | `connection-recovery.ts` | done | |
| Connection diagnostics (D) | `terminal/connection-diagnostics.ts` | done | `session::diagnostics` (the two copies merged) |
| Status bar host statistics | `StatusBar.tsx`, `api/host-stats.ts` | done | pauses while no frame of the window is shown, resumes when it shows again |
| Host editor, bulk edit, organize, folder dialog, launch group | `HostEditorDialog.tsx`, `SessionSidebar.tsx` | done | `ui/host_actions.rs`: every sidebar menu item; deletes behind the client's questions; order writes; Add host card and button; "Save & connect" |
| Sidebar drag and drop | `useTreeDnd.ts` | done | `ui/sidebar_dnd.rs` on `sidebar::tree_dnd`; hosts into folders and between siblings, folders into and beside folders |
| Host sessions menu | `HostSessionsMenu.tsx` | done | anchored at the row's edge (sidebar side aware); status dot, "Current tab"/"Tab N", details, the last two lines of output, "New session" with the middle-click hint |
| Terminal scrollback kept across restarts | `TerminalViewImpl.tsx` (`persistSnapshot`) | done | output marks the tab dirty, a snapshot is stored after 1.5 s of quiet and every 10 s; closing the window flushes; a restored tab replays it before it connects |
| "New secret…" in host fields | `VaultSecretField.tsx` | done | opens the vault's secret dialog; the field takes the new secret |
| Settings window, shortcuts editor | `SettingsWindow.tsx`, `ShortcutsDialog.tsx` | done | top bar, `app.settings`, `app.shortcuts`, local shell settings; the dispatcher stands down while a chord is recorded |
| Quick launcher, host picker, quick connect | `QuickLauncherDialog.tsx`, `HostPickerPopover.tsx` | done | every result kind and action, tunnels started with the shared prompts |
| Workspaces (restore, autosave, dialog, new windows) | `workspace-persistence.ts`, `WorkspaceDialog.tsx` | done | the window implements `WorkspaceLayoutHost` through a layout mirror (the runtime never reads the shell mid-update); one `WorkspaceBus`; restore waits for the vault's startup unlock; groups restored with `MultiExec::set_groups` |
| Command buttons, action bar, command menu, multi-execution | `ActionBar.tsx`, `CommandButtonMenu.tsx`, `MultiExecControl.tsx` | done | command hooks route typing, focus, secrets and the caret to the window the command ran in; typed input and pastes mirror to the group |
| Vault: startup unlock, send secret | `PasswordVaultStartupUnlock`, `SendSecretMenu` | done | the unlock keeps the keyboard while it asks |
| SFTP: panel, docked browser and rail, SFTP window, editors, drop to upload | `AppShell.tsx`, `Sidebar.tsx`, `file-browser.ts` | done | `ui/sftp.rs`; file links in terminals open in the editor; unsaved files are asked about before tabs close |
| Session history, log viewer, session import, forwarding panel, update notice | `SessionHistoryDialog`, `LogViewerDialog`, import dialogs, `ForwardingPanel`, `UpdateNotification` | done | top bar, launcher, settings hooks |
| Tab menu, rename, overflow | `TabStrip.tsx` | done | every item of the client's menu (reconnect + tmux/screen, force reconnect, flags, multi-execution, logging, BREAK); rows scroll with arrows and the wheel |
| Session logging controls | `TopBar.tsx`, `session-actions.ts` | done | start/stop, log to file (save dialog with the backend's suggestion), show, pause, input capture |
| Open/move a tab in a new window | `tab-transfer.ts` | done | a move carries the live session over (checked end to end) |
| Interface zoom | `applyInterfaceZoom` | done | Settings → Appearance "Interface scale" (80–150%), applied live to every window. `muxus_ui::zoom`: `px` scales CSS sizes, the rem follows, `layout_px`/`unzoom` convert values already in layout space, `window_px` keeps window geometry unscaled. Like Electron there is no chord for it; Ctrl+Shift+=/−/0 zoom the terminal font |
| Secondary windows (settings, SFTP) | `BrowserWindow` frame | done | client decorations with the window controls on every window (`app_window_options`); the close button runs the window's close hooks (`on_close_request`) |
| Keyboard shortcuts | `keymap/*` | done | every command id of the client is handled |

## Performance

`hack/perf.sh`, release build, headless GNOME Shell with lavapipe (software
rendering, so frame times are pessimistic), one plain bash tab. The current
run is under "Final verification"; `terminal-perf.md` has the history.

- **Opening tabs.** The 300–400 ms "stall" reported before was a frame gap:
  after the tabs open nothing needs painting for a while (the hidden tabs
  dial 250 ms later). The harness now measures how long the UI thread is
  busy in one go (a 4 ms heartbeat's lateness, for two seconds after the
  tabs open). That was 59–70 ms: nine local shells dialled in one turn, each
  fork of the 200 MB process holding the UI thread up for 5–8 ms. Hidden
  tabs opened together now dial 20 ms apart: 35 ms.
- **Output updates.** A `seq` flood arrived as ~300-byte events and each
  became its own UI update (~35 k/s). Streaming output is now gathered over
  one or two event-loop turns (no timer: waiting backs the session up and
  slowed `seq` to 1.5 s in a trial): ~1 k updates/s.
- **Idle.** Rows are cached by the engine's content generation, so a blink
  rebuilds the cursor only. The status bar stops polling while the window is
  hidden.

## Final verification

- **`cargo xtask ci`** (fmt, clippy with `-D warnings`, tests, layering,
  style): green. Workspace tests: 2526 passed, 0 failed, 6 ignored.
- **Screenshots.** `hack/regress.py` captured all 160 scenes before and
  after the consolidation (TextField/Select on FormField, the shared Monaco
  theme, grey tokens, the merged helpers). 129 are pixel-identical; 28 stay
  under 0.25%: the command buttons dialog (0.20%, its floating labels and
  select value now sit where FormField puts them, about one device pixel
  higher), workspaces (0.15%), launcher-empty (0.10%), history, log viewer
  and management scenes (0.01–0.09%, clocks and timestamps). Three changed
  for the better: the dark management review dialog (its baseline caught an
  empty desktop) and the raw JSON editor, light and dark (see
  `management-ui.md`). `hack/app-shots.sh`:
  overview 1.82% / 1.71% against the docs screenshots (1.71% / 1.72%
  before); panes 6.08% / 5.97%, as the July reference predates the current
  demo catalog and the status bar; the unmaximized window edge check passes
  in both themes.
- **Performance** (`hack/perf.sh`, release, two runs): `seq 1 2000000`
  1.04–1.05 s, `cat` of 50 MB 1.24–1.34 s; worst p95 frame gap 20.6–26.8 ms;
  opening nine tabs blocks the UI thread for 15.5–24.9 ms at most; RSS
  193 MiB with one tab, 195 MiB with ten; CPU idle 0.0%, right after `seq`
  0.0%, ten idle tabs 0.0%.
  After the end-to-end pass (zoom, scrollback snapshots): `seq` 1.06–1.07 s,
  `cat` 1.41–1.74 s, worst p95 frame gap 20.6–26.5 ms, nine tabs block the
  UI thread for 27.1 ms at most, RSS 195/197 MiB; see `terminal-perf.md`.

## End-to-end pass (`hack/e2e`)

The real app, driven through a headless GNOME Shell, against servers in
Docker. Nothing touches the real data: the app runs with a scratch HOME (its
`~/.ssh/config` names the containers) and `MUXUS_DATA_DIR` in the work folder.

```bash
hack/e2e/run.py all --binary target-release/release/muxus   # up, every scenario, down
hack/e2e/run.py all --only tunnels_carry_traffic_and_show_state --keep
hack/e2e/run.py up --binary B --work DIR     # servers, desktop and app; stays up
hack/e2e/run.py ctl --work DIR shot out.png  # one desktop request: click X Y, key ctrl+t, type TEXT, windows, ...
hack/e2e/run.py test --work DIR [--only A,B] # scenarios against a running `up`
hack/e2e/run.py down --work DIR [--images]   # containers, scope, scratch folders
```

- `containers.sh`: images `muxusrs-e2e-{sshd,telnet,vnc,rdp}` on network
  `muxusrs-e2e-net`, ports on 127.0.0.1 only: ssh 22201 (key, password,
  keyboard-interactive and a user without forwarding; an HTTP server on its
  8080), a jump host on 22202 and an inner host reachable only through it,
  22203 left out of `known_hosts` for the host-key prompt, telnet 22323, VNC
  25900, RDP (xrdp) 23389.
- `desktop.py`: gnome-shell `--headless --wayland` in a
  `systemd-run --user --scope -p MemoryMax=6G -p MemorySwapMax=0`, input
  through Mutter's RemoteDesktop (keysyms), screenshots, window lists through
  `org.gnome.Shell.Introspect`, file choosers through the GTK portal. An
  `ibus-daemon` stub keeps the real one (which grew to gigabytes) out.
- A serial line: a socat pty pair with `serial_echo.py` as the far end.
- `scenarios.py`: logging records connections; the SFTP browser follows the
  terminal's folder; the SFTP window closes; password, key,
  keyboard-interactive, jump host and host-key sign-ins; telnet, serial and
  local terminals; VNC and RDP; SFTP upload, download, new folder, rename,
  delete and the editor; local, remote and SOCKS tunnels carrying HTTP with
  the top bar badge following them; a denied forward and a port in use
  showing their errors; tunnels from `~/.ssh/config`. All nine pass on the
  release build.

What the pass found and fixed: the SFTP list cancelled by every render's
folder sync; the tunnel badge only polled from the launcher and "Save &
start" lost (Close before Saved); the SFTP window without decorations on
GNOME; `main.log` without a subscriber; menus, selects and autocompletes in
dialogs drawn under them yet catching clicks; toasts under dialogs; tooltips
left over dialogs; an empty keyboard-interactive dialog after a PAM
password; silent file choosers without a portal (now a fallback path and a
toast); the editor taking the keyboard back on every render; scrollback not
kept across restarts; overlay actions lost when Close came first.

Known open: in debug builds the remote editor can drop keys typed very fast
(release is fine); a tooltip can stay when the toolbar shifts under a still
pointer; a tooltip at the window's right edge is clipped.

## Needs from other crates

- `muxus-term-view`: `TerminalView::cursor_anchor` (added here) places the
  command and send-secret menus at the caret; the search box takes its
  colors from the muxus-ui theme.
- `muxus-session`: the update check is off without `MUXUS_UPDATE_FEED`
  (`UPDATE_FEED_VAR`, `process_update_manifest_url`); the development folder
  seeds `client-state.json` too.
