# Parity: dialogs B (import, forwarding, logs, vault, notices)

Feature folders under `crates/muxus-app/src/features/`: `import`, `forwarding`,
`logs`, `vault`, `notices` (one `pub mod` line each in `features/mod.rs`).
Every read and write goes through `muxus_session::Backend`; the views use the
`muxus-ui` widget kit and theme tokens only.

```bash
CARGO_TARGET_DIR=target-ui-b cargo test -p muxus-app --lib -- \
  features::import features::forwarding features::logs features::vault features::notices   # 38 tests
CARGO_TARGET_DIR=target-ui-b cargo build -p muxus-app \
  --example import --example forwarding --example logs --example vault --example notices
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=review out.png -- target-ui-b/debug/examples/import
```

Examples (`MUXUS_THEME=dark` for the dark variants): `import` (`mobaxterm`,
`securecrt`, `review`), `forwarding` (`panel`, `menu`, `adhoc`; web-01 is an
in-process SSH server reached through a `ProxyCommand`, so its ssh_config
`LocalForward` really runs and the panel shows the demo host name), `logs`,
`vault` (`secret`, `send` with `MUXUS_AT=x,y`, `unlock`), `notices`
(`update`, `rename`). All start a sandboxed backend on the demo data of
`hack/demo-env.mjs` (`host_editor::demo`); nothing touches the real home.

## Logic (UI-free, tested)

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Applying an import (fresh existing hosts, keep/replace, ssh_config upsert + metadata patch, saved host put + metadata patch, host order) | data-transfer.ts `restoreImportedConnections` | done | `import::apply`; uses existing Backend methods (no `import_apply.rs` needed); tests import a MobaXterm file into Muxus storage and into ssh_config, then keep and replace on a second run |
| Import dialog model (vendor copy, size check, selection, search, conflicts, skipped paging, toasts) | SessionImportDialog.tsx | done (elsewhere) | `muxus_import::review` (import crate) |
| Forwarding panel rules (grouping, tunnel row state, status lines, tooltips, delete confirmation, connection text, bookmark into ssh_config, save as tunnel, switch toggles, ad-hoc request, connection reuse) | ForwardingPanel.tsx, api/tunnels.ts | done | `forwarding::model`, 6 tests |
| Tunnel start (reuse a live config connection, else dial with status/auth/host-key round trips, 404 fallback, dial lease released after the forward starts) | api/tunnels.ts `startTunnel`, api/dial.ts `dialConnection` | done | `forwarding::start`; tests against the in-process SSH server: dial with host key + password prompts and traffic through the forward, a dismissed prompt cancels, a live connection is ridden without a second dial |
| Log viewer (level filter ranks, text filter, `formatLogEntry`, export document, debug notice, follow-to-end rule) | LogViewerDialog.tsx, api/logs.ts | done | `logs::model`, 4 tests |
| Sending a vault secret (unlock retry loop, error codes, toasts, vault refresh) | vault-secret-send.ts, vault-secrets.test.ts "sending a secret" | done | loop is `command_buttons::run::send_secret_to_terminals`; `vault::send` adds the unlock prompt, toasts and `SecretSender`; 4 tests against a sandbox backend with a real vault (Credential policy), a real secret and a live local shell |
| Secret dialog rules (description, submit label, value sent only when changed, maxLength) | VaultSecretDialog.tsx | done | `vault::secret_dialog`; checks shared with `settings::view::passwords::secret_problem` |
| Send-secret menu rules (empty lines, keyboard focus moves) | SendSecretMenu.tsx | done | `vault::send_menu`, 2 tests |
| Startup unlock | PasswordVaultStartupUnlock.tsx, password-vault-startup.ts | done | rule is `settings::vault_startup` (tested there) |
| Update notices (in-app updater decisions, keys, persistence, Store, actions) | update-notice.ts, UpdateNotification.tsx | done | `notices::update_notice`; update-notice.test.ts ported + action table |
| Connection diagnostics text (marks, grouped labels, padding, sanitising, progress, settle line) | terminal/connection-diagnostics.ts, TerminalViewImpl.tsx `diagnoseFromTerminalInput` | done | `notices::diagnostics`; connection-diagnostics-format.test.ts ported + an end-to-end Telnet diagnosis against a closed loopback port |
| Tab strip overflow (tolerance, arrows, scroll step, wheel, reveal) and rename rule | TabStrip.tsx | done | `notices::tab_strip`, 4 tests |
| Session logging policy UI bits | session-logging-policy.ts | n/a | drafts ported by the settings (`settings::draft`) and host editor (`logic::session_logging`) features, which draw the fields |
| Tab transfer UI parts | tab-transfer.test.ts | n/a | drag payload and drop handling belong to the window's tab strip (app core) |
| Vault auth prompts (`vault-unlock`, `vault-create`, `vault-repair`) | AuthPromptDialog.tsx | done (elsewhere) | `ui/prompts.rs` already handles all three (create checks, "Create vault") |

## Views

| Screen | TypeScript source | Status | Note |
|---|---|---|---|
| Session import dialog (source cards, local MobaXterm card on Windows, file picker, review with skipped list + pager, storage and conflict radios, filter, select all, rows with kind/folder chips and "Already exists", busy spinners, error alert, success/error toasts) | SessionImportDialog.tsx + vendor dialogs | done | `import::dialog::SessionImportDialog`; reading and parsing run on the backend runtime |
| Forwarding panel (tunnel rows, autostart/reconnect marks, spinner, stop/start/dismiss, hover options menu with switches/edit/delete, connections with session forwards, config chip, save as tunnel, save to ssh config, ad-hoc "Forward on …" dialog with the rule form and diagram, tunnel editor, delete confirmation) | ForwardingPanel.tsx, ForwardRuleForm.tsx | done | `forwarding::panel::ForwardingPanel`; live forwards via `subscribe_forwards`, 5 s polls as the queries; tunnel editor is the host editor's `TunnelEditor` |
| Log viewer | LogViewerDialog.tsx | done | `logs::view::LogViewerDialog` |
| Secret dialog (stand-alone) | VaultSecretDialog.tsx | done | `vault::secret_dialog::VaultSecretDialog` |
| Send-secret menu | SendSecretMenu.tsx | done | `vault::send_menu::SendSecretMenu` |
| Startup unlock dialog | PasswordVaultStartupUnlock.tsx | done | `vault::startup_unlock::VaultStartupUnlock` |
| Update snackbar (release and in-app updater variants, mute link) | UpdateNotification.tsx | done | `notices::update_view::UpdateNotification`; desktop actions through `SettingsHooks::desktop_update` (Store install uses its `download` hook) |
| Rename tab dialog, overflow arrows | TabStrip.tsx | done | `notices::tab_strip` |
| Connection diagnosis in the terminal | TerminalViewImpl.tsx | done (text) | `notices::diagnostics::diagnose_connection`; writing it into the terminal is the window's |
| Empty pane variants | EmptyPane.tsx | n/a | the shell draws it; its host picker is the launcher feature's |

## Screenshots

Device pixels differing by more than 24/255, cropped to the element the
reference measured (dialog paper, menu, snackbar; the panel at
`1040,52,380,828`). References: `tunnels.png` for the panel layout and fresh
captures of the current client (headless Chrome 1420×880 at 2×, the
`hack/demo-env.mjs` sandbox on private ports, fixed API answers for logs,
vault and update check) for everything else.

| Screen | Reference | Light | Dark |
|---|---|---|---|
| Forwarding panel (`panel`) | `tunnels.png` layout, fresh capture | 2.48% | 2.55% |
| Tunnel options menu (`menu`) | fresh capture | 3.94% | 4.50% |
| "Forward on web-01" (`adhoc`) | fresh capture | 3.36% | 3.22% |
| Import, MobaXterm source (`mobaxterm`) | fresh capture | 2.08% | 1.96% |
| Import, SecureCRT source (`securecrt`) | fresh capture | 2.64% | 2.56% |
| Import review (`review`) | fresh capture | 3.96% | 3.84% |
| Log viewer | fresh capture | 2.30% | 1.66% |
| Add secret (`secret`) | fresh capture (settings → Passwords → Add secret) | 2.85% | 2.69% |
| Send secret menu (`send`) | fresh capture (Ctrl+Shift+P in a local tab) | 3.25% | 3.57% |
| Startup unlock (`unlock`) | fresh capture | 3.58% | 3.30% |
| Update snackbar (`update`) | fresh capture | 9.41% | 3.48% |
| Rename tab (`rename`) | fresh capture | 1.57% | 1.51% |

The light snackbar's share is high because white text antialiased on the
saturated blue fill differs on almost every edge pixel; its geometry matches
to the pixel. The references came from a variant of `../muxus/hack/capture.mjs` that
answers `/api/logs`, `/api/password-vault` and `/api/app/update-check` with
fixed data (the same data the examples use); it is not checked in. Shots were
taken with `--step key:42` so the window is active, as the browser's is.

Main differences: glyph rasterization (swash vs FreeType, ±1 device px);
JetBrains Mono advances come out about 1% wider than Chrome's at 11px, so a
long forward rule ellipsizes a few characters earlier; MUI's skipped-session
grid is approximated by measuring the widest reason; the dialogs sit on a
plain background instead of the dimmed app (crops exclude it).

## Hook points for the window

- `import::dialog::open_session_import(backend, vendor, window, cx)` →
  `Entity<SessionImportDialog>`; events `Close`, `Imported(RestoreResult)`
  (refresh the host catalog). Register as `SettingsHooks::import_sessions`.
- `forwarding::panel::forwarding_panel(backend, cx)` → `Entity<ForwardingPanel>`,
  mounted right of the canvas while forwarding is toggled; events `Close` and
  `Prompt { prompt: DialPrompt, reply }` (show the auth / host-key dialog of
  `ui/prompts.rs`, send `DialAnswer`; dropping `reply` cancels). `refresh()`
  after outside changes.
- `logs::view::open_log_viewer(backend, window, cx)`; event `Close`. Register
  as `SettingsHooks::open_log_viewer`.
- `vault::send::secret_sender(backend, terminal_ids_of, cx)` +
  `install_secret_sender(sender, cx)` makes it `CommandHooks::send_secret`;
  render the sender entity in the window (it draws the unlock prompt); event
  `VaultChanged`.
- `vault::send_menu::open_send_secret_menu(backend, tab, connected, position,
  window, cx)` on `terminal.send-secret` (position from
  `CommandHooks::cursor_anchor` or `menu_center`); events `Close`,
  `ManageSecrets` (open settings at Passwords).
- `vault::secret_dialog::open_vault_secret_dialog(backend, secret, name,
  window, cx)` for `VaultSecretField`; events `Close`, `Saved`.
- `vault::startup_unlock::vault_startup_unlock(backend, window, cx)` at window
  start; hold the workspace restore until `Ready`.
- `notices::update_view::update_notification(backend, notify_pref, cx)`,
  mounted in a full-window relative layer; `set_notify_on_new_version` on prefs
  changes; event `Muted`.
- `notices::diagnostics::{diagnosis_started, diagnose_connection}` for the D key
  of an ended SSH/Telnet tab (replace `ui/terminal.rs`'s "backend not connected"
  placeholder).
- `notices::tab_strip::{TabScrollState::of, earlier_tabs_button,
  later_tabs_button, reveal_offset, scroll_row_by, scroll_step, wheel_delta,
  open_rename_tab_dialog}` for the tab strip (events `Rename { tab_id, title }`,
  `Close`).

## Added to muxus-ui

New widgets: `progress::CircularProgress`, `layer::{window_layer,
modal_layer}`, `form_control::{FormControlLabel, form_label}`,
`toggle_button::{ToggleButton, ToggleButtonGroup}`, `link::Link`,
`text::measure_text`. Options on existing ones: `Alert::{content, compact}` +
`display::alert_ink`; `Chip::{palette, mono, width, height, font_size}` and
MUI's small-icon geometry; `TextField::{floating_label, placeholder, start,
end, width}`; `Select::{label, width, track_bounds}`; `Label::{variant,
strut}`, `Paragraph::variant`; `dialog::dialog_content`,
`DialogWidth::px` public; dense `MenuItem` 32px minimum; theme tokens
`theme::weight::*`, `theme::sizes::*`. Others extended several of these
since (`FormControlLabel` switch/caption, `ToggleButton::content`,
`progress::linear_progress_indeterminate`).

## Attribution

No third-party code was adapted. `CircularProgress` moved the file
browser's spinner (Muxus's own code) into the kit.

## Integration pass

All of these are wired into the window (`app.md`). `notices/diagnostics.rs` was merged into
`session/diagnostics.rs` (one implementation, both test sets). The rename dialog, tab overflow
arrows and wheel scrolling are in the tab strip.
